use super::*;

#[test]
fn windows_runner_uses_app_server_default_output_cap() {
    assert_eq!(
        AppServerWorkspaceCommandRunner::output_cap_params(
            true,
            &WorkspaceCommand::new(["git", "status"]),
        ),
        (None, false)
    );
    assert_eq!(
        AppServerWorkspaceCommandRunner::output_cap_params(
            true,
            &WorkspaceCommand::new(["git", "diff"]).disable_output_cap(),
        ),
        (None, false)
    );
}

#[test]
fn non_windows_runner_preserves_requested_output_cap_policy() {
    assert_eq!(
        AppServerWorkspaceCommandRunner::output_cap_params(
            false,
            &WorkspaceCommand::new(["git", "status"]),
        ),
        (Some(64 * 1024), false)
    );
    assert_eq!(
        AppServerWorkspaceCommandRunner::output_cap_params(
            false,
            &WorkspaceCommand::new(["git", "diff"]).disable_output_cap(),
        ),
        (None, true)
    );
}
