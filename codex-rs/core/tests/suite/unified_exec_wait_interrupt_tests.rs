//! Foreground waiting, interactive yielding, and session-wide shell cancellation.
use super::*;
use core_test_support::responses::mount_sse_once;
use pretty_assertions::assert_eq;

async fn shell_commands(test: &TestCodex) -> Result<(&'static str, &'static str, &'static str)> {
    let info = test.executor_environment().environment().info().await?;
    Ok(match info.shell.name.as_str() {
        "powershell" => (
            "Start-Sleep -Seconds 12; Write-Output FINISHED",
            "Start-Sleep -Seconds 300",
            "Write-Output RESUMED",
        ),
        "cmd" => (
            "ping -n 13 127.0.0.1 >nul & echo FINISHED",
            "ping -n 301 127.0.0.1 >nul",
            "echo RESUMED",
        ),
        _ => ("sleep 12; echo FINISHED", "sleep 300", "echo RESUMED"),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_exec_waits_past_ten_seconds_before_serializing_next_request() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let (command, _, _) = shell_commands(&test).await?;
    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("first"),
                ev_function_call(
                    "foreground",
                    "exec_command",
                    &json!({"cmd": command}).to_string(),
                ),
                ev_completed("first"),
            ]),
            sse(vec![
                ev_assistant_message("answer", "done"),
                ev_completed("second"),
            ]),
        ],
    )
    .await;
    submit_unified_exec_turn(
        &test,
        "run a foreground command",
        PermissionProfile::Disabled,
    )
    .await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::TerminalInteraction(event) if event.call_id == "foreground")).await;
    tokio::time::sleep(Duration::from_secs(/*secs*/ 11)).await;
    assert_eq!(
        requests.requests().len(),
        1,
        "the tool must still hold serialization admission"
    );
    // The command deliberately outlasts the former ten-second foreground deadline.
    wait_for_event_with_timeout(
        &test.codex,
        |event| matches!(event, EventMsg::TurnComplete(_)),
        Duration::from_secs(/*secs*/ 20),
    )
    .await;
    let recorded = requests.requests();
    assert_eq!(recorded.len(), 2);
    let output = recorded[1]
        .function_call_output_text("foreground")
        .expect("foreground output");
    let output = parse_unified_exec_output(&output)?;
    assert_eq!(
        (output.process_id, output.exit_code, output.output.trim()),
        (None, Some(0), "FINISHED")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_kills_foreground_and_interactive_background_then_allows_new_command()
-> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "Wine does not retain interactive PTY child processes"
    );
    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let (_, long_command, resumed_command) = shell_commands(&test).await?;
    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("interactive"),
                ev_function_call(
                    "interactive",
                    "exec_command",
                    &json!({"cmd": long_command, "tty": true, "yield_time_ms": 250}).to_string(),
                ),
                ev_completed("interactive"),
            ]),
            sse(vec![
                ev_response_created("foreground"),
                ev_function_call(
                    "foreground",
                    "exec_command",
                    &json!({"cmd": long_command}).to_string(),
                ),
                ev_completed("foreground"),
            ]),
            sse(vec![
                ev_response_created("resumed"),
                ev_function_call(
                    "resumed",
                    "exec_command",
                    &json!({"cmd": resumed_command}).to_string(),
                ),
                ev_completed("resumed"),
            ]),
            sse(vec![
                ev_assistant_message("answer", "done"),
                ev_completed("done"),
            ]),
        ],
    )
    .await;
    submit_unified_exec_turn(
        &test,
        "start interactive and foreground commands",
        PermissionProfile::Disabled,
    )
    .await?;
    // Windows interactive startup retains its ten-second floor, instead of the new two minutes.
    wait_for_event_with_timeout(&test.codex, |event| matches!(event, EventMsg::TerminalInteraction(event) if event.call_id == "foreground"),
        Duration::from_secs(/*secs*/ 25)).await;
    assert_eq!(test.codex.list_background_terminals().await.len(), 2);
    assert_eq!(requests.requests().len(), 2);
    test.codex.submit(Op::Interrupt).await?;
    let mut ended = std::collections::BTreeSet::new();
    let mut aborted = false;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 15), async {
        while !aborted || ended.len() < 2 {
            match test.codex.next_event().await?.msg {
                EventMsg::ExecCommandEnd(event)
                    if event.call_id == "foreground" || event.call_id == "interactive" =>
                {
                    ended.insert(event.call_id);
                }
                EventMsg::TurnAborted(_) => aborted = true,
                _ => {}
            }
        }
        anyhow::Ok(())
    })
    .await??;
    assert!(test.codex.list_background_terminals().await.is_empty());
    test.submit_turn("run a fresh command").await?;
    let recorded = requests.requests();
    assert_eq!(recorded.len(), 4);
    let output = recorded[3]
        .function_call_output_text("resumed")
        .expect("new command output");
    assert!(output.contains("RESUMED"));
    assert!(
        recorded[2]
            .body_json()
            .to_string()
            .contains("Running shell processes in this session were terminated")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_after_foreground_exit_keeps_successful_output() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let (_, _, command) = shell_commands(&test).await?;
    let request = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("first"),
            ev_function_call(
                "quick",
                "exec_command",
                &json!({"cmd": command}).to_string(),
            ),
            ev_completed("first"),
        ]),
    )
    .await;
    submit_unified_exec_turn(&test, "run and interrupt", PermissionProfile::Disabled).await?;
    let end = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::ExecCommandEnd(event) if event.call_id == "quick" => Some(event.clone()),
        _ => None,
    })
    .await;
    assert_eq!(end.exit_code, 0);
    assert!(end.aggregated_output.contains("RESUMED"));
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert!(test.codex.list_background_terminals().await.is_empty());
    assert!(!request.requests().is_empty());
    Ok(())
}
