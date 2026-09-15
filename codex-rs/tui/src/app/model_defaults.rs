//! Saved model-default feedback, independent of active thread settings.
//!
//! A successful config write can still be overridden. Report that distinction without
//! replacing the active task's explicit selection with launch-time configuration.

use super::App;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ConfigEdit;
use color_eyre::eyre::Result;

impl App {
    pub(super) async fn persist_model_defaults(
        &mut self,
        request_handle: AppServerRequestHandle,
        edits: Vec<ConfigEdit>,
        _setting: &str,
    ) -> Result<()> {
        crate::config_update::write_config_batch(request_handle, edits).await?;
        Ok(())
    }
}
