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

use std::any::TypeId;
use std::sync::Arc;

use crate::app::App;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionRowDisplay;
use crate::bottom_pane::SelectionViewParams;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::UserMessage;
use crate::chatwidget::mention_bindings_from_user_inputs;
#[cfg(test)]
use crate::history_cell::AgentMessageCell;
use crate::history_cell::SessionInfoCell;
use crate::history_cell::UserHistoryCell;
use crate::pager_overlay::Overlay;
use crate::pager_overlay::TranscriptHistoryState;
use crate::tui;
use crate::tui::TuiEvent;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use codex_protocol::models::local_image_label_text;
use color_eyre::eyre::Result;
use color_eyre::eyre::bail;

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
    fn open_backtrack_message_picker(&mut self, tui: &mut tui::Tui) {
        if !has_backtrack_target(&self.transcript_cells) {
            self.reset_backtrack_state();
            self.chat_widget
                .add_info_message(NO_PREVIOUS_MESSAGE_TO_EDIT.to_string(), /*hint*/ None);
            tui.frame_requester().schedule_frame();
            return;
        }
        self.chat_widget.clear_esc_backtrack_hint();
        let count = user_count(&self.transcript_cells);
        let items = (0..count)
            .filter_map(|nth_user_message| {
                let selection = self.backtrack_selection(nth_user_message)?;
                let name = selection
                    .prompt
                    .text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                Some(SelectionItem {
                    name,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::RollbackSessionForPromptEdit {
                            thread_id: selection.thread_id,
                            nth_user_message: selection.nth_user_message,
                            newer_user_messages: selection.newer_user_messages,
                            prompt: selection.prompt.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                })
            })
            .collect();
        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some(BACKTRACK_MESSAGE_VIEW_ID),
            title: Some("Rewind".to_string()),
            subtitle: Some("Restore code and/or conversation to the point before...".to_string()),
            items,
            initial_selected_idx: count.checked_sub(1),
            row_display: SelectionRowDisplay::SingleLine,
            on_cancel: Some(Box::new(|tx| tx.send(AppEvent::CancelBacktrackRestore))),
            ..Default::default()
        });
        tui.frame_requester().schedule_frame();
    }

    pub(crate) fn show_backtrack_restore_picker(
        &mut self,
        selection: BacktrackSelection,
        target: BacktrackRollbackTarget,
        file_count: usize,
    ) {
        let mut options = Vec::new();
        if file_count > 0 {
            options.push((
                "Restore code and conversation",
                format!("Restore {file_count} tracked files and remove later messages"),
                Some(crate::app_event::BacktrackRestoreMode::CodeAndConversation),
            ));
        }
        options.push((
            "Restore conversation",
            if file_count > 0 {
                "Keep tracked file changes and remove later messages".to_string()
            } else {
                "Remove this message and every later message".to_string()
            },
            Some(crate::app_event::BacktrackRestoreMode::Conversation),
        ));
        if file_count > 0 {
            options.push((
                "Restore code",
                format!("Restore {file_count} tracked files and keep the conversation"),
                Some(crate::app_event::BacktrackRestoreMode::Code),
            ));
        }
        options.push((
            "Never mind",
            "Leave the code and conversation unchanged".to_string(),
            None,
        ));
        let items = options
            .into_iter()
            .map(|(name, description, mode)| {
                let prompt = selection.prompt.clone();
                let target = target.clone();
                SelectionItem {
                    name: name.to_string(),
                    description: Some(description),
                    actions: vec![Box::new(move |tx| match mode {
                        Some(mode) => tx.send(AppEvent::ApplyBacktrackRestore {
                            thread_id: selection.thread_id,
                            nth_user_message: selection.nth_user_message,
                            target: target.clone(),
                            prompt: prompt.clone(),
                            mode,
                        }),
                        None => tx.send(AppEvent::CancelBacktrackRestore),
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some(BACKTRACK_RESTORE_VIEW_ID),
            title: Some("Rewind".to_string()),
            subtitle: Some("Choose what to restore.".to_string()),
            footer_note: Some(
                "Only changes made through Codex apply_patch are restored; shell and manual edits are left untouched."
                    .into(),
            ),
            items,
            initial_selected_idx: Some(0),
            on_cancel: Some(Box::new(|tx| tx.send(AppEvent::CancelBacktrackRestore))),
            ..Default::default()
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
        let local_images = selected
            .local_image_paths
            .iter()
            .enumerate()
            .map(|(index, path)| LocalImageAttachment {
                placeholder: local_image_label_text(index + 1),
                path: path.clone(),
            })
            .collect();
        let newer_user_messages = user_count(&self.transcript_cells)
            .checked_sub(nth_user_message.checked_add(/*rhs*/ 1)?)?;

        Some(BacktrackSelection {
            thread_id: base_id,
            nth_user_message,
            newer_user_messages,
            prompt: UserMessage {
                text: selected.message.clone(),
                local_images,
                remote_image_urls: selected.remote_image_urls.clone(),
                text_elements: selected.text_elements.clone(),
                mention_bindings: Vec::new(),
            },
        })
    }
}

/// Find the persisted turn that contains a selected transcript prompt and calculate the rollback.
///
/// Replay hides review prompts and other display-empty inputs, so the selected distance from the
/// end must be resolved against the same visible projection before restoring its canonical mention
/// bindings. Counting from the end keeps the selection stable when the TUI has loaded only a suffix
/// of the persisted history or has inserted a newer session header.
///
/// A turn can contain multiple user messages when it was steered. Only its initial prompt can be
/// reopened independently because app-server cannot roll back in the middle of a turn.
pub(crate) fn backtrack_rollback_target(
    turns: &[Turn],
    newer_user_messages: usize,
    prompt: &mut UserMessage,
) -> Result<BacktrackRollbackTarget> {
    let mut visible_user_messages = Vec::new();
    let mut review_mode = false;
    for (turn_index, turn) in turns.iter().enumerate() {
        let hidden_nested_review_turn = turn_index
            .checked_sub(/*rhs*/ 1)
            .and_then(|index| turns.get(index))
            .is_some_and(|previous| is_hidden_nested_review_turn(previous, turn));
        let mut user_messages_in_turn = 0_usize;
        for item in &turn.items {
            let content = match item {
                ThreadItem::EnteredReviewMode { .. } => {
                    review_mode = true;
                    continue;
                }
                ThreadItem::ExitedReviewMode { .. } => {
                    review_mode = false;
                    continue;
                }
                ThreadItem::UserMessage { content, .. } => content,
                _ => continue,
            };
            let is_steer = user_messages_in_turn > 0;
            user_messages_in_turn = user_messages_in_turn.saturating_add(/*rhs*/ 1);
            if review_mode {
                continue;
            }

            let display = ChatWidget::user_message_display_from_inputs(content);
            if hidden_nested_review_turn {
                continue;
            }
            if display.message.trim().is_empty()
                && display.text_elements.is_empty()
                && display.local_images.is_empty()
                && display.remote_image_urls.is_empty()
            {
                continue;
            }
            visible_user_messages.push((turn_index, is_steer, content));
        }
    }

    let Some(&(turn_index, is_steer, content)) =
        visible_user_messages.iter().rev().nth(newer_user_messages)
    else {
        bail!("the selected prompt was not found in the persisted thread");
    };
    let turn = &turns[turn_index];
    let display = ChatWidget::user_message_display_from_inputs(content);
    let selected_local_images = prompt.local_images.iter().map(|image| &image.path);
    if prompt.text != display.message
        || prompt.text_elements != display.text_elements
        || prompt.remote_image_urls != display.remote_image_urls
        || !selected_local_images.eq(display.local_images.iter())
    {
        bail!("the selected transcript prompt no longer matches the persisted thread");
    }
    if is_steer {
        bail!("the selected prompt is a steer and cannot be rolled back independently");
    }
    if matches!(turn.status, TurnStatus::InProgress) {
        bail!("the selected prompt belongs to a turn that is still in progress");
    }

    prompt.mention_bindings = mention_bindings_from_user_inputs(content, &display.message);
    let legacy_num_turns = turns[turn_index..]
        .iter()
        .flat_map(|turn| &turn.items)
        .filter(|item| matches!(item, ThreadItem::UserMessage { .. }))
        .count();
    let Ok(legacy_num_turns) = u32::try_from(legacy_num_turns) else {
        bail!("the selected prompt requires rolling back too many turns");
    };
    Ok(BacktrackRollbackTarget {
        before_turn_id: turn.id.clone(),
        legacy_num_turns,
    })
}

/// Returns whether a turn is the reconstructed inline-review child with duplicated prompt inputs.
pub(crate) fn is_hidden_nested_review_turn(previous: &Turn, turn: &Turn) -> bool {
    if previous.status != TurnStatus::Completed
        || turn.status != TurnStatus::Interrupted
        || turn.completed_at.is_some()
        || !previous
            .items
            .iter()
            .any(|item| matches!(item, ThreadItem::EnteredReviewMode { .. }))
        || !previous
            .items
            .iter()
            .any(|item| matches!(item, ThreadItem::ExitedReviewMode { .. }))
    {
        return false;
    }

    let mut user_messages = turn.items.iter().filter_map(|item| match item {
        ThreadItem::UserMessage { content, .. } => Some(content),
        _ => None,
    });
    matches!(
        (
            user_messages.next(),
            user_messages.next(),
            user_messages.next(),
        ),
        (Some(first), Some(second), None) if first == second
    )
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
    use crate::bottom_pane::MentionBinding;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use codex_app_server_protocol::UserInput;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use std::path::PathBuf;
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

    fn turn(turn_id: &str, status: TurnStatus, user_messages: usize) -> Turn {
        Turn {
            id: turn_id.to_string(),
            items: (0..user_messages)
                .map(|index| ThreadItem::UserMessage {
                    id: format!("user-{index}"),
                    client_id: None,
                    content: vec![UserInput::Text {
                        text: format!("{turn_id}-prompt-{index}"),
                        text_elements: Vec::new(),
                    }],
                })
                .collect(),
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            status,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }
    }

    fn prompt(text: &str) -> UserMessage {
        UserMessage {
            text: text.to_string(),
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        }
    }

    #[test]
    fn backtrack_rollback_target_resolves_first_and_later_prompts() {
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            turn(
                "turn-compaction",
                TurnStatus::Completed,
                /*user_messages*/ 0,
            ),
            turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1),
        ];

        assert_eq!(
            backtrack_rollback_target(
                &turns,
                /*newer_user_messages*/ 1,
                &mut prompt("turn-1-prompt-0"),
            )
            .expect("first prompt should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-1".to_string(),
                legacy_num_turns: 2,
            }
        );
        assert_eq!(
            backtrack_rollback_target(
                &turns,
                /*newer_user_messages*/ 0,
                &mut prompt("turn-2-prompt-0"),
            )
            .expect("later prompt should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
            }
        );
    }

    #[test]
    fn backtrack_rollback_target_resolves_failed_compact_after_earlier_steers() {
        let mut earlier_compact = turn(
            "turn-earlier-compact",
            TurnStatus::Completed,
            /*user_messages*/ 1,
        );
        let ThreadItem::UserMessage { content, .. } = &mut earlier_compact.items[0] else {
            panic!("expected user message")
        };
        *content = vec![UserInput::Text {
            text: "/compact".to_string(),
            text_elements: Vec::new(),
        }];
        let mut failed_compact = turn(
            "turn-failed-compact",
            TurnStatus::Failed,
            /*user_messages*/ 1,
        );
        let ThreadItem::UserMessage { content, .. } = &mut failed_compact.items[0] else {
            panic!("expected user message")
        };
        *content = vec![UserInput::Text {
            text: "/compact".to_string(),
            text_elements: Vec::new(),
        }];
        let turns = vec![
            turn(
                "turn-with-steers",
                TurnStatus::Interrupted,
                /*user_messages*/ 3,
            ),
            earlier_compact,
            turn(
                "turn-after-compact",
                TurnStatus::Completed,
                /*user_messages*/ 1,
            ),
            failed_compact,
        ];

        assert_eq!(
            backtrack_rollback_target(
                &turns,
                /*newer_user_messages*/ 0,
                &mut prompt("/compact"),
            )
            .expect("latest failed compact should resolve independently of the history prefix"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-failed-compact".to_string(),
                legacy_num_turns: 1,
            }
        );
    }

    #[test]
    fn backtrack_rollback_target_rejects_mid_turn_steers() {
        let turns = vec![turn(
            "turn-1",
            TurnStatus::Completed,
            /*user_messages*/ 2,
        )];

        let error = backtrack_rollback_target(
            &turns,
            /*newer_user_messages*/ 0,
            &mut prompt("turn-1-prompt-1"),
        )
        .expect_err("a steer cannot be rolled back independently");

        assert_eq!(
            error.to_string(),
            "the selected prompt is a steer and cannot be rolled back independently"
        );
    }

    #[test]
    fn backtrack_rollback_target_rejects_in_progress_and_missing_prompts() {
        let turns = vec![turn(
            "turn-1",
            TurnStatus::InProgress,
            /*user_messages*/ 1,
        )];

        assert_eq!(
            backtrack_rollback_target(
                &turns,
                /*newer_user_messages*/ 0,
                &mut prompt("turn-1-prompt-0"),
            )
            .expect_err("in-progress prompt cannot be rolled back")
            .to_string(),
            "the selected prompt belongs to a turn that is still in progress"
        );
        assert_eq!(
            backtrack_rollback_target(
                &turns,
                /*newer_user_messages*/ 1,
                &mut prompt("missing prompt"),
            )
            .expect_err("missing prompt cannot be rolled back")
            .to_string(),
            "the selected prompt was not found in the persisted thread"
        );

        let completed_turns = vec![turn(
            "turn-1",
            TurnStatus::Completed,
            /*user_messages*/ 1,
        )];
        assert_eq!(
            backtrack_rollback_target(
                &completed_turns,
                /*newer_user_messages*/ 0,
                &mut prompt("different prompt"),
            )
            .expect_err("a stale transcript prompt cannot be rolled back")
            .to_string(),
            "the selected transcript prompt no longer matches the persisted thread"
        );
    }

    #[test]
    fn backtrack_rollback_target_skips_hidden_review_prompts() {
        let mut review_turn = turn(
            "turn-review",
            TurnStatus::Completed,
            /*user_messages*/ 1,
        );
        review_turn.items.insert(
            /*index*/ 0,
            ThreadItem::EnteredReviewMode {
                id: "review-start".to_string(),
                review: "changes against main".to_string(),
            },
        );
        review_turn.items.push(ThreadItem::ExitedReviewMode {
            id: "review-end".to_string(),
            review: "review complete".to_string(),
        });
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            review_turn,
            turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1),
        ];

        assert_eq!(
            backtrack_rollback_target(
                &turns,
                /*newer_user_messages*/ 0,
                &mut prompt("turn-2-prompt-0"),
            )
            .expect("the visible prompt after review should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
            }
        );
    }

    #[test]
    fn backtrack_rollback_target_skips_hidden_nested_review_prompts() {
        let review_hint = "current changes";
        let review_prompt =
            "Review the current code changes (staged, unstaged, and untracked files).";
        let review_turn = Turn {
            items: vec![
                ThreadItem::EnteredReviewMode {
                    id: "review-start".to_string(),
                    review: review_hint.to_string(),
                },
                ThreadItem::ExitedReviewMode {
                    id: "review-end".to_string(),
                    review: "review complete".to_string(),
                },
            ],
            ..turn(
                "turn-review",
                TurnStatus::Completed,
                /*user_messages*/ 0,
            )
        };
        let review_child_turn = Turn {
            items: (0..2)
                .map(|index| ThreadItem::UserMessage {
                    id: format!("review-prompt-{index}"),
                    client_id: None,
                    content: vec![UserInput::Text {
                        text: review_prompt.to_string(),
                        text_elements: Vec::new(),
                    }],
                })
                .collect(),
            ..turn(
                "turn-review-child",
                TurnStatus::Interrupted,
                /*user_messages*/ 0,
            )
        };
        let interrupted_steered_turn = Turn {
            items: review_child_turn.items.clone(),
            completed_at: Some(1),
            ..turn(
                "turn-interrupted-steer",
                TurnStatus::Interrupted,
                /*user_messages*/ 0,
            )
        };
        assert!(!is_hidden_nested_review_turn(
            &review_turn,
            &interrupted_steered_turn,
        ));
        let turns = vec![
            review_turn,
            review_child_turn,
            turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1),
        ];

        assert_eq!(
            backtrack_rollback_target(
                &turns,
                /*newer_user_messages*/ 0,
                &mut prompt("turn-2-prompt-0"),
            )
            .expect("the visible prompt after a nested review should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
            }
        );
    }

    #[test]
    fn backtrack_rollback_target_restores_canonical_mention_bindings() {
        let mut selected_turn = turn("turn-2", TurnStatus::Completed, /*user_messages*/ 1);
        selected_turn.items = vec![ThreadItem::UserMessage {
            id: "selected-prompt".to_string(),
            client_id: None,
            content: vec![
                UserInput::Text {
                    text: "use $skill @sample $google-calendar".to_string(),
                    text_elements: Vec::new(),
                },
                UserInput::Skill {
                    name: "skill".to_string(),
                    path: PathBuf::from("/tmp/skills/skill/SKILL.md"),
                },
                UserInput::Mention {
                    name: "Sample Plugin".to_string(),
                    path: "plugin://sample@test".to_string(),
                },
                UserInput::Mention {
                    name: "Google Calendar".to_string(),
                    path: "app://google_calendar".to_string(),
                },
            ],
        }];
        let turns = vec![
            turn("turn-1", TurnStatus::Completed, /*user_messages*/ 1),
            selected_turn,
        ];
        let mut selected_prompt = prompt("use $skill @sample $google-calendar");

        assert_eq!(
            backtrack_rollback_target(&turns, /*newer_user_messages*/ 0, &mut selected_prompt,)
                .expect("the selected prompt should resolve"),
            BacktrackRollbackTarget {
                before_turn_id: "turn-2".to_string(),
                legacy_num_turns: 1,
            }
        );
        assert_eq!(
            selected_prompt.mention_bindings,
            vec![
                MentionBinding {
                    sigil: '$',
                    mention: "skill".to_string(),
                    path: "/tmp/skills/skill/SKILL.md".to_string(),
                },
                MentionBinding {
                    sigil: '@',
                    mention: "sample".to_string(),
                    path: "plugin://sample@test".to_string(),
                },
                MentionBinding {
                    sigil: '$',
                    mention: "google-calendar".to_string(),
                    path: "app://google_calendar".to_string(),
                },
            ]
        );
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
