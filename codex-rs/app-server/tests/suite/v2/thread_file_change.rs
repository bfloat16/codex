use std::time::Duration;

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_apply_patch_sse_response;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadFileChangeDisposition;
use codex_app_server_protocol::ThreadFileChangeKind;
use codex_app_server_protocol::ThreadFileChangeReadParams;
use codex_app_server_protocol::ThreadFileChangeReadResponse;
use codex_app_server_protocol::ThreadFileChangeRestoreParams;
use codex_app_server_protocol::ThreadFileChangeRestoreResponse;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS;
use core_test_support::responses;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

#[test_case::test_case(false; "completed")]
#[test_case::test_case(true; "background_terminal_running")]
#[tokio::test]
async fn restores_apply_patch_changes_before_selected_turn(background: bool) -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );

    let root = TempDir::new()?;
    let codex_home = root.path().join("codex-home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&codex_home)?;
    std::fs::create_dir(&workspace)?;
    let file = workspace.join("sample.txt");
    let moved_file = workspace.join("moved.txt");
    std::fs::write(&file, "original\n")?;
    let patch = "*** Begin Patch\n*** Update File: sample.txt\n*** Move to: moved.txt\n@@\n-original\n+managed\n*** End Patch";
    let mut replies = vec![create_apply_patch_sse_response(patch, "patch-call")?];
    if background {
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 60"
        } else {
            "sleep 60"
        };
        let shell = if cfg!(windows) {
            "powershell.exe"
        } else {
            "/bin/sh"
        };
        let args = serde_json::to_string(
            &serde_json::json!({"cmd": cmd, "shell": shell, "login": false, "yield_time_ms": 10}),
        )?;
        replies.push(responses::sse(vec![
            responses::ev_response_created("background"),
            responses::ev_function_call("background-call", "exec_command", &args),
            responses::ev_completed("background"),
        ]));
    }
    replies.push(create_final_assistant_message_sse_response("done")?);
    let server = create_mock_responses_server_sequence(replies).await;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(codex_features::Feature::UnifiedExec)
        .with_approval_policy("never")
        .with_sandbox_mode("workspace-write")
        .write(&codex_home)?;
    let mut app = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .without_auto_env()
        .build_initialized()
        .await?;
    let ThreadStartResponse { thread, .. } = app
        .request(|request_id| ClientRequest::ThreadStart {
            request_id,
            params: ThreadStartParams {
                cwd: Some(workspace.to_string_lossy().into_owned()),
                history_mode: Some(ThreadHistoryMode::Paginated),
                approval_policy: Some(AskForApproval::Never),
                permissions: Some(BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS.to_string()),
                ..Default::default()
            },
        })
        .await?;
    let completed = app
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "update the file".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;
    assert!(!file.exists());
    assert_eq!(std::fs::read_to_string(&moved_file)?, "managed\n");

    // Rewind must use the checkpoint even when the tracked file was edited again.
    std::fs::write(
        &moved_file,
        "later manual edit
",
    )?;
    let terminals: codex_app_server_protocol::ThreadBackgroundTerminalsListResponse = app
        .request(|request_id| ClientRequest::ThreadBackgroundTerminalsList {
            request_id,
            params: codex_app_server_protocol::ThreadBackgroundTerminalsListParams {
                thread_id: thread.id.clone(),
                cursor: None,
                limit: None,
            },
        })
        .await?;
    assert_eq!(terminals.data.len(), usize::from(background));
    let preview: ThreadFileChangeReadResponse = app
        .request(|request_id| ClientRequest::ThreadFileChangeRead {
            request_id,
            params: ThreadFileChangeReadParams {
                thread_id: thread.id.clone(),
                before_turn_id: completed.turn.id.clone(),
            },
        })
        .await?;
    assert_eq!(
        preview
            .data
            .iter()
            .map(|change| (
                change.path.as_str().to_string(),
                change.change_kind,
                change.disposition
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                moved_file.to_string_lossy().into_owned(),
                ThreadFileChangeKind::Delete,
                ThreadFileChangeDisposition::Restorable,
            ),
            (
                file.to_string_lossy().into_owned(),
                ThreadFileChangeKind::Create,
                ThreadFileChangeDisposition::Restorable,
            ),
        ]
    );

    let restored: ThreadFileChangeRestoreResponse = app
        .request(|request_id| ClientRequest::ThreadFileChangeRestore {
            request_id,
            params: ThreadFileChangeRestoreParams {
                thread_id: thread.id.clone(),
                before_turn_id: completed.turn.id,
            },
        })
        .await?;
    assert_eq!(restored.restored, preview.data);
    assert_eq!(restored.skipped, Vec::new());
    assert_eq!(restored.failed, Vec::new());
    assert_eq!(std::fs::read_to_string(file)?, "original\n");
    assert!(!moved_file.exists());
    let terminals: codex_app_server_protocol::ThreadBackgroundTerminalsListResponse = app
        .request(|request_id| ClientRequest::ThreadBackgroundTerminalsList {
            request_id,
            params: codex_app_server_protocol::ThreadBackgroundTerminalsListParams {
                thread_id: thread.id,
                cursor: None,
                limit: None,
            },
        })
        .await?;
    assert!(terminals.data.is_empty());
    Ok(())
}

#[tokio::test]
async fn rejects_file_restore_while_turn_is_in_progress() -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "active-turn file restore uses a host workspace fixture"
    );

    let root = TempDir::new()?;
    let codex_home = root.path().join("codex-home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&codex_home)?;
    std::fs::create_dir(&workspace)?;
    let server = responses::start_mock_server().await;
    let delayed_response =
        responses::sse_response(create_final_assistant_message_sse_response("done")?)
            .set_delay(Duration::from_secs(5));
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(delayed_response)
        .mount(&server)
        .await;
    MockResponsesConfig::new(&server.uri())
        .with_approval_policy("never")
        .with_sandbox_mode("workspace-write")
        .write(&codex_home)?;
    let mut app = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .without_auto_env()
        .build_initialized()
        .await?;
    let ThreadStartResponse { thread, .. } = app
        .request(|request_id| ClientRequest::ThreadStart {
            request_id,
            params: ThreadStartParams {
                cwd: Some(workspace.to_string_lossy().into_owned()),
                history_mode: Some(ThreadHistoryMode::Paginated),
                approval_policy: Some(AskForApproval::Never),
                permissions: Some(BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS.to_string()),
                ..Default::default()
            },
        })
        .await?;
    let turn_request_id = app
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "keep this turn active".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace),
            ..Default::default()
        })
        .await?;
    let TurnStartResponse { turn } = app.read_response(turn_request_id).await?;

    let restore_request_id = app
        .send_request(
            "thread/fileChange/restore",
            Some(serde_json::to_value(ThreadFileChangeRestoreParams {
                thread_id: thread.id.clone(),
                before_turn_id: turn.id.clone(),
            })?),
        )
        .await?;
    let error: JSONRPCError = app
        .read_stream_until_error_message(RequestId::Integer(restore_request_id))
        .await?;
    assert_eq!(
        error.error.message,
        "failed to restore files: file checkpoint data is invalid: cannot access file checkpoints while a turn is in progress"
    );

    Ok(())
}
