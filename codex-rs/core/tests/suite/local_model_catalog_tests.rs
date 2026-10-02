#![cfg(not(target_os = "windows"))]

use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_models_manager::bundled_models_response;
use codex_models_manager::model_info::BASE_INSTRUCTIONS;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_sandbox;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_update_plan_preserves_custom_catalog_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));
    const INSTRUCTIONS: &str = "## Plan tool\nNever deploy without explicit approval.\n";
    let server = start_mock_server().await;
    let response = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let mut catalog = bundled_models_response()?;
    let model = catalog
        .models
        .iter_mut()
        .find(|model| model.slug == "gpt-6-sol")
        .expect("bundled gpt-6-sol model");
    let messages = model
        .model_messages
        .as_mut()
        .expect("model prompt templates");
    messages.instructions_template = Some(INSTRUCTIONS.to_string());
    messages.instructions_variables = None;
    let test = test_codex()
        .with_model("gpt-6-sol")
        .with_config(move |config| {
            config.update_plan_enabled = false;
            config.model_catalog = Some(catalog);
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("hello").await?;
    let request = response.single_request().body_json();
    assert_eq!(request["instructions"], INSTRUCTIONS);
    assert!(!request["tools"].to_string().contains("update_plan"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_model_sends_builtin_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let mut builder = test_codex()
        .with_model("future-custom-model")
        .with_config(|config| config.update_plan_enabled = true);
    let test = builder.build_with_auto_env(&server).await?;

    test.submit_turn("use fallback model metadata").await?;

    let request = response.single_request();
    assert_eq!(request.instructions_text(), BASE_INSTRUCTIONS);
    let body = request.body_json();
    let tools = body["tools"]
        .as_array()
        .expect("fallback model tools should be present");
    for tool_name in ["exec_command", "write_stdin"] {
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"].as_str() == Some(tool_name)),
            "fallback model should expose {tool_name}: {tools:?}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn namespaced_model_slug_uses_catalog_metadata_without_fallback_warning() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let requested_model = "custom/gpt-6-sol";
    let response_mock = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;

    let TestCodex { codex, .. } = test_codex()
        .with_model(requested_model)
        .build_with_auto_env(&server)
        .await?;

    codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "check namespaced model metadata".into(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let mut fallback_warning_count = 0;
    loop {
        let event = wait_for_event(&codex, |_| true).await;
        match event {
            EventMsg::Warning(warning)
                if warning.message.contains("Defaulting to fallback metadata") =>
            {
                fallback_warning_count += 1;
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    let body = response_mock.single_request().body_json();
    assert_eq!(body["model"].as_str(), Some(requested_model));
    assert_eq!(fallback_warning_count, 0);

    Ok(())
}
