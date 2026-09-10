//! Safety-buffering status UI for active turns.

use super::*;
use codex_app_server_protocol::ModelSafetyBufferingUpdatedNotification;

const SAFETY_BUFFERING_PROMPT_VIEW_ID: &str = "safety-buffering-prompt";
const SAFETY_BUFFERING_HEADER: &str =
    "Our systems are thinking a bit more about this request before responding.";
const SAFETY_BUFFERING_MESSAGE_WITH_RETRY: &str = "Hang tight or retry with a faster model for a quicker response, though it may be less capable of handling complex requests.";

#[derive(Debug)]
struct ActiveSafetyBuffering {
    turn_id: String,
    agent_message_started: bool,
}

#[derive(Debug, Default)]
pub(super) struct SafetyBufferingState {
    active: Option<ActiveSafetyBuffering>,
}

impl ChatWidget {
    pub(super) fn reset_safety_buffering_for_turn_start(&mut self) {
        self.bottom_pane
            .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);
        self.safety_buffering.active = None;
    }

    pub(crate) fn clear_safety_buffering(&mut self) {
        self.bottom_pane
            .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);
        self.safety_buffering = SafetyBufferingState::default();
    }

    pub(super) fn mark_safety_buffering_agent_message_started(&mut self) {
        if let Some(active) = self.safety_buffering.active.as_mut() {
            active.agent_message_started = true;
        }
    }

    pub(super) fn safety_buffering_is_waiting(&self) -> bool {
        self.safety_buffering
            .active
            .as_ref()
            .is_some_and(|active| !active.agent_message_started)
    }

    pub(super) fn on_model_safety_buffering_updated(
        &mut self,
        notification: ModelSafetyBufferingUpdatedNotification,
        replay_kind: Option<ReplayKind>,
    ) {
        let ModelSafetyBufferingUpdatedNotification {
            turn_id,
            show_buffering_ui,
            ..
        } = notification;
        if matches!(replay_kind, Some(ReplayKind::ResumeInitialMessages))
            || !self.turn_lifecycle.agent_turn_running
            || self.turn_lifecycle.last_turn_id.as_deref() != Some(turn_id.as_str())
        {
            return;
        }
        if !show_buffering_ui {
            if self
                .safety_buffering
                .active
                .as_ref()
                .is_some_and(|active| active.turn_id == turn_id)
            {
                self.bottom_pane
                    .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);
                self.safety_buffering.active = None;
                self.restore_reasoning_status_header();
            }
            return;
        }

        let previous_active = self
            .safety_buffering
            .active
            .as_ref()
            .filter(|active| active.turn_id == turn_id);
        let should_show_warning = previous_active.is_none();
        let agent_message_started =
            previous_active.is_some_and(|active| active.agent_message_started);
        self.safety_buffering.active = Some(ActiveSafetyBuffering {
            turn_id: turn_id.clone(),
            agent_message_started,
        });

        self.bottom_pane.ensure_status_indicator();
        self.set_status(
            "Working".to_string(),
            Some(SAFETY_BUFFERING_HEADER.to_string()),
            StatusDetailsCapitalization::Preserve,
            /*details_max_lines*/ 6,
        );

        if !should_show_warning {
            return;
        }
        self.bottom_pane
            .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);

        self.on_warning(SAFETY_BUFFERING_MESSAGE_WITH_RETRY);
    }
}
