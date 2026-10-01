use anyhow::Result;
use codex_core::CodexThreadSettingsOverrides;
use codex_core::TurnInputRequest;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse_completed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use test_case::test_case;

#[test_case("gpt-6-sol", &["gpt-6-astra"], &["vendor/DeepSeek-v4", "moonshot/Kimi-K2", "zai/GLM-5"])]
#[test_case("vendor/DeepSeek-v4", &["moonshot/Kimi-K2", "zai/GLM-5"], &["gpt-6-sol"])]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_locks_model_family_on_first_prompt_and_allows_provider_changes(
    first_model: &str,
    same_family: &[&str],
    other_family: &[&str],
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let base_url = format!("{}/v1", server.uri());
    let test = test_codex()
        .with_pre_build_hook(move |home| {
            std::fs::write(
                home.join("config.toml"),
                format!(
                    r#"
model_provider = "deepseek"
[model_providers.deepseek]
name = "DeepSeek"
base_url = "{base_url}"
[model_providers.alpha]
name = "Alpha"
base_url = "{base_url}"
"#
                ),
            )
            .expect("provider config");
        })
        .with_config(|config| {
            config
                .model_providers
                .insert("openai".to_string(), config.model_provider.clone());
            config.model_provider = config.model_providers["deepseek"].clone();
        })
        .build_with_auto_env(&server)
        .await?;

    for model in ["moonshot/Kimi-K2", "gpt-6-sol", first_model] {
        test.codex
            .restore_thread_settings(CodexThreadSettingsOverrides {
                model: Some(model.to_string()),
                ..Default::default()
            })
            .await?;
        let config = test.codex.config_snapshot().await;
        assert_eq!(
            (config.model.as_str(), config.model_provider_id.as_str()),
            (model, "deepseek")
        );
    }

    let mock = mount_sse_once(&server, sse_completed("first-prompt")).await;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "lock this model family".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.single_request().body_json()["model"], first_model);

    for provider in ["alpha", "openai", "deepseek"] {
        test.codex
            .restore_thread_settings(CodexThreadSettingsOverrides {
                model_provider: Some(provider.to_string()),
                ..Default::default()
            })
            .await?;
        let config = test.codex.config_snapshot().await;
        assert_eq!(
            (config.model.as_str(), config.model_provider_id.as_str()),
            (first_model, provider)
        );
    }
    for model in same_family {
        test.codex
            .restore_thread_settings(CodexThreadSettingsOverrides {
                model: Some(model.to_string()),
                ..Default::default()
            })
            .await?;
    }
    for model in other_family {
        let error = test
            .codex
            .preview_thread_settings_overrides(CodexThreadSettingsOverrides {
                model: Some(model.to_string()),
                ..Default::default()
            })
            .await
            .expect_err("first prompt must lock the model family");
        assert!(error.to_string().contains("same model family"));
    }
    Ok(())
}
