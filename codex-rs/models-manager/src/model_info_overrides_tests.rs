use super::static_manager_for_tests;
use crate::ModelsManagerConfig;
use crate::manager::ModelsManager;
use codex_protocol::openai_models::TruncationMode;
use codex_protocol::openai_models::TruncationPolicyConfig;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_with_tool_output_override() {
    let config = ModelsManagerConfig {
        tool_output_token_limit: Some(123),
        ..Default::default()
    };
    let manager = static_manager_for_tests(
        crate::bundled_models_response().expect("bundled models should parse"),
    );

    // Pick a model that ships in the bundled catalog so the override takes the token
    // truncation branch instead of falling back to byte-based metadata.
    let slug = crate::bundled_models_response()
        .expect("bundled models.json should parse")
        .models
        .into_iter()
        .find(|model| model.truncation_policy.mode == TruncationMode::Tokens)
        .map(|model| model.slug)
        .expect("bundled catalog should contain a token-truncated model");

    let model_info = manager.get_model_info(&slug, &config).await;

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::tokens(/*limit*/ 123)
    );
}
