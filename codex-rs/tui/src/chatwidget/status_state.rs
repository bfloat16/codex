//! Status indicator and terminal-title state for `ChatWidget`.

use crate::status_indicator_widget::ModelTransferPhase;
use crate::status_indicator_widget::ModelTransferStatus;
use crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompactionStatusKind {
    Local,
    RemoteV1,
    RemoteV2,
}

impl CompactionStatusKind {
    pub(super) fn header(self) -> &'static str {
        match self {
            Self::Local => "Compacting locally",
            Self::RemoteV1 => "Compacting remotely (v1)",
            Self::RemoteV2 => "Compacting remotely (v2)",
        }
    }
}

#[derive(Debug, Default)]
struct ModelTransferAccumulator {
    phase: Option<ModelTransferPhase>,
    completed_sent_bytes: u64,
    completed_received_bytes: u64,
    request_sent_bytes: u64,
    request_received_bytes: u64,
}

impl ModelTransferAccumulator {
    fn update(
        &mut self,
        phase: ModelTransferPhase,
        sent: u64,
        received: u64,
    ) -> ModelTransferStatus {
        if matches!(phase, ModelTransferPhase::Sending) {
            if self.phase.is_some() {
                self.completed_sent_bytes = self
                    .completed_sent_bytes
                    .saturating_add(self.request_sent_bytes);
                self.completed_received_bytes = self
                    .completed_received_bytes
                    .saturating_add(self.request_received_bytes);
            }
            self.request_sent_bytes = 0;
            self.request_received_bytes = 0;
        } else {
            self.request_sent_bytes = sent;
            self.request_received_bytes = received;
        }
        self.phase = Some(phase);
        ModelTransferStatus {
            phase,
            sent_bytes: self
                .completed_sent_bytes
                .saturating_add(self.request_sent_bytes),
            received_bytes: self
                .completed_received_bytes
                .saturating_add(self.request_received_bytes),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StatusIndicatorState {
    pub(super) header: String,
    pub(super) details: Option<String>,
    pub(super) details_max_lines: usize,
}

impl StatusIndicatorState {
    pub(super) fn working() -> Self {
        Self {
            header: String::from("Working"),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
        }
    }

    pub(super) fn is_guardian_review(&self) -> bool {
        self.header == "Reviewing approval request" || self.header.starts_with("Reviewing ")
    }
}

/// Compact runtime states that can be rendered into the terminal title.
///
/// This is intentionally smaller than the full status-header vocabulary. The
/// title needs short, stable labels, so callers map richer lifecycle events
/// onto one of these buckets before rendering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TerminalTitleStatusKind {
    Working,
    WaitingForBackgroundTerminal,
    #[default]
    Thinking,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PendingGuardianReviewStatus {
    entries: Vec<PendingGuardianReviewStatusEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingGuardianReviewStatusEntry {
    id: String,
    detail: String,
}

impl PendingGuardianReviewStatus {
    pub(super) fn start_or_update(&mut self, id: String, detail: String) {
        if let Some(existing) = self.entries.iter_mut().find(|entry| entry.id == id) {
            existing.detail = detail;
        } else {
            self.entries
                .push(PendingGuardianReviewStatusEntry { id, detail });
        }
    }

    pub(super) fn finish(&mut self, id: &str) -> bool {
        let original_len = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() != original_len
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // Guardian review status is derived from the full set of currently pending
    // review entries. The generic status cache on `ChatWidget` stores whichever
    // footer is currently rendered; this helper computes the guardian-specific
    // footer snapshot that should replace it while reviews remain in flight.
    pub(super) fn status_indicator_state(&self) -> Option<StatusIndicatorState> {
        let details = if self.entries.len() == 1 {
            self.entries.first().map(|entry| entry.detail.clone())
        } else if self.entries.is_empty() {
            None
        } else {
            let mut lines = self
                .entries
                .iter()
                .take(3)
                .map(|entry| format!("• {}", entry.detail))
                .collect::<Vec<_>>();
            let remaining = self.entries.len().saturating_sub(3);
            if remaining > 0 {
                lines.push(format!("+{remaining} more"));
            }
            Some(lines.join("\n"))
        };
        let details = details?;
        let header = if self.entries.len() == 1 {
            String::from("Reviewing approval request")
        } else {
            format!("Reviewing {} approval requests", self.entries.len())
        };
        let details_max_lines = if self.entries.len() == 1 { 1 } else { 4 };
        Some(StatusIndicatorState {
            header,
            details: Some(details),
            details_max_lines,
        })
    }
}

#[derive(Debug)]
pub(super) struct StatusState {
    pub(super) compaction: Option<super::compaction::ActiveCompaction>,
    pub(super) current_status: StatusIndicatorState,
    pub(super) active_compaction: Option<CompactionStatusKind>,
    pub(super) pre_compaction_status: Option<StatusIndicatorState>,
    pub(super) pending_guardian_review_status: PendingGuardianReviewStatus,
    waiting_status: Option<StatusIndicatorState>,
    pub(super) terminal_title_status_kind: TerminalTitleStatusKind,
    pub(super) retry_status_header: Option<String>,
    pub(super) pending_status_indicator_restore: bool,
    pub(super) thread_title_generation_pending: bool,
    model_transfer: ModelTransferAccumulator,
}

impl Default for StatusState {
    fn default() -> Self {
        Self {
            compaction: None,
            current_status: StatusIndicatorState::working(),
            active_compaction: None,
            pre_compaction_status: None,
            pending_guardian_review_status: PendingGuardianReviewStatus::default(),
            waiting_status: None,
            terminal_title_status_kind: TerminalTitleStatusKind::Working,
            retry_status_header: None,
            pending_status_indicator_restore: false,
            thread_title_generation_pending: false,
            model_transfer: ModelTransferAccumulator::default(),
        }
    }
}

impl StatusState {
    pub(super) fn set_status(&mut self, status: StatusIndicatorState) {
        if self.waiting_status.is_some() && status.header != "Waiting" {
            self.waiting_status = Some(status);
        } else if self.waiting_status.is_none() {
            self.current_status = status;
        }
    }

    pub(super) fn begin_waiting(&mut self) -> StatusIndicatorState {
        if self.waiting_status.is_none() {
            self.waiting_status = Some(self.current_status.clone());
        }
        self.current_status = StatusIndicatorState {
            header: String::from("Waiting"),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
        };
        self.current_status.clone()
    }

    pub(super) fn finish_waiting(&mut self) -> Option<StatusIndicatorState> {
        let previous = self.waiting_status.take()?;
        self.current_status = previous.clone();
        Some(previous)
    }

    pub(super) fn begin_compaction(&mut self, kind: CompactionStatusKind) {
        if self.active_compaction.is_none() {
            self.pre_compaction_status = Some(self.current_status.clone());
        }
        self.active_compaction = Some(kind);
    }

    pub(super) fn finish_compaction(&mut self) -> Option<StatusIndicatorState> {
        self.active_compaction = None;
        self.pre_compaction_status.take()
    }

    pub(super) fn take_retry_status_header(&mut self) -> Option<String> {
        self.retry_status_header.take()
    }

    pub(super) fn remember_retry_status_header(&mut self) {
        if self.retry_status_header.is_none() {
            self.retry_status_header = Some(self.current_status.header.clone());
        }
    }

    pub(super) fn reset_model_transfer(&mut self) {
        self.model_transfer = ModelTransferAccumulator::default();
    }

    pub(super) fn update_model_transfer(
        &mut self,
        phase: ModelTransferPhase,
        sent: u64,
        received: u64,
    ) -> ModelTransferStatus {
        self.model_transfer.update(phase, sent, received)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn guardian_status_aggregates_parallel_reviews() {
        let mut state = PendingGuardianReviewStatus::default();
        state.start_or_update("a".to_string(), "first".to_string());
        state.start_or_update("b".to_string(), "second".to_string());

        assert_eq!(
            state.status_indicator_state(),
            Some(StatusIndicatorState {
                header: "Reviewing 2 approval requests".to_string(),
                details: Some("• first\n• second".to_string()),
                details_max_lines: 4,
            })
        );
    }

    #[test]
    fn retry_status_header_is_taken_once() {
        let mut state = StatusState::default();
        state.current_status.header = "Thinking".to_string();

        state.remember_retry_status_header();

        assert_eq!(
            state.take_retry_status_header(),
            Some("Thinking".to_string())
        );
        assert_eq!(state.take_retry_status_header(), None);
    }

    #[test]
    fn waiting_status_restores_updates_after_all_waiters_finish() {
        let mut state = StatusState::default();
        state.current_status.header = "Thinking".to_string();

        assert_eq!(state.begin_waiting().header, "Waiting");
        state.set_status(StatusIndicatorState {
            header: "Checking files".to_string(),
            details: None,
            details_max_lines: 1,
        });
        assert_eq!(state.current_status.header, "Waiting");
        assert_eq!(
            state.finish_waiting(),
            Some(StatusIndicatorState {
                header: "Checking files".to_string(),
                details: None,
                details_max_lines: 1,
            })
        );
        assert_eq!(state.current_status.header, "Checking files");
    }
}
