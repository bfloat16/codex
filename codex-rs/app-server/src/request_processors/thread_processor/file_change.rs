use super::ThreadRequestProcessor;
use super::ensure_direct_input_allowed;
use crate::error_code::internal_error;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadFileChange;
use codex_app_server_protocol::ThreadFileChangeDiscardParams;
use codex_app_server_protocol::ThreadFileChangeDiscardResponse;
use codex_app_server_protocol::ThreadFileChangeDisposition;
use codex_app_server_protocol::ThreadFileChangeKind;
use codex_app_server_protocol::ThreadFileChangeReadParams;
use codex_app_server_protocol::ThreadFileChangeReadResponse;
use codex_app_server_protocol::ThreadFileChangeRestoreParams;
use codex_app_server_protocol::ThreadFileChangeRestoreResponse;
use codex_file_checkpoint::FileRestoreChangeKind;
use codex_file_checkpoint::FileRestoreDisposition;
use codex_file_checkpoint::FileRestorePreviewEntry;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_file_change_read(
        &self,
        params: ThreadFileChangeReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let ThreadFileChangeReadParams {
            thread_id,
            before_turn_id,
        } = params;
        let (_, thread) = self.load_thread(&thread_id).await?;
        ensure_direct_input_allowed(thread.as_ref()).await?;
        let preview = thread
            .preview_file_restore(&before_turn_id)
            .await
            .map_err(|err| internal_error(format!("failed to preview file restore: {err}")))?;
        Ok(Some(
            ThreadFileChangeReadResponse {
                data: preview
                    .files
                    .into_iter()
                    .map(file_change_from_core)
                    .collect(),
            }
            .into(),
        ))
    }

    pub(crate) async fn thread_file_change_restore(
        &self,
        params: ThreadFileChangeRestoreParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let ThreadFileChangeRestoreParams {
            thread_id,
            before_turn_id,
        } = params;
        let (_, thread) = self.load_thread(&thread_id).await?;
        ensure_direct_input_allowed(thread.as_ref()).await?;
        let outcome = thread
            .restore_files_before_turn(&before_turn_id)
            .await
            .map_err(|err| internal_error(format!("failed to restore files: {err}")))?;
        Ok(Some(
            ThreadFileChangeRestoreResponse {
                restored: outcome
                    .restored
                    .into_iter()
                    .map(file_change_from_core)
                    .collect(),
                skipped: outcome
                    .skipped
                    .into_iter()
                    .map(file_change_from_core)
                    .collect(),
                failed: outcome
                    .failed
                    .into_iter()
                    .map(file_change_from_core)
                    .collect(),
            }
            .into(),
        ))
    }

    pub(crate) async fn thread_file_change_discard(
        &self,
        params: ThreadFileChangeDiscardParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let ThreadFileChangeDiscardParams {
            thread_id,
            before_turn_id,
        } = params;
        let (_, thread) = self.load_thread(&thread_id).await?;
        ensure_direct_input_allowed(thread.as_ref()).await?;
        thread
            .discard_file_checkpoints_from_turn(&before_turn_id)
            .await
            .map_err(|err| internal_error(format!("failed to discard file checkpoints: {err}")))?;
        Ok(Some(ThreadFileChangeDiscardResponse {}.into()))
    }
}

fn file_change_from_core(entry: FileRestorePreviewEntry) -> ThreadFileChange {
    ThreadFileChange {
        environment_id: entry.environment_id,
        path: entry.path,
        change_kind: match entry.change_kind {
            FileRestoreChangeKind::Create => ThreadFileChangeKind::Create,
            FileRestoreChangeKind::Update => ThreadFileChangeKind::Update,
            FileRestoreChangeKind::Delete => ThreadFileChangeKind::Delete,
        },
        disposition: match entry.disposition {
            FileRestoreDisposition::Restorable => ThreadFileChangeDisposition::Restorable,
            FileRestoreDisposition::Conflict => ThreadFileChangeDisposition::Conflict,
            FileRestoreDisposition::Unavailable => ThreadFileChangeDisposition::Unavailable,
        },
        detail: entry.detail,
    }
}
