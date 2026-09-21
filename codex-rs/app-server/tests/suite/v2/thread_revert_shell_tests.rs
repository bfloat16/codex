//! Cancellation while a shell is running or its approval is pending must leave no tool history.

use super::*;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::TerminalInteractionNotification;
use codex_app_server_protocol::ThreadItem;
use pretty_assertions::assert_eq;

#[derive(Clone, Copy)]
enum ShellState {
    Running,
    Interactive,
    Approval,
}

#[test_case::test_case(ShellState::Running, Stop::Interrupt; "interrupted_shell")]
#[test_case::test_case(ShellState::Running, Stop::Revert; "running_shell")]
#[test_case::test_case(ShellState::Interactive, Stop::Revert; "interactive_shell")]
#[test_case::test_case(ShellState::Approval, Stop::Revert; "pending_approval")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rewind_shell_then_rewind_again_matches_cold_context(
    state: ShellState,
    stop: Stop,
) -> Result<()> {
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    let config = MockResponsesConfig::new(&server.uri()).enable_feature(Feature::UnifiedExec);
    let config = match state {
        ShellState::Running | ShellState::Interactive => {
            config.with_sandbox_mode("danger-full-access")
        }
        ShellState::Approval => config.with_approval_policy("on-request"),
    };
    config.write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    initialize_experimental(&mut mcp).await?;
    let info = mcp.auto_env()?.environment().info().await?;
    let command = match info.shell.name.as_str() {
        "powershell" => "Write-Output RW_SHELL_STARTED; Start-Sleep -Seconds 60",
        "cmd" => "echo RW_SHELL_STARTED & ping -n 61 127.0.0.1 >nul",
        _ => "echo RW_SHELL_STARTED; sleep 60",
    };
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..Default::default()
        })
        .await?;
    responses::mount_sse_once(
        &server,
        create_final_assistant_message_sse_response("RW_FIRST_REPLY")?,
    )
    .await;
    let first = send_prompt(&mut mcp, &thread.id, "RW_FIRST").await?;
    server.reset().await;
    let mut args = serde_json::json!({"cmd": command, "login": false});
    if matches!(state, ShellState::Interactive) {
        args["tty"] = serde_json::json!(true);
    }
    if matches!(state, ShellState::Approval) {
        args["sandbox_permissions"] = serde_json::json!("require_escalated");
        args["justification"] = serde_json::json!("Run the test command?");
    }
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("shell"),
            responses::ev_exec_command_call_with_args("RW_SHELL_CALL", &args),
            responses::ev_completed("shell"),
        ]),
    )
    .await;
    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: prompt_input("RW_SHELL_PROMPT"),
                ..Default::default()
            },
        })
        .await?;
    match state {
        ShellState::Interactive => {
            timeout(DEFAULT_READ_TIMEOUT, async {
                loop {
                    let started: ItemStartedNotification = mcp.read_notification("item/started").await?;
                    if matches!(started.item, ThreadItem::CommandExecution { ref id, .. } if id == "RW_SHELL_CALL") {
                        return anyhow::Ok(());
                    }
                }
            }).await??;
        }
        ShellState::Running => {
            let started: TerminalInteractionNotification = timeout(
                DEFAULT_READ_TIMEOUT,
                mcp.read_notification("item/commandExecution/terminalInteraction"),
            )
            .await??;
            assert_eq!(started.item_id, "RW_SHELL_CALL");
        }
        ShellState::Approval => {
            let request = timeout(
                DEFAULT_READ_TIMEOUT,
                mcp.read_stream_until_request_message(),
            )
            .await??;
            let ServerRequest::CommandExecutionRequestApproval { params, .. } = request else {
                anyhow::bail!("expected command approval");
            };
            assert_eq!(params.item_id, "RW_SHELL_CALL");
        }
    }
    if matches!(stop, Stop::Interrupt) {
        mcp.interrupt_turn_and_wait_for_aborted(
            thread.id.clone(),
            turn.id.clone(),
            DEFAULT_READ_TIMEOUT,
        )
        .await?;
    }
    revert(&mut mcp, &thread.id, &turn.id).await?;
    let persisted = std::fs::read_to_string(thread.path.as_ref().expect("rollout path"))?;
    assert!(!persisted.contains("RW_SHELL"));
    let mut inputs = Vec::new();
    for cold in [false, true] {
        if cold {
            restart(&mut mcp, &home, &thread.id).await?;
        }
        server.reset().await;
        let mock = responses::mount_sse_once(
            &server,
            create_final_assistant_message_sse_response("RW_PROBE_REPLY")?,
        )
        .await;
        let probe = send_prompt(&mut mcp, &thread.id, "RW_PROBE").await?;
        let input = mock.single_request().input();
        let serialized = serde_json::to_string(&input)?;
        assert!(serialized.contains("RW_FIRST_REPLY"));
        assert!(!serialized.contains("RW_SHELL"));
        assert!(!serialized.contains("<turn_aborted>"));
        assert!(input.iter().all(|item| item["type"] == "message"));
        inputs.push(conversation_items(input));
        revert(&mut mcp, &thread.id, &probe).await?;
    }
    assert_eq!(inputs[0], inputs[1]);
    revert(&mut mcp, &thread.id, &first).await?;
    assert_eq!(
        turn_ids_from_cursor(
            &mut mcp, &thread.id, /*cursor*/ None, /*sort_direction*/ None
        )
        .await?,
        Vec::<String>::new()
    );
    Ok(())
}
