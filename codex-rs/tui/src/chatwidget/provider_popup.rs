//! Provider selection for the active thread.

use super::*;

impl ChatWidget {
    pub(crate) fn open_provider_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Provider selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let mut provider_ids = self
            .config
            .config_layer_stack
            .effective_user_config()
            .and_then(|config| {
                config
                    .get("model_providers")
                    .and_then(|providers| providers.as_table())
                    .map(|providers| {
                        providers
                            .keys()
                            .filter(|provider_id| {
                                self.config
                                    .model_providers
                                    .contains_key(provider_id.as_str())
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
            })
            .unwrap_or_default();
        provider_ids.sort();

        if provider_ids.is_empty() {
            self.add_info_message(
                "No model providers are configured in config.toml.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let current_provider_id = self.config.model_provider_id.as_str();
        let items = provider_ids
            .into_iter()
            .filter_map(|provider_id| {
                let provider = self.config.model_providers.get(&provider_id)?;
                let description = provider
                    .base_url
                    .as_deref()
                    .filter(|base_url| !base_url.is_empty())
                    .map_or_else(
                        || Some(provider.name.clone()),
                        |base_url| Some(format!("{} - {base_url}", provider.name)),
                    );
                let selected_provider_id = provider_id.clone();
                Some(SelectionItem {
                    name: provider_id.clone(),
                    description,
                    is_current: provider_id == current_provider_id,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::UpdateModelProvider(selected_provider_id.clone()));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                })
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Provider".bold()));
        header.push(Line::from(
            "Choose a provider configured in config.toml for this session.".dim(),
        ));
        self.bottom_pane.show_selection_view(SelectionViewParams {
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }
}
