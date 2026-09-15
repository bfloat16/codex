use super::permissions::requirements_stack;
use super::*;
use ApprovalsReviewer::AutoReview;
use ApprovalsReviewer::User;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn permission_shortcuts_use_local_permission_events() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
    chat.chat_keymap.next_permission_mode = vec![crate::key_hint::plain(KeyCode::F(8))];
    #[cfg(target_os = "windows")]
    {
        chat.local_settings.notices.hide_world_writable_warning = Some(true);
        chat.set_windows_sandbox_mode(Some(WindowsSandboxModeToml::Unelevated));
    }
    chat.config.approvals_reviewer = User;
    chat.config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(":workspace"),
        ))
        .expect("set current profile");

    chat.handle_key_event(KeyEvent::from(KeyCode::F(8)));

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::CodexOp(AppCommand::OverrideTurnContext {
            approval_policy: Some(AskForApproval::OnRequest),
            approvals_reviewer: Some(AutoReview),
            active_permission_profile: Some(ActivePermissionProfile { id, .. }),
            ..
        })) if id == ":workspace"
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateAskForApprovalPolicy(
            AskForApproval::OnRequest
        ))
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateActivePermissionProfile(profile))
            if profile.id == ":workspace"
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateApprovalsReviewer(AutoReview))
    ));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn permission_shortcuts_respect_managed_mode_requirements() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
    chat.config.approvals_reviewer = AutoReview;
    chat.chat_keymap.next_permission_mode = vec![crate::key_hint::plain(KeyCode::F(8))];
    chat.config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(":workspace"),
        ))
        .expect("set active profile");

    for requirements in [
        codex_config::ConfigRequirementsToml {
            allowed_approvals_reviewers: Some(vec![AutoReview]),
            ..Default::default()
        },
        codex_config::ConfigRequirementsToml {
            auto_review: Some(codex_config::AutoReviewRequirementsToml {
                required_on_models: Some(vec![chat.current_model().to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        },
    ] {
        chat.config.config_layer_stack = requirements_stack(requirements);
        chat.handle_key_event(KeyEvent::from(KeyCode::F(8)));
        let AppEvent::InsertHistoryCell(cell) = rx.try_recv().expect("unavailable-mode notice")
        else {
            panic!("must not submit a forbidden mode");
        };
        insta::assert_snapshot!(
            "permission_shortcut_no_alternative",
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        );
        assert!(rx.try_recv().is_err(), "must not submit a forbidden mode");
    }
}

#[tokio::test]
async fn shift_tab_cycles_to_plan_without_changing_model() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    chat.config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::Disabled,
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS),
        ))
        .expect("set full-access profile");
    chat.config
        .permissions
        .approval_policy
        .set(AskForApproval::Never.to_core())
        .expect("set full-access approval policy");
    chat.config.approvals_reviewer = User;
    let model = chat.current_model().to_string();

    chat.handle_key_event(KeyEvent::from(KeyCode::BackTab));

    assert_eq!(chat.active_mode_kind(), ModeKind::Plan);
    assert_eq!(chat.current_model(), model);
    assert!(rx.try_recv().is_err(), "mode switch must stay local");

    chat.handle_key_event(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(chat.active_mode_kind(), ModeKind::Default);
}
