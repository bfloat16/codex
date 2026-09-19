use super::App;
use super::ThreadBufferedEvent;
use crate::app_backtrack::BacktrackFileRestoreSummary;
use crate::app_backtrack::BacktrackRollbackTarget;
use crate::app_backtrack::BacktrackSelection;
use crate::app_backtrack::PendingBacktrackRollback;
use crate::app_backtrack::backtrack_rollback_target;
use crate::app_event::BacktrackRestoreMode;
use crate::app_server_session::AppServerSession;
use crate::chatwidget::UserMessage;
use crate::tui;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadFileChangeDisposition;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;

pub(super) struct BacktrackRestoreRequest {
    pub(super) thread_id: ThreadId,
    pub(super) nth_user_message: usize,
    pub(super) target: BacktrackRollbackTarget,
    pub(super) prompt: UserMessage,
    pub(super) mode: BacktrackRestoreMode,
}

impl App {
    pub(super) async fn handle_backtrack_rollback_request(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        selection: BacktrackSelection,
        transcript_prompts: std::sync::Arc<[UserMessage]>,
    ) -> Result<()> {
        let thread_id = selection.thread_id;
        let newer_user_messages = selection.newer_user_messages;
        let mut prompt = selection.prompt.clone();
        if self.chat_widget.thread_id() != Some(thread_id) {
            return Ok(());
        }
        if self.backtrack.pending_rollback.is_none() {
            self.backtrack.pending_rollback = Some(PendingBacktrackRollback {
                selection: selection.clone(),
            });
        }
        let rollback_target = match self.thread_event_channels.get(&thread_id) {
            Some(channel) => {
                let store = channel.store.lock().await;
                let mut turns = store.turns.clone();
                for event in &store.buffer {
                    let ThreadBufferedEvent::Notification(notification) = event else {
                        continue;
                    };
                    match notification.as_ref() {
                        ServerNotification::TurnStarted(notification)
                            if !turns.iter().any(|turn| turn.id == notification.turn.id) =>
                        {
                            turns.push(notification.turn.clone());
                        }
                        ServerNotification::ItemCompleted(notification) => {
                            if matches!(
                                notification.item,
                                ThreadItem::UserMessage { .. }
                                    | ThreadItem::EnteredReviewMode { .. }
                                    | ThreadItem::ExitedReviewMode { .. }
                            ) && let Some(turn) = turns
                                .iter_mut()
                                .find(|turn| turn.id == notification.turn_id)
                                && !turn
                                    .items
                                    .iter()
                                    .any(|item| item.id() == notification.item.id())
                            {
                                turn.items.push(notification.item.clone());
                            }
                        }
                        ServerNotification::TurnCompleted(notification) => {
                            if let Some(turn) = turns
                                .iter_mut()
                                .find(|turn| turn.id == notification.turn.id)
                            {
                                turn.status = notification.turn.status.clone();
                                turn.error = notification.turn.error.clone();
                                turn.started_at = notification.turn.started_at;
                                turn.completed_at = notification.turn.completed_at;
                                turn.duration_ms = notification.turn.duration_ms;
                            }
                        }
                        _ => {}
                    }
                }
                backtrack_rollback_target(
                    &turns,
                    transcript_prompts.as_ref(),
                    newer_user_messages,
                    &mut prompt,
                )
            }
            None => Err(color_eyre::eyre::eyre!(
                "the selected thread is no longer available for prompt editing"
            )),
        };
        let rollback_target = match rollback_target {
            Ok(target) => Ok(target),
            Err(_) => {
                let refreshed_thread = match app_server
                    .thread_read(thread_id, /*include_turns*/ false)
                    .await
                {
                    Ok(mut thread) => match app_server
                        .hydrate_initial_thread_history(
                            &mut thread,
                            /*turn_cursor*/ None,
                            /*item_cursor*/ None,
                            /*config*/ None,
                            /*local_settings*/ None,
                            crate::app_server_session::HistoryHydrationScope::Complete,
                        )
                        .await
                    {
                        Ok(()) => Ok(thread),
                        Err(err) => Err(err),
                    },
                    Err(err) => Err(err),
                };
                match refreshed_thread {
                    Ok(thread) => backtrack_rollback_target(
                        &thread.turns,
                        transcript_prompts.as_ref(),
                        newer_user_messages,
                        &mut prompt,
                    ),
                    Err(err) => Err(err),
                }
            }
        };
        match rollback_target {
            Ok(target) => {
                let file_restore = match app_server
                    .thread_file_change_read(thread_id, target.before_turn_id.clone())
                    .await
                {
                    Ok(response) => {
                        let restorable = response
                            .data
                            .iter()
                            .filter(|change| {
                                change.disposition == ThreadFileChangeDisposition::Restorable
                            })
                            .count();
                        BacktrackFileRestoreSummary {
                            restorable,
                            blocked: response.data.len().saturating_sub(restorable),
                        }
                    }
                    Err(err) => {
                        tracing::warn!("failed to preview tracked file restore: {err}");
                        BacktrackFileRestoreSummary::default()
                    }
                };
                self.show_backtrack_restore_picker(selection, target, file_restore);
            }
            Err(err) => {
                self.handle_backtrack_rollback_failed();
                self.restore_backtrack_prompt_after_rollback_error(prompt, err);
            }
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(super) async fn handle_apply_backtrack_restore(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        request: BacktrackRestoreRequest,
    ) -> Result<()> {
        let BacktrackRestoreRequest {
            thread_id,
            nth_user_message,
            target,
            prompt,
            mode,
        } = request;
        if self.chat_widget.thread_id() != Some(thread_id) {
            self.handle_backtrack_rollback_failed();
            return Ok(());
        }
        let restore_files = matches!(
            mode,
            BacktrackRestoreMode::CodeAndConversation | BacktrackRestoreMode::Code
        );
        let restore_conversation = matches!(
            mode,
            BacktrackRestoreMode::CodeAndConversation | BacktrackRestoreMode::Conversation
        );
        let file_restore = if restore_files {
            Some(
                app_server
                    .thread_file_change_restore(thread_id, target.before_turn_id.clone())
                    .await,
            )
        } else {
            None
        };

        if restore_conversation {
            match app_server
                .truncate_thread_before_turn(
                    thread_id,
                    target.before_turn_id.clone(),
                    target.legacy_num_turns,
                )
                .await
            {
                Ok(history_mode) => {
                    if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                        let mut store = channel.store.lock().await;
                        let boundary_loaded = store.remove_reverted_turns(
                            &target.before_turn_id,
                            &target.removed_turn_ids,
                        );
                        if history_mode == ThreadHistoryMode::Paginated && boundary_loaded {
                            app_server.mark_thread_history_complete(thread_id);
                        }
                    }
                    self.scrollback_has_older_history = app_server.has_older_history(thread_id);
                    if let Err(err) = app_server
                        .thread_file_change_discard(thread_id, target.before_turn_id.clone())
                        .await
                    {
                        tracing::warn!(
                            "failed to discard checkpoints after conversation rewind: {err}"
                        );
                    }
                    self.chat_widget.restore_user_message_to_composer(prompt);
                    self.handle_backtrack_rollback_succeeded(nth_user_message);
                }
                Err(err) => {
                    self.handle_backtrack_rollback_failed();
                    self.restore_backtrack_prompt_after_rollback_error(prompt, err);
                }
            }
        } else {
            self.handle_backtrack_rollback_failed();
            self.reset_backtrack_state();
        }

        if let Some(file_restore) = file_restore {
            match file_restore {
                Ok(response) => {
                    let restored = response.restored.len();
                    let skipped = response.skipped.len();
                    let failed = response.failed.len();
                    let message = if skipped == 0 && failed == 0 {
                        format!("Restored {restored} tracked files.")
                    } else {
                        format!(
                            "Restored {restored} tracked files; skipped {skipped}; failed {failed}."
                        )
                    };
                    self.chat_widget.add_info_message(message, /*hint*/ None);
                }
                Err(err) => self
                    .chat_widget
                    .add_error_message(format!("Failed to restore tracked files: {err}")),
            }
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }
}
