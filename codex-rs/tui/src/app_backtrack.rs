//! Prompt rewind and transcript overlay event routing.
//!
//! This file owns the double-Esc rewind flow and also mediates a key rendering boundary for the
//! read-only transcript overlay.
//!
//! Rewind stays in the main view. It first presents historical prompts below the composer, then
//! offers separate choices for restoring tracked files, conversation history, or both. Conversation
//! restore removes the selected turn and every later turn, then puts the prompt back in the
//! composer.
//!
//! Backtrack operates as a small state machine:
//! - The first `Esc` in the main view "primes" the feature and captures a base thread id.
//! - A subsequent `Esc` opens the prompt picker below the composer.
//! - Selecting a prompt resolves its persisted turn and opens the restore-choice picker.
//! - Selecting a restore mode applies the requested file and/or conversation rewind.
//!
//! The separate transcript overlay (`Ctrl+T`) is read-only. It renders committed transcript cells
//! plus a render-only live tail derived from the current in-flight `ChatWidget.active_cell`.
//!
//! That live tail is kept in sync during `TuiEvent::Draw` handling for `Overlay::Transcript` by
//! asking `ChatWidget` for an active-cell cache key and transcript lines and by passing them into
//! `TranscriptOverlay::sync_live_tail`. This preserves the invariant that the overlay reflects
//! both committed history and in-flight activity without changing flush or coalescing behavior.

mod legacy_input;
mod rollback_target;

pub(crate) use rollback_target::backtrack_rollback_target;
pub(crate) use rollback_target::is_hidden_nested_review_turn;

use std::any::TypeId;
use std::sync::Arc;

use crate::app::App;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::RewindPromptItem;
use crate::bottom_pane::RewindRestoreOption;
use crate::bottom_pane::RewindViewParams;
use crate::chatwidget::UserMessage;
#[cfg(test)]
use crate::history_cell::AgentMessageCell;
use crate::history_cell::SessionInfoCell;
use crate::history_cell::UserHistoryCell;
use crate::pager_overlay::Overlay;
use crate::pager_overlay::TranscriptHistoryState;
use crate::tui;
use crate::tui::TuiEvent;
use codex_protocol::ThreadId;
use codex_protocol::models::local_image_label_text;
use color_eyre::eyre::Result;
use ratatui::text::Line;

const NO_PREVIOUS_MESSAGE_TO_EDIT: &str = "No previous message to edit.";
const BACKTRACK_MESSAGE_VIEW_ID: &str = "backtrack-message";
const BACKTRACK_RESTORE_VIEW_ID: &str = "backtrack-restore";
pub(crate) const SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE: &str =
    "Editing previous prompts is unavailable in side conversations.";

/// Aggregates all backtrack-related state used by the App.
#[derive(Default)]
pub(crate) struct BacktrackState {
    /// True when Esc has primed backtrack mode in the main view.
    pub(crate) primed: bool,
    /// Session id of the base thread whose transcript is being inspected.
    ///
    /// If the current thread changes, backtrack selections become invalid and must be ignored.
    pub(crate) base_id: Option<ThreadId>,
    /// Index of the user message selected by legacy backtrack tests and pagination state.
    ///
    /// This is an index into the filtered "user messages since the last session start" view,
    /// not an index into `transcript_cells`. `usize::MAX` indicates "no selection".
    pub(crate) nth_user_message: usize,
    /// Legacy transcript-preview state retained for paginated transcript bookkeeping.
    pub(crate) overlay_preview_active: bool,
    /// True while older persisted pages are being loaded before opening the prompt picker.
    pub(crate) loading_older_history: bool,
    pub(crate) pending_rollback: Option<PendingBacktrackRollback>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingBacktrackRollback {
    pub(crate) selection: BacktrackSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BacktrackRollbackTarget {
    pub(crate) before_turn_id: String,
    pub(crate) legacy_num_turns: u32,
    pub(crate) removed_turn_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BacktrackFileRestoreSummary {
    pub(crate) restorable: usize,
    pub(crate) blocked: usize,
}

/// A user-visible backtrack choice that can be reopened after truncating later history.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BacktrackSelection {
    pub(crate) thread_id: ThreadId,
    /// The selected user message, counted from the most recent session start.
    pub(crate) nth_user_message: usize,
    /// Number of visible user messages after the selection.
    ///
    /// Unlike `nth_user_message`, this remains stable when the transcript only contains a suffix
    /// of the persisted thread history.
    pub(crate) newer_user_messages: usize,
    pub(crate) prompt: UserMessage,
}

impl App {
    /// Route overlay events while the transcript overlay is active.
    ///
    /// If backtrack preview is active, Esc / Left steps selection, Right steps forward, Enter
    /// confirms. Otherwise, Esc begins preview mode and all other events are forwarded to the
    /// overlay.
    pub(crate) async fn handle_backtrack_overlay_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: TuiEvent,
    ) -> Result<bool> {
        self.handle_legacy_transcript_event(tui, app_server, event)
    }

    /// Handle global Esc presses for backtracking when no overlay is present.
    pub(crate) fn handle_backtrack_esc_key(&mut self, tui: &mut tui::Tui) {
        if !self.chat_widget.composer_is_empty() {
            return;
        }

        if !self.backtrack.primed {
            self.prime_backtrack();
        } else if self.overlay.is_none() {
            self.open_backtrack_message_picker(tui);
        }
    }

    /// Request a rollback before the selected prompt in the current thread.
    #[cfg(test)]
    pub(crate) fn apply_backtrack_selection(&mut self, selection: BacktrackSelection) {
        if self.chat_widget.side_conversation_active() {
            self.reset_backtrack_state();
            self.chat_widget
                .add_error_message(SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE.to_string());
            return;
        }

        if self.chat_widget.thread_id() != Some(selection.thread_id) {
            return;
        }

        if self.backtrack.pending_rollback.is_some() {
            return;
        }
        self.backtrack.pending_rollback = Some(PendingBacktrackRollback {
            selection: selection.clone(),
        });
        self.app_event_tx
            .send(AppEvent::RollbackSessionForPromptEdit {
                thread_id: selection.thread_id,
                nth_user_message: selection.nth_user_message,
                newer_user_messages: selection.newer_user_messages,
                prompt: selection.prompt,
            });
    }

    pub(crate) fn handle_backtrack_rollback_succeeded(&mut self, nth_user_message: usize) {
        let pending = self.backtrack.pending_rollback.take();
        let nth_user_message = pending
            .map(|pending| pending.selection.nth_user_message)
            .unwrap_or(nth_user_message);
        let Some(cut_idx) = nth_user_position(&self.transcript_cells, nth_user_message) else {
            return;
        };
        self.transcript_cells.truncate(cut_idx);
        self.sync_owned_screen_cells();
        self.chat_widget.clear_pending_token_activity_refreshes();
        self.chat_widget.clear_pending_rate_limit_reset_hint();
        if let Some(Overlay::Transcript(overlay)) = &mut self.overlay {
            overlay.replace_cells(self.transcript_cells.clone());
        }
        self.deferred_history_lines.clear();
        self.reset_backtrack_state();
        self.backtrack_render_pending = true;
    }

    pub(crate) fn handle_backtrack_rollback_failed(&mut self) {
        self.backtrack.pending_rollback = None;
    }

    pub(crate) fn restore_backtrack_prompt_after_rollback_error(
        &mut self,
        prompt: UserMessage,
        err: impl std::fmt::Display,
    ) {
        self.chat_widget.restore_user_message_to_composer(prompt);
        self.chat_widget.add_error_message(format!(
            "Failed to roll back before the selected prompt: {err}"
        ));
    }

    /// Open transcript overlay (enters alternate screen and shows full transcript).
    pub(crate) fn open_transcript_overlay(&mut self, tui: &mut tui::Tui) {
        let _ = tui.enter_alt_screen();
        self.overlay = Some(Overlay::new_transcript(
            self.transcript_cells.clone(),
            self.keymap.pager.clone(),
        ));
        if self.scrollback_has_older_history
            && let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut()
        {
            overlay.set_history_state(TranscriptHistoryState::Partial);
        }
        tui.frame_requester().schedule_frame();
    }

    /// Close transcript overlay and restore normal UI.
    pub(crate) fn close_transcript_overlay(&mut self, tui: &mut tui::Tui) {
        let _ = tui.leave_alt_screen();
        let was_backtrack = self.backtrack.overlay_preview_active;
        if !self.deferred_history_lines.is_empty() {
            let lines = std::mem::take(&mut self.deferred_history_lines);
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                lines,
                self.history_line_wrap_policy(),
            );
        }
        self.overlay = None;
        if self.pending_thread_usage_history_refresh
            && let Err(err) = self.refresh_thread_usage_history_tail(tui)
        {
            tracing::warn!(error = %err, "failed to refresh thread usage after closing overlay");
        }
        self.backtrack.overlay_preview_active = false;
        tui.frame_requester().schedule_frame();
        if was_backtrack {
            // Ensure backtrack state is fully reset when overlay closes (e.g. via 'q').
            self.reset_backtrack_state();
        }
    }

    /// Initialize backtrack state and show composer hint.
    fn prime_backtrack(&mut self) {
        self.backtrack.primed = true;
        self.backtrack.nth_user_message = usize::MAX;
        self.backtrack.base_id = self.chat_widget.thread_id();
        if has_backtrack_target(&self.transcript_cells) {
            self.chat_widget.show_esc_backtrack_hint();
        }
    }

    /// Open the Claude-style prompt picker in the bottom pane.
    pub(crate) fn open_backtrack_message_picker(&mut self, tui: &mut tui::Tui) {
        if self.scrollback_has_older_history {
            if !self.backtrack.loading_older_history
                && let Some(thread_id) = self.chat_widget.thread_id()
            {
                self.backtrack.loading_older_history = true;
                self.app_event_tx
                    .send(AppEvent::RequestOlderScrollbackHistory { thread_id });
            }
            return;
        }
        if !has_backtrack_target(&self.transcript_cells) {
            self.reset_backtrack_state();
            self.chat_widget
                .add_info_message(NO_PREVIOUS_MESSAGE_TO_EDIT.to_string(), /*hint*/ None);
            tui.frame_requester().schedule_frame();
            return;
        }
        self.chat_widget.clear_esc_backtrack_hint();
        let user_positions = user_positions_iter(&self.transcript_cells).collect::<Vec<_>>();
        let mut items = user_positions
            .iter()
            .enumerate()
            .filter_map(|(nth_user_message, position)| {
                let selection = self.backtrack_selection(nth_user_message)?;
                let end = user_positions
                    .get(nth_user_message + 1)
                    .copied()
                    .unwrap_or(self.transcript_cells.len());
                let code_summary =
                    backtrack_code_summary(&self.transcript_cells[position.saturating_add(1)..end]);
                let prompt = selection
                    .prompt
                    .text
                    .lines()
                    .next()
                    .unwrap_or("(no prompt)")
                    .to_string();
                Some(RewindPromptItem {
                    prompt,
                    code_summary: Some(code_summary),
                    is_current: false,
                    action: Box::new(move |tx| {
                        tx.send(AppEvent::RollbackSessionForPromptEdit {
                            thread_id: selection.thread_id,
                            nth_user_message: selection.nth_user_message,
                            newer_user_messages: selection.newer_user_messages,
                            prompt: selection.prompt.clone(),
                        });
                    }),
                })
            })
            .collect::<Vec<_>>();
        items.push(RewindPromptItem {
            prompt: String::new(),
            code_summary: None,
            is_current: true,
            action: Box::new(|tx| tx.send(AppEvent::CancelBacktrackRestore)),
        });
        self.chat_widget
            .show_rewind_view(RewindViewParams::Prompts {
                view_id: BACKTRACK_MESSAGE_VIEW_ID,
                items,
                on_cancel: Box::new(|tx| tx.send(AppEvent::CancelBacktrackRestore)),
            });
        tui.frame_requester().schedule_frame();
    }

    pub(crate) fn show_backtrack_restore_picker(
        &mut self,
        selection: BacktrackSelection,
        target: BacktrackRollbackTarget,
        file_restore: BacktrackFileRestoreSummary,
    ) {
        let file_count = file_restore.restorable;
        let file_label = if file_count == 1 { "file" } else { "files" };
        let conversation_removed = "The selected message and everything after it will be removed.";
        let code_restored =
            format!("The code in {file_count} tracked {file_label} will be restored.");
        let mut options = Vec::new();
        if file_count > 0 {
            options.push(RewindRestoreOption {
                label: "Restore code and conversation".to_string(),
                details: vec![conversation_removed.into(), code_restored.clone().into()],
                action: backtrack_restore_action(
                    &selection,
                    &target,
                    crate::app_event::BacktrackRestoreMode::CodeAndConversation,
                ),
            });
        }
        options.push(RewindRestoreOption {
            label: "Restore conversation".to_string(),
            details: vec![
                conversation_removed.into(),
                "The code will be unchanged.".into(),
            ],
            action: backtrack_restore_action(
                &selection,
                &target,
                crate::app_event::BacktrackRestoreMode::Conversation,
            ),
        });
        if file_count > 0 {
            options.push(RewindRestoreOption {
                label: "Restore code".to_string(),
                details: vec![
                    "The conversation will be unchanged.".into(),
                    code_restored.into(),
                ],
                action: backtrack_restore_action(
                    &selection,
                    &target,
                    crate::app_event::BacktrackRestoreMode::Code,
                ),
            });
        }
        options.push(RewindRestoreOption {
            label: "Never mind".to_string(),
            details: vec!["The code and conversation will be unchanged.".into()],
            action: Box::new(|tx| tx.send(AppEvent::CancelBacktrackRestore)),
        });
        let footer_note = if file_restore.blocked == 0 {
            "Only changes made through Codex apply_patch are restored; shell and manual edits are left untouched."
                .to_string()
        } else {
            let blocked_file_label = if file_restore.blocked == 1 {
                "file"
            } else {
                "files"
            };
            format!(
                "Only safe Codex apply_patch changes are restored; {} conflicting or unavailable tracked {} will be left untouched.",
                file_restore.blocked, blocked_file_label
            )
        };
        self.chat_widget
            .show_rewind_view(RewindViewParams::Restore {
                view_id: BACKTRACK_RESTORE_VIEW_ID,
                prompt: selection.prompt.text,
                options,
                warning: footer_note.into(),
                on_cancel: Box::new(|tx| tx.send(AppEvent::BacktrackRestoreBack)),
            });
    }

    /// Apply a computed backtrack selection to the overlay and internal counter.
    pub(crate) fn apply_backtrack_selection_internal(&mut self, nth_user_message: usize) {
        if let Some(cell_idx) = nth_user_position(&self.transcript_cells, nth_user_message) {
            self.backtrack.nth_user_message = nth_user_message;
            if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                t.set_highlight_cell(Some(cell_idx));
            }
        } else {
            self.backtrack.nth_user_message = usize::MAX;
            if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                t.set_highlight_cell(/*cell*/ None);
            }
        }
    }

    /// Forwards an event to the overlay and closes it if done.
    ///
    /// The transcript overlay draw path is special because the overlay should match the main
    /// viewport while the active cell is still streaming or mutating.
    ///
    /// `TranscriptOverlay` owns committed transcript cells, while `ChatWidget` owns the current
    /// in-flight active cell (often a coalesced exec/tool group). During draws we append that
    /// in-flight cell as a cached, render-only live tail so `Ctrl+T` does not appear to "lose" tool
    /// calls until a later flush boundary.
    ///
    /// This logic lives here (instead of inside the overlay widget) because `ChatWidget` is the
    /// source of truth for the active cell and its cache invalidation key, and because `App` owns
    /// overlay lifecycle and frame scheduling for animations.
    fn overlay_forward_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        if matches!(
            &event,
            TuiEvent::Draw | TuiEvent::Resume | TuiEvent::Resize(_) | TuiEvent::FocusGained
        ) && let Some(Overlay::Transcript(t)) = &mut self.overlay
        {
            let active_key = self.chat_widget.active_cell_render_key();
            let chat_widget = &self.chat_widget;
            tui.draw(u16::MAX, |frame| {
                let width = frame.area().width.max(1);
                t.sync_live_tail(width, active_key, |w| {
                    chat_widget.active_cell_transcript_hyperlink_lines(w)
                });
                t.render(frame.area(), frame.buffer);
            })?;
            let close_overlay = t.is_done();
            if !close_overlay
                && active_key.is_some_and(|key| key.animation_tick.is_some())
                && t.is_scrolled_to_bottom()
            {
                tui.frame_requester()
                    .schedule_frame_in(std::time::Duration::from_millis(50));
            }
            if close_overlay {
                self.close_transcript_overlay(tui);
                tui.frame_requester().schedule_frame();
            }
            return Ok(());
        }

        if let Some(overlay) = &mut self.overlay {
            overlay.handle_event(tui, event)?;
            if overlay.is_done() {
                self.close_transcript_overlay(tui);
                tui.frame_requester().schedule_frame();
            }
        }
        Ok(())
    }

    /// Confirm a primed backtrack from the main view (no overlay visible).
    /// Computes the prompt state from the selected user message.
    #[cfg(test)]
    pub(crate) fn confirm_backtrack_from_main(&mut self) -> Option<BacktrackSelection> {
        let selection = self.backtrack_selection(self.backtrack.nth_user_message);
        self.reset_backtrack_state();
        selection
    }

    /// Clear all backtrack-related state and composer hints.
    pub(crate) fn reset_backtrack_state(&mut self) {
        self.backtrack.primed = false;
        self.backtrack.base_id = None;
        self.backtrack.nth_user_message = usize::MAX;
        self.backtrack.loading_older_history = false;
        // In case a hint is somehow still visible (e.g., race with overlay open/close).
        self.chat_widget.clear_esc_backtrack_hint();
    }

    fn backtrack_selection(&self, nth_user_message: usize) -> Option<BacktrackSelection> {
        let base_id = self.backtrack.base_id?;
        if self.chat_widget.thread_id() != Some(base_id) {
            return None;
        }

        let selected = nth_user_position(&self.transcript_cells, nth_user_message)
            .and_then(|idx| self.transcript_cells.get(idx))
            .and_then(|cell| cell.as_any().downcast_ref::<UserHistoryCell>())?;
        let newer_user_messages = user_count(&self.transcript_cells)
            .checked_sub(nth_user_message.checked_add(/*rhs*/ 1)?)?;

        Some(BacktrackSelection {
            thread_id: base_id,
            nth_user_message,
            newer_user_messages,
            prompt: user_message_from_history_cell(selected),
        })
    }

    pub(crate) fn backtrack_transcript_prompts(&self) -> Vec<UserMessage> {
        user_positions_iter(&self.transcript_cells)
            .filter_map(|index| {
                self.transcript_cells[index]
                    .as_any()
                    .downcast_ref::<UserHistoryCell>()
                    .map(user_message_from_history_cell)
            })
            .collect()
    }
}

fn user_message_from_history_cell(cell: &UserHistoryCell) -> UserMessage {
    UserMessage {
        text: cell.message.clone(),
        local_images: cell
            .local_image_paths
            .iter()
            .enumerate()
            .map(|(index, path)| LocalImageAttachment {
                placeholder: local_image_label_text(index + 1),
                path: path.clone(),
            })
            .collect(),
        remote_image_urls: cell.remote_image_urls.clone(),
        text_elements: cell.text_elements.clone(),
        mention_bindings: Vec::new(),
    }
}

fn backtrack_code_summary(cells: &[Arc<dyn crate::history_cell::HistoryCell>]) -> Line<'static> {
    let headings = cells
        .iter()
        .filter(|cell| cell.is_file_change())
        .filter_map(|cell| cell.raw_lines().into_iter().next())
        .collect::<Vec<_>>();
    match headings.as_slice() {
        [] => "No code changes".into(),
        [heading] => {
            let text = heading
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            text.strip_prefix("● ").unwrap_or(&text).to_string().into()
        }
        headings => format!("{} code changes", headings.len()).into(),
    }
}

fn backtrack_restore_action(
    selection: &BacktrackSelection,
    target: &BacktrackRollbackTarget,
    mode: crate::app_event::BacktrackRestoreMode,
) -> Box<dyn Fn(&crate::app_event_sender::AppEventSender) + Send + Sync> {
    let thread_id = selection.thread_id;
    let nth_user_message = selection.nth_user_message;
    let prompt = selection.prompt.clone();
    let target = target.clone();
    Box::new(move |tx| {
        tx.send(AppEvent::ApplyBacktrackRestore {
            thread_id,
            nth_user_message,
            target: target.clone(),
            prompt: prompt.clone(),
            mode,
        });
    })
}

pub(crate) fn user_count(cells: &[Arc<dyn crate::history_cell::HistoryCell>]) -> usize {
    user_positions_iter(cells).count()
}

fn has_backtrack_target(cells: &[Arc<dyn crate::history_cell::HistoryCell>]) -> bool {
    user_count(cells) > 0
}

pub(crate) fn nth_user_position(
    cells: &[Arc<dyn crate::history_cell::HistoryCell>],
    nth: usize,
) -> Option<usize> {
    user_positions_iter(cells)
        .enumerate()
        .find_map(|(i, idx)| (i == nth).then_some(idx))
}

fn user_positions_iter(
    cells: &[Arc<dyn crate::history_cell::HistoryCell>],
) -> impl Iterator<Item = usize> + '_ {
    let session_start_type = TypeId::of::<SessionInfoCell>();
    let user_type = TypeId::of::<UserHistoryCell>();
    let type_of = |cell: &Arc<dyn crate::history_cell::HistoryCell>| cell.as_any().type_id();

    let start = cells
        .iter()
        .rposition(|cell| type_of(cell) == session_start_type)
        .map_or(0, |idx| idx + 1);

    cells
        .iter()
        .enumerate()
        .skip(start)
        .filter_map(move |(idx, cell)| (type_of(cell) == user_type).then_some(idx))
}

#[cfg(test)]
fn agent_group_count(cells: &[Arc<dyn crate::history_cell::HistoryCell>]) -> usize {
    agent_group_positions_iter(cells).count()
}

#[cfg(test)]
fn agent_group_positions_iter(
    cells: &[Arc<dyn crate::history_cell::HistoryCell>],
) -> impl Iterator<Item = usize> + '_ {
    let session_start_type = TypeId::of::<SessionInfoCell>();
    let type_of = |cell: &Arc<dyn crate::history_cell::HistoryCell>| cell.as_any().type_id();

    let start = cells
        .iter()
        .rposition(|cell| type_of(cell) == session_start_type)
        .map_or(0, |idx| idx + 1);

    cells
        .iter()
        .enumerate()
        .skip(start)
        .filter_map(move |(idx, cell)| {
            let is_agent = cell.as_any().downcast_ref::<AgentMessageCell>().is_some();
            let is_copy_source_group = is_agent && !cell.is_stream_continuation();
            is_copy_source_group.then_some(idx)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use std::sync::Arc;

    fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn agent_group_count_ignores_context_compacted_marker() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(AgentMessageCell::new(
                vec![Line::from("first")],
                /*is_first_line*/ true,
            )) as Arc<dyn HistoryCell>,
            Arc::new(crate::history_cell::new_info_event(
                "Context compacted".to_string(),
                /*hint*/ None,
            )) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(
                vec![Line::from("second")],
                /*is_first_line*/ true,
            )) as Arc<dyn HistoryCell>,
        ];

        assert_eq!(agent_group_count(&cells), 2);
    }

    #[test]
    fn backtrack_target_requires_user_message() {
        let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(AgentMessageCell::new(
                vec![Line::from("assistant")],
                /*is_first_line*/ true,
            )) as Arc<dyn HistoryCell>,
            Arc::new(crate::history_cell::new_info_event(
                "Context compacted".to_string(),
                /*hint*/ None,
            )) as Arc<dyn HistoryCell>,
        ];

        assert!(!has_backtrack_target(&cells));

        cells.push(Arc::new(UserHistoryCell {
            message: "hello".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>);

        assert!(has_backtrack_target(&cells));
    }

    #[test]
    fn backtrack_unavailable_info_message_snapshot() {
        let cell = crate::history_cell::new_info_event(
            NO_PREVIOUS_MESSAGE_TO_EDIT.to_string(),
            /*hint*/ None,
        );
        let rendered = render_lines(&cell.display_lines(/*width*/ 80)).join("\n");

        insta::assert_snapshot!(rendered);
    }
}
