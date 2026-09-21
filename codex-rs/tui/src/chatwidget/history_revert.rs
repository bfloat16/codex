//! Discard transient rendering state after the server confirms a history rewind.

use super::ChatWidget;

impl ChatWidget {
    pub(crate) fn clear_reverted_turn(&mut self) {
        self.bottom_pane
            .dismiss_view_by_id(crate::app_backtrack::BACKTRACK_RESTORE_VIEW_ID);
        self.bottom_pane
            .dismiss_view_by_id(crate::app_backtrack::BACKTRACK_MESSAGE_VIEW_ID);
        let discarded_calls = self
            .running_commands
            .keys()
            .cloned()
            .chain(
                self.unified_exec_processes
                    .iter()
                    .map(|process| process.call_id.clone()),
            )
            .chain(self.interrupted_unified_exec_calls.iter().cloned())
            .chain(self.transcript.running_tool_cells.keys().cloned())
            .collect::<Vec<_>>();
        // These cells and streams belong to removed history. Finalization must not flush them
        // back into the transcript, and delayed terminal completions must not recreate them.
        self.transcript.take_active_cell();
        self.transcript.bump_active_cell_revision();
        self.stream_controller = None;
        self.plan_stream_controller = None;
        self.unified_exec_wait_streak = None;
        self.unified_exec_processes.clear();
        self.interrupted_unified_exec_calls.clear();
        self.transcript.running_tool_cells.clear();
        self.output_free_interrupt_turn_id = None;
        self.finalize_turn();
        self.suppressed_exec_calls.extend(discarded_calls);
        self.turn_lifecycle.reset_thread();
        self.transcript.reset_copy_history();
        self.transcript.reset_turn_flags();
        self.request_redraw();
    }
}
