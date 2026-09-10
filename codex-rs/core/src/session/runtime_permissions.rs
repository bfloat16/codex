use codex_network_proxy::NetworkProxy;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EnvironmentConfig;

/// Permission state that can change while a turn is already running.
#[derive(Clone, Debug)]
pub(crate) struct RuntimePermissions {
    pub(super) approval_policy: AskForApproval,
    pub(super) approvals_reviewer: ApprovalsReviewer,
    pub(super) permission_profile: PermissionProfile,
    pub(super) environment_config: EnvironmentConfig,
    pub(super) network: Option<NetworkProxy>,
    pub(super) windows_sandbox_level: WindowsSandboxLevel,
}
