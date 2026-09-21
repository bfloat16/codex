//! Rewind must reconstruct the same conversation in memory and after a process restart.

use super::*;
use codex_app_server_protocol::CompactionMode;
use codex_app_server_protocol::ThreadCompactStartParams;
use codex_app_server_protocol::ThreadCompactStartResponse;
use pretty_assertions::assert_eq;

#[derive(Clone, Copy)]
enum Stop {
    Interrupt,
    Revert,
}

#[test_case::test_case(Stop::Interrupt, CompactionMode::Local; "local_already_interrupted")]
#[test_case::test_case(Stop::Revert, CompactionMode::Local; "local_still_waiting_for_tool")]
#[test_case::test_case(Stop::Interrupt, CompactionMode::RemoteV1; "remote_already_interrupted")]
#[test_case::test_case(Stop::Revert, CompactionMode::RemoteV1; "remote_still_waiting_for_tool")]
#[tokio::test]
async fn repeated_revert_across_compactions_matches_cold_context(
    stop: Stop,
    mode: CompactionMode,
) -> Result<()> {
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    MockResponsesConfig::new(&server.uri())
        .with_root_config("model_auto_compact_token_limit = 1000000")
        .write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    initialize_experimental(&mut mcp).await?;
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..Default::default()
        })
        .await?;

    let mut turns = Vec::new();
    for (prompt, reply, summary) in [
        ("RW_FIRST", "RW_REPLY_FIRST", "RW_SUMMARY_ONE"),
        ("RW_SECOND", "RW_REPLY_SECOND", "RW_SUMMARY_TWO"),
    ] {
        server.reset().await;
        responses::mount_sse_once(&server, create_final_assistant_message_sse_response(reply)?)
            .await;
        turns.push(send_prompt(&mut mcp, &thread.id, prompt).await?);
        server.reset().await;
        if mode == CompactionMode::Local {
            responses::mount_sse_once(
                &server,
                create_final_assistant_message_sse_response(summary)?,
            )
            .await;
        } else {
            let mut output: Vec<Value> = ["RW_FIRST", "RW_SECOND"].iter().take(turns.len())
                .map(|text| serde_json::json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}))
                .collect();
            output.push(serde_json::json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": summary}]}));
            output.push(serde_json::json!({"type": "compaction", "encrypted_content": summary}));
            responses::mount_compact_json_once(&server, serde_json::json!({"output": output}))
                .await;
        }
        mcp.clear_message_buffer();
        let compact = mcp
            .send_thread_compact_start_request(ThreadCompactStartParams {
                thread_id: thread.id.clone(),
                mode: Some(mode),
            })
            .await?;
        let _: ThreadCompactStartResponse = mcp.read_response(compact).await?;
        let completed: TurnCompletedNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_notification("turn/completed"),
        )
        .await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
    }

    server.reset().await;
    responses::mount_sse_once(
        &server,
        create_request_user_input_sse_response("RW_PENDING_CALL")?,
    )
    .await;
    let TurnStartResponse { turn: pending } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: prompt_input("RW_INTERRUPTED"),
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Plan,
                    settings: Settings {
                        model: "mock-model".to_string(),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    if matches!(stop, Stop::Interrupt) {
        mcp.interrupt_turn_and_wait_for_aborted(
            thread.id.clone(),
            pending.id.clone(),
            DEFAULT_READ_TIMEOUT,
        )
        .await?;
    }

    // First remove the interrupted turn, then cross each compaction back to an empty thread.
    // Undoing each probe also exercises appending after rewind and reusing durable ordinals.
    for (target, expected, removed) in [
        (
            pending.id,
            vec!["RW_FIRST", "RW_SECOND", "RW_SUMMARY_TWO"],
            vec!["RW_INTERRUPTED", "RW_PENDING_CALL", "RW_SUMMARY_ONE"],
        ),
        (
            turns[1].clone(),
            vec!["RW_FIRST", "RW_SUMMARY_ONE"],
            vec!["RW_SECOND", "RW_SUMMARY_TWO", "RW_INTERRUPTED"],
        ),
        (
            turns[0].clone(),
            vec![],
            vec!["RW_FIRST", "RW_SECOND", "RW_SUMMARY_ONE", "RW_SUMMARY_TWO"],
        ),
    ] {
        revert(&mut mcp, &thread.id, &target).await?;
        let persisted = std::fs::read_to_string(thread.path.as_ref().expect("rollout path"))?;
        for marker in ["RW_FIRST", "RW_SECOND", "RW_INTERRUPTED", "RW_PENDING_CALL"] {
            if !expected.contains(&marker) {
                assert!(
                    !persisted.contains(marker),
                    "persisted history retained {marker}"
                );
            }
        }
        server.reset().await;
        let warm = responses::mount_sse_once(
            &server,
            create_final_assistant_message_sse_response("RW_PROBE_REPLY")?,
        )
        .await;
        let probe = send_prompt(&mut mcp, &thread.id, "RW_PROBE").await?;
        let warm_input = warm.single_request().input();
        let serialized = serde_json::to_string(&warm_input)?;
        for marker in &expected {
            assert!(serialized.contains(marker), "memory history lost {marker}");
        }
        for marker in &removed {
            assert!(
                !serialized.contains(marker),
                "memory history retained {marker}"
            );
        }
        assert!(
            warm_input
                .iter()
                .all(|item| item["type"] == "message" || item["type"] == "compaction")
        );

        revert(&mut mcp, &thread.id, &probe).await?;
        restart(&mut mcp, &home, &thread.id).await?;
        server.reset().await;
        let cold = responses::mount_sse_once(
            &server,
            create_final_assistant_message_sse_response("RW_PROBE_REPLY")?,
        )
        .await;
        let probe = send_prompt(&mut mcp, &thread.id, "RW_PROBE").await?;
        assert_eq!(
            conversation_items(cold.single_request().input()),
            conversation_items(warm_input)
        );
        revert(&mut mcp, &thread.id, &probe).await?;
    }
    assert_eq!(
        turn_ids_from_cursor(
            &mut mcp, &thread.id, /*cursor*/ None, /*sort_direction*/ None
        )
        .await?,
        Vec::<String>::new()
    );
    Ok(())
}

#[test_case::test_case(Stop::Interrupt; "interrupted_compaction")]
#[test_case::test_case(Stop::Revert; "active_compaction")]
#[tokio::test]
async fn revert_during_compaction_keeps_preceding_context(stop: Stop) -> Result<()> {
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    MockResponsesConfig::new(&server.uri()).write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    initialize_experimental(&mut mcp).await?;
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..Default::default()
        })
        .await?;
    let mut turns = Vec::new();
    for prompt in ["RW_FIRST", "RW_SECOND"] {
        server.reset().await;
        responses::mount_sse_once(
            &server,
            create_final_assistant_message_sse_response("RW_REPLY")?,
        )
        .await;
        turns.push(send_prompt(&mut mcp, &thread.id, prompt).await?);
    }
    server.reset().await;
    let pending = responses::mount_response_sequence(
        &server,
        vec![
            responses::sse_response(create_final_assistant_message_sse_response(
                "RW_STALE_SUMMARY",
            )?)
            .set_delay(std::time::Duration::from_secs(60)),
        ],
    )
    .await;
    mcp.clear_message_buffer();
    let compact = mcp
        .send_thread_compact_start_request(ThreadCompactStartParams {
            thread_id: thread.id.clone(),
            mode: Some(CompactionMode::Local),
        })
        .await?;
    let _: ThreadCompactStartResponse = mcp.read_response(compact).await?;
    timeout(DEFAULT_READ_TIMEOUT, async {
        while pending.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    if matches!(stop, Stop::Interrupt) {
        let started: codex_app_server_protocol::TurnStartedNotification =
            mcp.read_notification("turn/started").await?;
        mcp.interrupt_turn_and_wait_for_aborted(
            thread.id.clone(),
            started.turn.id,
            DEFAULT_READ_TIMEOUT,
        )
        .await?;
    }
    revert(&mut mcp, &thread.id, &turns[1]).await?;
    restart(&mut mcp, &home, &thread.id).await?;
    server.reset().await;
    let followup = responses::mount_sse_once(
        &server,
        create_final_assistant_message_sse_response("RW_CONTINUED")?,
    )
    .await;
    send_prompt(&mut mcp, &thread.id, "RW_PROBE").await?;
    let input = serde_json::to_string(&followup.single_request().input())?;
    assert!(input.contains("RW_FIRST"));
    assert!(!input.contains("RW_SECOND"));
    assert!(!input.contains("RW_STALE_SUMMARY"));
    revert(&mut mcp, &thread.id, &turns[0]).await?;
    assert_eq!(
        turn_ids_from_cursor(
            &mut mcp, &thread.id, /*cursor*/ None, /*sort_direction*/ None
        )
        .await?,
        Vec::<String>::new()
    );
    Ok(())
}

fn prompt_input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

fn conversation_items(input: Vec<Value>) -> Vec<Value> {
    // Probe turns get fresh message IDs. Settings/environment fragments can also legitimately
    // differ: a live revert preserves the current Plan mode, while cold resume uses saved settings.
    input
        .into_iter()
        .filter(|item| {
            item["type"] != "message"
                || item["role"] == "assistant"
                || item["content"].as_array().is_some_and(|content| {
                    content.iter().any(|part| {
                        part["text"].as_str().is_some_and(|text| {
                            text.contains("RW_") || text.contains("<turn_aborted>")
                        })
                    })
                })
        })
        .map(|mut item| {
            item.as_object_mut().expect("input item").remove("id");
            item
        })
        .collect()
}

async fn send_prompt(mcp: &mut TestAppServer, thread_id: &str, text: &str) -> Result<String> {
    let completed = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread_id.to_string(),
            input: prompt_input(text),
            ..Default::default()
        }),
    )
    .await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    Ok(completed.turn.id)
}

async fn revert(mcp: &mut TestAppServer, thread_id: &str, target: &str) -> Result<()> {
    let response: ThreadRevertResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.request(|request_id| ClientRequest::ThreadRevert {
            request_id,
            params: ThreadRevertParams {
                thread_id: thread_id.to_string(),
                before_turn_id: target.to_string(),
            },
        }),
    )
    .await??;
    assert_eq!(response.thread.id, thread_id);
    Ok(())
}

async fn restart(mcp: &mut TestAppServer, home: &TempDir, thread_id: &str) -> Result<()> {
    timeout(DEFAULT_READ_TIMEOUT, mcp.shutdown_gracefully()).await??;
    *mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    initialize_experimental(mcp).await?;
    let _: ThreadResumeResponse = mcp
        .request(|request_id| ClientRequest::ThreadResume {
            request_id,
            params: ThreadResumeParams {
                thread_id: thread_id.to_string(),
                exclude_turns: true,
                ..Default::default()
            },
        })
        .await?;
    Ok(())
}
