//! Input routing for the read-only transcript viewer.
//!
//! Rewind uses a bottom-pane picker; this overlay remains a read-only transcript viewer.

use super::*;

impl App {
    pub(super) fn handle_legacy_transcript_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: TuiEvent,
    ) -> Result<bool> {
        if let TuiEvent::Key(key_event) = &event
            && let Some(Overlay::Transcript(overlay)) = self.overlay.as_ref()
            && overlay.should_load_older(*key_event)
            && let Some(thread_id) = self.chat_widget.thread_id()
            && app_server.has_older_history(thread_id)
            && self.request_older_history_page(app_server, thread_id)
        {
            if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                overlay.set_history_state(if overlay.should_load_from_start(*key_event) {
                    TranscriptHistoryState::LoadingBeginning
                } else {
                    TranscriptHistoryState::LoadingOlder
                });
            }
            tui.frame_requester().schedule_frame();
        }
        self.overlay_forward_event(tui, event)?;
        Ok(true)
    }
}
