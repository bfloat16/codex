use anyhow::Result;
use codex_core::CodexThreadSettingsOverrides;
use codex_core::TurnInputRequest;
use codex_core::config::AgentRoleConfig;
use codex_core::config::Config;
use codex_features::Feature;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_completed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
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

const GPT_PARENT: &str = "gpt-parent";
const GPT_WORKER: &str = "gpt-worker";
const OPEN_PARENT: &str = "vendor/DeepSeek-parent";
const OPEN_WORKER: &str = "moonshot/Kimi-worker";

fn configure_subagent_model_catalog(config: &mut Config, version: MultiAgentVersion) {
    config
        .features
        .enable(Feature::Collab)
        .expect("enable collaboration");
    config
        .features
        .disable(Feature::ToolSearch)
        .expect("disable deferred tool loading");
    config.multi_agent_v2.hide_spawn_agent_metadata = false;
    config.multi_agent_v2.expose_spawn_agent_model_overrides = true;
    config.model_catalog = Some(ModelsResponse {
        models: [
            GPT_PARENT,
            GPT_WORKER,
            "gpt-extra-1",
            "gpt-extra-2",
            "gpt-extra-3",
            "gpt-extra-4",
            "gpt-extra-5",
            "gpt-extra-6",
            "gpt-extra-7",
            OPEN_PARENT,
            OPEN_WORKER,
        ]
        .into_iter()
        .enumerate()
        .map(|(priority, slug)| {
            let mut model = model_info_from_slug(slug);
            model.description = Some(format!(
                "Worker model {slug}. API prices (USD/1M tokens): input $1; cached input $0.10; cache write $1.25; output $2."
            ));
            model.visibility = ModelVisibility::List;
            model.priority = priority as i32;
            model.default_reasoning_level = Some(ReasoningEffort::High);
            model.supported_reasoning_levels = vec![ReasoningEffortPreset {
                effort: ReasoningEffort::High,
                description: "Deep reasoning".to_string(),
            }];
            model.tool_mode = Some(ToolMode::Direct);
            model.multi_agent_version = Some(version);
            model
        })
        .collect(),
    });
}

fn request_body_json(request: &wiremock::Request) -> Option<serde_json::Value> {
    let body = match request
        .headers
        .get("content-encoding")
        .and_then(|encoding| encoding.to_str().ok())
    {
        Some(encoding) if encoding.eq_ignore_ascii_case("zstd") => {
            zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()?
        }
        _ => request.body.clone(),
    };
    serde_json::from_slice(&body).ok()
}

#[test_case(GPT_PARENT, MultiAgentVersion::V1; "v1 GPT family")]
#[test_case(OPEN_PARENT, MultiAgentVersion::V1; "v1 open-weight family")]
#[test_case(GPT_PARENT, MultiAgentVersion::V2; "v2 GPT family")]
#[test_case(OPEN_PARENT, MultiAgentVersion::V2; "v2 open-weight family")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_model_descriptions_follow_parent_family(
    parent_model: &str,
    version: MultiAgentVersion,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model(parent_model)
        .with_config(move |config| configure_subagent_model_catalog(config, version));
    let test = builder.build_with_auto_env(&server).await?;
    let mock = mount_sse_once(&server, sse_completed("model-list")).await;

    test.submit_turn("list the subagent models").await?;

    let namespace = match version {
        MultiAgentVersion::V1 => "multi_agent_v1",
        MultiAgentVersion::V2 => "collaboration",
        MultiAgentVersion::Disabled => unreachable!("test requires collaboration"),
    };
    let spawn_tool = mock
        .single_request()
        .tool_by_name(namespace, "spawn_agent")
        .expect("spawn tool should be exposed");
    let description = spawn_tool["description"]
        .as_str()
        .expect("spawn tool should have a description");
    let models = description
        .lines()
        .filter_map(|line| line.strip_prefix("- `"))
        .filter_map(|line| line.split_once("`: Worker model "))
        .map(|(model, _)| model)
        .collect::<Vec<_>>();
    let expected = if parent_model == GPT_PARENT {
        vec![
            GPT_PARENT,
            GPT_WORKER,
            "gpt-extra-1",
            "gpt-extra-2",
            "gpt-extra-3",
            "gpt-extra-4",
            "gpt-extra-5",
            "gpt-extra-6",
        ]
    } else {
        vec![OPEN_PARENT, OPEN_WORKER]
    };
    assert_eq!(models, expected);
    let summaries = description
        .lines()
        .filter(|line| line.starts_with("- `"))
        .filter_map(|line| line.split_once(" Reasoning efforts:"))
        .map(|(summary, _)| summary.to_string())
        .collect::<Vec<_>>();
    let expected_summaries = expected
        .iter()
        .map(|model| {
            format!(
                "- `{model}`: Worker model {model}. API prices (USD/1M tokens): input $1; cached input $0.10; cache write $1.25; output $2."
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(summaries, expected_summaries);
    Ok(())
}

#[derive(Clone, Copy)]
enum SubagentModelSelection {
    Explicit,
    ConfiguredDefault,
    Role,
    DefaultRole,
}

#[test_case(GPT_PARENT, OPEN_WORKER, SubagentModelSelection::Explicit, "none"; "GPT rejects explicit open-weight model")]
#[test_case(OPEN_PARENT, GPT_WORKER, SubagentModelSelection::Explicit, "none"; "open-weight rejects explicit GPT model")]
#[test_case(GPT_PARENT, OPEN_WORKER, SubagentModelSelection::ConfiguredDefault, "none"; "GPT rejects open-weight default")]
#[test_case(OPEN_PARENT, GPT_WORKER, SubagentModelSelection::ConfiguredDefault, "none"; "open-weight rejects GPT default")]
#[test_case(GPT_PARENT, OPEN_WORKER, SubagentModelSelection::Role, "none"; "GPT rejects open-weight role")]
#[test_case(OPEN_PARENT, GPT_WORKER, SubagentModelSelection::Role, "none"; "open-weight rejects GPT role")]
#[test_case(GPT_PARENT, OPEN_WORKER, SubagentModelSelection::DefaultRole, "none"; "GPT rejects open-weight default role")]
#[test_case(OPEN_PARENT, GPT_WORKER, SubagentModelSelection::DefaultRole, "none"; "open-weight rejects GPT default role")]
#[test_case(GPT_PARENT, OPEN_WORKER, SubagentModelSelection::Explicit, "all"; "full GPT fork rejects open-weight model")]
#[test_case(OPEN_PARENT, GPT_WORKER, SubagentModelSelection::Explicit, "all"; "full open-weight fork rejects GPT model")]
#[test_case(GPT_PARENT, OPEN_WORKER, SubagentModelSelection::Explicit, "1"; "partial GPT fork rejects open-weight model")]
#[test_case(OPEN_PARENT, GPT_WORKER, SubagentModelSelection::Explicit, "1"; "partial open-weight fork rejects GPT model")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_model_selection_rejects_other_family(
    parent_model: &str,
    child_model: &str,
    selection: SubagentModelSelection,
    fork_turns: &str,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let child_model = child_model.to_string();
    let configured_child_model = child_model.clone();
    let mut builder = test_codex()
        .with_model(parent_model)
        .with_config(move |config| {
            configure_subagent_model_catalog(config, MultiAgentVersion::V2);
            match selection {
                SubagentModelSelection::Explicit => {}
                SubagentModelSelection::ConfiguredDefault => {
                    config.agent_default_subagent_model = Some(configured_child_model);
                }
                SubagentModelSelection::Role | SubagentModelSelection::DefaultRole => {
                    let role_path = config.codex_home.join("worker.toml");
                    std::fs::write(&role_path, format!("model = {configured_child_model:?}\n"))
                        .expect("write worker role");
                    let role_name = match selection {
                        SubagentModelSelection::Role => "worker",
                        SubagentModelSelection::DefaultRole => "default",
                        SubagentModelSelection::Explicit
                        | SubagentModelSelection::ConfiguredDefault => unreachable!(),
                    };
                    config.agent_roles.insert(
                        role_name.to_string(),
                        AgentRoleConfig {
                            description: Some("Worker role".to_string()),
                            config_file: Some(role_path.to_path_buf()),
                            nickname_candidates: None,
                        },
                    );
                }
            }
        });
    let test = builder.build_with_auto_env(&server).await?;
    let mut created_threads = test.thread_manager.subscribe_thread_created();
    let mut arguments = json!({"task_name": "worker", "message": "inspect the repository"});
    match selection {
        SubagentModelSelection::Explicit => arguments["model"] = json!(child_model),
        SubagentModelSelection::Role => arguments["agent_type"] = json!("worker"),
        SubagentModelSelection::ConfiguredDefault | SubagentModelSelection::DefaultRole => {}
    }
    if fork_turns != "none" {
        arguments["fork_turns"] = json!(fork_turns);
    }
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("spawn"),
                ev_function_call_with_namespace(
                    "spawn-worker",
                    "collaboration",
                    "spawn_agent",
                    &arguments.to_string(),
                ),
                ev_completed("spawn"),
            ]),
            sse_completed("rejected"),
        ],
    )
    .await;

    test.submit_turn("spawn the worker").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let output = requests[1]
        .function_call_output_text("spawn-worker")
        .expect("spawn error should be returned to the parent");
    let expected = match selection {
        SubagentModelSelection::Explicit | SubagentModelSelection::ConfiguredDefault => {
            "Unknown model"
        }
        SubagentModelSelection::Role | SubagentModelSelection::DefaultRole => "same model family",
    };
    assert!(
        output.contains(expected),
        "unexpected spawn result: {output}"
    );
    assert!(
        created_threads.try_recv().is_err(),
        "no child should be created"
    );
    Ok(())
}

#[test_case(GPT_PARENT, GPT_WORKER; "GPT family")]
#[test_case(OPEN_PARENT, OPEN_WORKER; "open-weight family")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_model_selection_allows_same_family(
    parent_model: &str,
    child_model: &str,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model(parent_model)
        .with_config(|config| configure_subagent_model_catalog(config, MultiAgentVersion::V2));
    let test = builder.build_with_auto_env(&server).await?;
    let mut created_threads = test.thread_manager.subscribe_thread_created();
    let initial_model = parent_model.to_string();
    let followup_model = initial_model.clone();
    let worker_model = child_model.to_string();
    let arguments = json!({
        "task_name": "worker",
        "message": "complete the worker task",
        "model": child_model,
    });
    let initial = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request_body_json(request).is_some_and(|body| {
                body["model"] == initial_model
                    && body["input"].as_array().is_some_and(|input| {
                        input.iter().all(|item| item["call_id"] != "spawn-worker")
                    })
            })
        },
        sse(vec![
            ev_response_created("spawn"),
            ev_function_call_with_namespace(
                "spawn-worker",
                "collaboration",
                "spawn_agent",
                &arguments.to_string(),
            ),
            ev_completed("spawn"),
        ]),
    )
    .await;
    let followup = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request_body_json(request).is_some_and(|body| {
                body["model"] == followup_model
                    && body["input"].as_array().is_some_and(|input| {
                        input.iter().any(|item| item["call_id"] == "spawn-worker")
                    })
            })
        },
        sse_completed("parent-complete"),
    )
    .await;
    let worker = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request_body_json(request).is_some_and(|body| body["model"] == worker_model)
        },
        sse_completed("worker-complete"),
    )
    .await;

    test.submit_turn("spawn the worker").await?;
    let child_id = created_threads.recv().await?;
    let child = test.thread_manager.get_thread(child_id).await?;
    wait_for_event(&child, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    assert_eq!(
        json!([
            initial.single_request().body_json()["model"],
            followup.single_request().body_json()["model"],
            worker.single_request().body_json()["model"],
        ]),
        json!([parent_model, parent_model, child_model])
    );
    Ok(())
}
