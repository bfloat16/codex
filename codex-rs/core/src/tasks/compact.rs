use std::sync::Arc;

use super::SessionTask;
use super::SessionTaskResult;
use super::emit_compact_metric;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use codex_features::Feature;
use codex_model_provider::RemoteCompactionSupport;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::CompactionMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Default)]
pub(crate) struct CompactTask {
    pub(crate) mode: Option<CompactionMode>,
}

impl SessionTask for CompactTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }

    fn span_name(&self) -> &'static str {
        "session_task.compact"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        _cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let _profile_guard = ctx.turn_timing_state.begin_compaction();
        let start_event = EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: ctx.sub_id.clone(),
            trace_id: ctx.trace_id.clone(),
            started_at: ctx.turn_timing_state.started_at_unix_secs().await,
            model_context_window: ctx.model_context_window(),
            collaboration_mode_kind: ctx.mode(),
        });
        session.send_event(&ctx, start_event).await;
        let command = match self.mode {
            Some(CompactionMode::Local) => "/compact local",
            Some(CompactionMode::RemoteV1) => "/compact remotev1",
            Some(CompactionMode::RemoteV2) => "/compact remotev2",
            None => "/compact",
        };
        crate::compact::record_manual_compact_command(&session, &ctx, command).await;

        let mode = self.mode.or(ctx.provider.info().compact);
        let support = match mode {
            Some(CompactionMode::Local) => RemoteCompactionSupport::Unsupported,
            Some(CompactionMode::RemoteV1) => RemoteCompactionSupport::V1,
            Some(CompactionMode::RemoteV2) => RemoteCompactionSupport::V2,
            None => ctx.provider.capabilities().remote_compaction,
        };
        if ctx.config.features.enabled(Feature::TokenBudget)
            && !matches!(
                mode,
                Some(CompactionMode::RemoteV1 | CompactionMode::RemoteV2)
            )
        {
            crate::compact_token_budget::run_manual_compact_task(session, ctx).await?;
            return Ok(None);
        }

        let result = match support {
            RemoteCompactionSupport::V2
                if ctx.config.features.enabled(Feature::RemoteCompactionV2)
                    || mode == Some(CompactionMode::RemoteV2) =>
            {
                emit_compact_metric(
                    &session.services.session_telemetry,
                    "remote_v2",
                    /*manual*/ true,
                );
                crate::compact_remote_v2::run_remote_compact_task(session.clone(), ctx).await
            }
            RemoteCompactionSupport::V2 | RemoteCompactionSupport::V1 => {
                emit_compact_metric(
                    &session.services.session_telemetry,
                    "remote",
                    /*manual*/ true,
                );
                crate::compact_remote::run_remote_compact_task(session.clone(), ctx).await
            }
            RemoteCompactionSupport::Unsupported => {
                emit_compact_metric(
                    &session.services.session_telemetry,
                    "local",
                    /*manual*/ true,
                );
                let input = vec![UserInput::Text {
                    text: ctx
                        .config
                        .compact_prompt
                        .as_deref()
                        .unwrap_or(crate::compact::SUMMARIZATION_PROMPT)
                        .to_string(),
                    // Compaction prompt is synthesized; no UI element ranges to preserve.
                    text_elements: Vec::new(),
                }];
                crate::compact::run_compact_task(session.clone(), ctx, input).await
            }
        };
        if let Err(err) = result
            && matches!(err.details(), CodexErrorDetails::TurnAborted)
        {
            return Err(err);
        }
        Ok(None)
    }
}
