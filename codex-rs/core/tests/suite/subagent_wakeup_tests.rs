use anyhow::Result;
use codex_core::config::Config;
use codex_features::Feature;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadHistoryMode;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;

fn request_json(request: &wiremock::Request) -> Value {
    let body = if request
        .headers
        .get("content-encoding")
        .and_then(|encoding| encoding.to_str().ok())
        == Some("zstd")
    {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).expect("decode request")
    } else {
        request.body.clone()
    };
    serde_json::from_slice(&body).expect("request JSON")
}

fn addressed_to(request: &wiremock::Request, recipient: &str) -> bool {
    request_json(request)["input"]
        .as_array()
        .expect("request input")
        .iter()
        .any(|item| item["type"] == "agent_message" && item["recipient"] == recipient)
}

fn configure_v2(config: &mut Config) {
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("enable v2");
    config
        .features
        .disable(Feature::ToolSearch)
        .expect("disable deferred tools");
    config.agent_default_subagent_model = None;
    config.model_provider.supports_websockets = false;
    config.model_provider.stream_max_retries = Some(0);
    config.model_provider.request_max_retries = Some(0);
}

#[derive(Clone, Copy)]
enum ChildOutcome {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy)]
enum WorkerOutcome {
    Completed,
    Failed,
}

#[test_case(ChildOutcome::Completed, ThreadHistoryMode::Legacy, 0; "all results wake idle parent")]
#[test_case(ChildOutcome::Completed, ThreadHistoryMode::Paginated, 0; "paginated history wakes idle parent")]
#[test_case(ChildOutcome::Completed, ThreadHistoryMode::Legacy, 2000; "results arriving before parent yields")]
#[test_case(ChildOutcome::Failed, ThreadHistoryMode::Legacy, 0; "failed child ends batch")]
#[test_case(ChildOutcome::Interrupted, ThreadHistoryMode::Legacy, 0; "interrupted child ends batch")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_resumes_parent_once_after_all_child_tasks_end(
    outcome: ChildOutcome,
    history_mode: ThreadHistoryMode,
    parent_delay_ms: u64,
) -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex()
        .with_model("gpt-6.1-sol")
        .with_model_info_override("gpt-6.1-sol", |model| {
            model.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_config(configure_v2)
        .with_history_mode(history_mode)
        .build_with_auto_env(&server)
        .await?;
    let mut children = test.thread_manager.subscribe_thread_created();
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            let input = request_json(request)["input"]
                .as_array()
                .expect("input")
                .clone();
            !input.iter().any(|item| {
                item["type"] == "agent_message" || item["type"] == "function_call_output"
            })
        },
        sse(vec![
            ev_response_created("dispatch"),
            ev_function_call_with_namespace(
                "spawn-fast",
                "collaboration",
                "spawn_agent",
                &json!({"task_name":"fast", "message":"fast task"}).to_string(),
            ),
            ev_function_call_with_namespace(
                "spawn-slow",
                "collaboration",
                "spawn_agent",
                &json!({"task_name":"slow", "message":"slow task"}).to_string(),
            ),
            ev_completed("dispatch"),
        ]),
    )
    .await;
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            request_json(request)["input"]
                .as_array()
                .expect("input")
                .iter()
                .any(|item| {
                    item["type"] == "function_call_output" && item["call_id"] == "spawn-slow"
                })
                && !addressed_to(request, "/root")
        },
        sse_response(sse(vec![
            ev_response_created("yield"),
            ev_assistant_message("yield-message", "Waiting for the batch."),
            ev_completed("yield"),
        ]))
        .set_delay(Duration::from_millis(parent_delay_ms)),
    )
    .await;
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| addressed_to(request, "/root/fast"),
        sse_response(sse(vec![
            ev_response_created("fast"),
            ev_assistant_message("fast-message", "fast result"),
            ev_completed("fast"),
        ]))
        .set_delay(Duration::from_millis(/*millis*/ 300)),
    )
    .await;
    let slow_events = match outcome {
        ChildOutcome::Completed | ChildOutcome::Interrupted => vec![
            ev_response_created("slow"),
            ev_assistant_message("slow-message", "slow result"),
            ev_completed("slow"),
        ],
        ChildOutcome::Failed => vec![ev_response_created("slow")],
    };
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| addressed_to(request, "/root/slow"),
        sse_response(sse(slow_events)).set_delay(Duration::from_secs(/*secs*/ 1)),
    )
    .await;
    let resumed = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| addressed_to(request, "/root"),
        sse(vec![
            ev_response_created("resumed"),
            ev_assistant_message("resumed-message", "Integrated both results."),
            ev_completed("resumed"),
        ]),
    )
    .await;

    test.submit_turn("dispatch both tasks and yield").await?;
    let mut fast = None;
    let mut slow = None;
    for _ in 0..2 {
        let child = test
            .thread_manager
            .get_thread(children.recv().await?)
            .await?;
        match child
            .config_snapshot()
            .await
            .session_source
            .get_agent_path()
            .expect("child path")
            .name()
        {
            "fast" => fast = Some(child),
            "slow" => slow = Some(child),
            name => panic!("unexpected child {name}"),
        }
    }
    wait_for_event(&fast.expect("fast child"), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    if parent_delay_ms == 0 {
        assert_eq!(
            resumed.requests().len(),
            0,
            "first child must not wake the parent"
        );
    }
    let slow = slow.expect("slow child");
    if matches!(outcome, ChildOutcome::Interrupted) {
        slow.submit(Op::Interrupt).await?;
        wait_for_event(&slow, |event| matches!(event, EventMsg::TurnAborted(_))).await;
    }
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = resumed.single_request();
    let mut results = request.inputs_of_type("agent_message");
    results.sort_by(|left, right| left["author"].as_str().cmp(&right["author"].as_str()));
    assert_eq!(
        results
            .iter()
            .map(|item| item["author"].as_str().expect("author"))
            .collect::<Vec<_>>(),
        vec!["/root/fast", "/root/slow"]
    );
    assert!(
        results[0]["content"][0]["text"]
            .as_str()
            .expect("result")
            .contains("fast result")
    );
    let expected = match outcome {
        ChildOutcome::Completed => "slow result",
        ChildOutcome::Failed => "Agent errored:",
        ChildOutcome::Interrupted => "Agent interrupted.",
    };
    assert!(
        results[1]["content"][0]["text"]
            .as_str()
            .expect("result")
            .contains(expected)
    );
    assert!(
        request
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains("end your current turn immediately"))
    );

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            let body = request_json(request);
            body.to_string().contains("reuse the slow agent")
                && !body["input"]
                    .as_array()
                    .expect("input")
                    .iter()
                    .any(|item| item["call_id"] == "reuse-slow")
        },
        sse(vec![
            ev_response_created("reuse"),
            ev_function_call_with_namespace(
                "reuse-slow",
                "collaboration",
                "followup_task",
                &json!({"target":"slow", "message":"reuse task"}).to_string(),
            ),
            ev_completed("reuse"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| addressed_to(request, "/root/slow"),
        sse(vec![
            ev_response_created("reused-child"),
            ev_assistant_message("reused-message", "reuse result"),
            ev_completed("reused-child"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            let body = request_json(request);
            body["input"].as_array().expect("input").iter().any(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "reuse-slow"
            }) && !body.to_string().contains("reuse result")
        },
        sse(vec![
            ev_response_created("reuse-yield"),
            ev_assistant_message("reuse-yield-message", "Waiting again."),
            ev_completed("reuse-yield"),
        ]),
    )
    .await;
    let reused = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            addressed_to(request, "/root")
                && request_json(request).to_string().contains("reuse result")
        },
        sse(vec![
            ev_response_created("reuse-integrated"),
            ev_completed("reuse-integrated"),
        ]),
    )
    .await;
    test.submit_turn("reuse the slow agent").await?;
    if reused.requests().is_empty() {
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
    }
    assert!(
        reused
            .single_request()
            .inputs_of_type("agent_message")
            .iter()
            .any(|item| item["content"][0]["text"]
                .as_str()
                .expect("result")
                .contains("reuse result"))
    );
    Ok(())
}

#[test_case(0, WorkerOutcome::Completed; "fast nested completion")]
#[test_case(300, WorkerOutcome::Completed; "nested parents yield before completion")]
#[test_case(300, WorkerOutcome::Failed; "failed nested parent recovers before root")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_nested_batches_resume_each_parent_before_root(
    leaf_delay_ms: u64,
    worker_outcome: WorkerOutcome,
) -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex()
        .with_model("gpt-6.1-sol")
        .with_model_info_override("gpt-6.1-sol", |model| {
            model.multi_agent_version = Some(MultiAgentVersion::V2)
        })
        .with_config(configure_v2)
        .build_with_auto_env(&server)
        .await?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            !request_json(request)["input"]
                .as_array()
                .expect("input")
                .iter()
                .any(|item| {
                    item["type"] == "agent_message" || item["type"] == "function_call_output"
                })
        },
        sse(vec![
            ev_response_created("root-dispatch"),
            ev_function_call_with_namespace(
                "spawn-worker",
                "collaboration",
                "spawn_agent",
                &json!({"task_name":"worker", "message":"delegate the nested task"}).to_string(),
            ),
            ev_completed("root-dispatch"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| addressed_to(request, "/root/worker"),
        sse(vec![
            ev_response_created("nested-dispatch"),
            ev_function_call_with_namespace(
                "spawn-leaf",
                "collaboration",
                "spawn_agent",
                &json!({"task_name":"leaf", "message":"leaf task"}).to_string(),
            ),
            ev_completed("nested-dispatch"),
        ]),
    )
    .await;
    for (call_id, response_id) in [
        ("spawn-worker", "root-yield"),
        ("spawn-leaf", "worker-yield"),
    ] {
        let events = if call_id == "spawn-leaf" && matches!(worker_outcome, WorkerOutcome::Failed) {
            vec![ev_response_created(response_id)]
        } else {
            vec![
                ev_response_created(response_id),
                ev_assistant_message(response_id, "Waiting for child."),
                ev_completed(response_id),
            ]
        };
        mount_sse_once_match(
            &server,
            move |request: &wiremock::Request| {
                request_json(request)["input"]
                    .as_array()
                    .expect("input")
                    .iter()
                    .any(|item| {
                        item["type"] == "function_call_output" && item["call_id"] == call_id
                    })
            },
            sse(events),
        )
        .await;
    }
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| addressed_to(request, "/root/worker/leaf"),
        sse_response(sse(vec![
            ev_response_created("leaf"),
            ev_assistant_message("leaf-message", "leaf result"),
            ev_completed("leaf"),
        ]))
        .set_delay(Duration::from_millis(leaf_delay_ms)),
    )
    .await;
    let worker_resumed = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_json(request)["input"]
                .as_array()
                .expect("input")
                .iter()
                .any(|item| {
                    item["type"] == "agent_message" && item["author"] == "/root/worker/leaf"
                })
        },
        sse(vec![
            ev_response_created("worker-resumed"),
            ev_assistant_message("worker-result", "integrated leaf result"),
            ev_completed("worker-resumed"),
        ]),
    )
    .await;
    let root_resumed = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| addressed_to(request, "/root"),
        sse(vec![
            ev_response_created("root-resumed"),
            ev_completed("root-resumed"),
        ]),
    )
    .await;
    test.submit_turn("delegate a nested task and yield").await?;
    if root_resumed.requests().is_empty() {
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
    }
    assert_eq!(
        worker_resumed
            .single_request()
            .inputs_of_type("agent_message")
            .len(),
        2
    );
    let results = root_resumed
        .single_request()
        .inputs_of_type("agent_message");
    assert_eq!(
        results.len(),
        match worker_outcome {
            WorkerOutcome::Completed => 1,
            WorkerOutcome::Failed => 2,
        }
    );
    assert!(
        results.last().expect("worker result")["content"][0]["text"]
            .as_str()
            .expect("result")
            .contains("integrated leaf result")
    );
    Ok(())
}
