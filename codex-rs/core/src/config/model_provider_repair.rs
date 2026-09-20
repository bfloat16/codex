use codex_config::ConfigLayerStack;
use codex_protocol::openai_models::DEEPSEEK_PROVIDER_ID;
use codex_protocol::openai_models::is_deepseek_model;

pub(super) const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-flash";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelProviderCorrection {
    pub model: String,
    pub model_provider: String,
}

pub(super) fn user_config_correction(
    config_layer_stack: &ConfigLayerStack,
) -> Result<Option<ModelProviderCorrection>, String> {
    let Some(config) = config_layer_stack.effective_user_config() else {
        return Ok(None);
    };
    let model = config.get("model").and_then(toml::Value::as_str);
    let model_provider = config
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .unwrap_or("openai");
    let configured_provider_ids = config
        .get("model_providers")
        .and_then(toml::Value::as_table)
        .map(|providers| providers.keys().cloned().collect())
        .unwrap_or_default();

    correction(model, model_provider, configured_provider_ids)
}

pub(super) fn correction(
    model: Option<&str>,
    model_provider: &str,
    configured_provider_ids: Vec<String>,
) -> Result<Option<ModelProviderCorrection>, String> {
    let model_was_missing = model.is_none();
    let model = match model {
        Some(model) => model,
        None if model_provider == DEEPSEEK_PROVIDER_ID => DEFAULT_DEEPSEEK_MODEL,
        None => return Ok(None),
    };

    let corrected_provider = if is_deepseek_model(model) {
        if model_provider == DEEPSEEK_PROVIDER_ID {
            return Ok(model_was_missing.then_some(ModelProviderCorrection {
                model: model.to_string(),
                model_provider: model_provider.to_string(),
            }));
        }
        if !configured_provider_ids
            .iter()
            .any(|provider_id| provider_id == DEEPSEEK_PROVIDER_ID)
        {
            return Err(format!(
                "Model provider `{DEEPSEEK_PROVIDER_ID}` required by model `{model}` was not found"
            ));
        }
        DEEPSEEK_PROVIDER_ID.to_string()
    } else if model_provider == DEEPSEEK_PROVIDER_ID {
        let mut provider_ids = configured_provider_ids;
        provider_ids.sort();
        provider_ids.dedup();
        provider_ids
            .into_iter()
            .find(|provider_id| provider_id != DEEPSEEK_PROVIDER_ID)
            .ok_or_else(|| {
                format!("No non-DeepSeek model provider is configured for model `{model}`")
            })?
    } else {
        return Ok(None);
    };

    Ok(Some(ModelProviderCorrection {
        model: model.to_string(),
        model_provider: corrected_provider,
    }))
}

#[cfg(test)]
#[path = "model_provider_repair_tests.rs"]
mod tests;
