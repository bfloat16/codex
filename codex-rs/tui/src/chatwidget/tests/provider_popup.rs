use super::*;
use pretty_assertions::assert_eq;

fn provider(name: &str, base_url: &str) -> ModelProviderInfo {
    ModelProviderInfo {
        name: name.to_string(),
        base_url: Some(base_url.to_string()),
        ..ModelProviderInfo::default()
    }
}

#[tokio::test]
async fn provider_popup_lists_only_user_configured_providers_and_selects_one() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());

    let alpha = provider("Alpha", "https://alpha.example/v1");
    let zeta = provider("Zeta", "https://zeta.example/v1");
    chat.config
        .model_providers
        .insert("alpha".to_string(), alpha);
    chat.config.model_providers.insert("zeta".to_string(), zeta);
    chat.config.model_provider_id = "alpha".to_string();

    let config_path = tempdir().expect("tempdir");
    let config_path = config_path.path().join("config.toml").abs();
    chat.config.config_layer_stack = ConfigLayerStack::default()
        .with_user_config(
            &config_path,
            toml::from_str::<TomlValue>(
                r#"
[model_providers.zeta]
name = "Zeta"
base_url = "https://zeta.example/v1"

[model_providers.alpha]
name = "Alpha"
base_url = "https://alpha.example/v1"
"#,
            )
            .expect("provider config"),
        )
        .expect("user config");

    chat.dispatch_command(SlashCommand::Provider);
    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(!popup.contains("openai"));
    assert!(popup.contains("alpha"));
    assert!(popup.contains("zeta"));
    assert_chatwidget_snapshot!("provider_popup", popup);

    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let selected_provider = loop {
        match rx.try_recv() {
            Ok(AppEvent::UpdateModelProvider(provider)) => break provider,
            Ok(_) => continue,
            Err(err) => panic!("expected provider selection event: {err}"),
        }
    };
    assert_eq!(selected_provider, "zeta");
}

#[tokio::test]
async fn provider_popup_reports_when_config_has_no_user_providers() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.open_provider_popup();

    assert!(!render_bottom_popup(&chat, /*width*/ 100).contains("Select Provider"));
    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected one info message");
    assert!(
        lines_to_single_string(&cells[0]).contains("No model providers are configured"),
        "expected missing-provider info message"
    );
}
