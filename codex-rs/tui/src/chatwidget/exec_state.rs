//! Unified exec bookkeeping state and helpers for `ChatWidget`.

use codex_app_server_protocol::CommandExecutionSource as ExecCommandSource;
use codex_protocol::parse_command::ParsedCommand;

use crate::exec_command::split_command_string;

use super::status_state::StatusIndicatorState;
use super::status_state::TerminalTitleStatusKind;

pub(super) struct RunningCommand {
    pub(super) command: Vec<String>,
    pub(super) parsed_cmd: Vec<ParsedCommand>,
    pub(super) source: ExecCommandSource,
}

pub(super) struct UnifiedExecProcessSummary {
    pub(super) key: String,
    pub(super) call_id: String,
    pub(super) command_display: String,
    pub(super) recent_chunks: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct UnifiedExecWaitStreak {
    pub(super) process_id: String,
    pub(super) previous_status: StatusIndicatorState,
    pub(super) previous_terminal_title_status_kind: TerminalTitleStatusKind,
}

impl UnifiedExecWaitStreak {
    pub(super) fn new(
        process_id: String,
        previous_status: StatusIndicatorState,
        previous_terminal_title_status_kind: TerminalTitleStatusKind,
    ) -> Self {
        Self {
            process_id,
            previous_status,
            previous_terminal_title_status_kind,
        }
    }
}

pub(super) fn is_unified_exec_source(source: ExecCommandSource) -> bool {
    matches!(
        source,
        ExecCommandSource::UnifiedExecStartup | ExecCommandSource::UnifiedExecInteraction
    )
}

pub(super) fn command_execution_command_and_parsed(
    command: &str,
    command_actions: &[codex_app_server_protocol::CommandAction],
) -> (Vec<String>, Vec<ParsedCommand>) {
    (
        split_command_string(command),
        command_actions
            .iter()
            .cloned()
            .map(codex_app_server_protocol::CommandAction::into_core)
            .collect(),
    )
}
