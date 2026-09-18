//! Session-scoped shortcuts for the ordinary built-in permission modes.

use super::*;

impl ChatWidget {
    pub(super) fn handle_permission_shortcut(&mut self, key_event: KeyEvent) -> bool {
        let backtab =
            matches!(key_event.code, KeyCode::BackTab) && key_event.kind == KeyEventKind::Press;
        if backtab && self.collaboration_modes_enabled() {
            return self.handle_collaboration_mode_shift_tab();
        }
        let forward = if self.chat_keymap.next_permission_mode.is_pressed(key_event) || backtab {
            true
        } else if self
            .chat_keymap
            .previous_permission_mode
            .is_pressed(key_event)
        {
            false
        } else {
            return false;
        };
        if !self.bottom_pane.no_modal_or_popup_active() {
            return false;
        }
        if self.blocks_direct_input {
            self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
            return true;
        }
        let mut choices = self.permission_shortcut_choices();
        if !forward {
            choices.reverse();
        }
        let start = choices
            .iter()
            .position(|(current, _)| *current)
            .map_or(0, |index| index + 1);
        if let Some((_, selection)) = choices
            .iter()
            .cycle()
            .skip(start)
            .take(choices.len())
            .find(|(current, _)| !current)
        {
            self.apply_permission_shortcut_selection(selection.clone());
        } else {
            self.add_info_message(
                "No other permission modes are available.".to_string(),
                /*hint*/ None,
            );
        }
        true
    }

    fn permission_shortcut_choices(&self) -> Vec<(bool, PermissionProfileSelection)> {
        let current_approval =
            AskForApproval::from(self.config.permissions.approval_policy.value());
        let active_profile = self.config.permissions.active_permission_profile();
        let mut choices = Vec::new();
        for preset in builtin_approval_presets() {
            if !matches!(preset.id, "read-only" | "auto" | "full-access") {
                continue;
            }
            if !cfg!(target_os = "windows") && preset.id == "read-only" {
                continue;
            }
            for reviewer in [ApprovalsReviewer::User, ApprovalsReviewer::AutoReview] {
                if reviewer == ApprovalsReviewer::AutoReview
                    && (preset.id != "auto"
                        || !self.config.features.enabled(Feature::GuardianApproval))
                {
                    continue;
                }
                let approval = AskForApproval::from(preset.approval);
                let requirements = self.config.config_layer_stack.requirements();
                if self
                    .permission_mode_disabled_reason(&preset, approval)
                    .is_some()
                    || requirements.approvals_reviewer.can_set(&reviewer).is_err()
                    || (requirements.auto_review_required_for_model(self.current_model())
                        && reviewer != ApprovalsReviewer::AutoReview)
                {
                    continue;
                }
                #[cfg(target_os = "windows")]
                if preset.id == "auto"
                    && reviewer == ApprovalsReviewer::User
                    && (crate::windows_sandbox::level_from_config(&self.config)
                        == WindowsSandboxLevel::Disabled
                        || self.world_writable_warning_details().is_some())
                {
                    continue;
                }
                let is_current = current_approval == approval
                    && self.config.approvals_reviewer == reviewer
                    && active_profile.as_ref().map_or_else(
                        || {
                            Self::preset_matches_current(
                                current_approval,
                                self.config.permissions.permission_profile(),
                                self.config.cwd.as_path(),
                                &preset,
                            )
                        },
                        |active| active.id == preset.active_permission_profile.id,
                    );
                let label = match (preset.id, reviewer) {
                    ("auto", ApprovalsReviewer::User) => ASK_FOR_APPROVAL_LABEL,
                    ("auto", ApprovalsReviewer::AutoReview) => APPROVE_FOR_ME_LABEL,
                    _ => preset.label,
                };
                choices.push((
                    is_current,
                    PermissionProfileSelection {
                        profile_id: preset.active_permission_profile.id.clone(),
                        approval_policy: Some(approval),
                        approvals_reviewer: Some(reviewer),
                        display_label: label.to_string(),
                    },
                ));
            }
        }
        choices
    }

    fn handle_collaboration_mode_shift_tab(&mut self) -> bool {
        if !self.bottom_pane.no_modal_or_popup_active() {
            return false;
        }
        if self.blocks_direct_input {
            self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
            return true;
        }
        if self.thread_id.is_none() {
            self.cycle_collaboration_mode_preserving_model();
            return true;
        }
        let choices = self.permission_shortcut_choices();
        if self.active_mode_kind() == ModeKind::Plan {
            self.cycle_collaboration_mode_preserving_model();
            if let Some((_, selection)) = choices.first() {
                self.apply_permission_shortcut_selection(selection.clone());
            }
            return true;
        }
        let current = choices.iter().position(|(current, _)| *current);
        let Some(current) = current else {
            self.cycle_collaboration_mode_preserving_model();
            return true;
        };
        if let Some((_, selection)) = choices.get(current + 1) {
            self.apply_permission_shortcut_selection(selection.clone());
        } else {
            self.cycle_collaboration_mode_preserving_model();
        }
        true
    }

    fn apply_permission_shortcut_selection(&mut self, selection: PermissionProfileSelection) {
        let Some(preset) = builtin_approval_presets()
            .into_iter()
            .find(|preset| preset.active_permission_profile.id == selection.profile_id)
        else {
            self.add_error_message(format!(
                "Unknown built-in permission profile: {}",
                selection.profile_id
            ));
            return;
        };
        let approval = selection
            .approval_policy
            .unwrap_or_else(|| AskForApproval::from(preset.approval));
        let reviewer = selection
            .approvals_reviewer
            .unwrap_or(ApprovalsReviewer::User);
        if let Err(error) = self.set_permission_profile_with_active_profile(
            preset.permission_profile.clone(),
            Some(preset.active_permission_profile.clone()),
        ) {
            self.add_error_message(format!("Failed to set permission profile: {error}"));
            return;
        }
        self.set_approval_policy(approval);
        self.set_approvals_reviewer(reviewer);
        self.app_event_tx
            .send(AppEvent::CodexOp(AppCommand::override_turn_context(
                /*cwd*/ None,
                Some(approval),
                Some(reviewer),
                Some(preset.permission_profile),
                Some(preset.active_permission_profile),
                /*windows_sandbox_level*/ None,
                /*model*/ None,
                /*effort*/ None,
                /*summary*/ None,
                /*service_tier*/ None,
                /*collaboration_mode*/ None,
                /*personality*/ None,
            )));
        self.app_event_tx
            .send(AppEvent::UpdateAskForApprovalPolicy(approval));
        self.app_event_tx
            .send(AppEvent::UpdateActivePermissionProfile(
                ActivePermissionProfile::new(selection.profile_id),
            ));
        self.app_event_tx
            .send(AppEvent::UpdateApprovalsReviewer(reviewer));
    }
}
