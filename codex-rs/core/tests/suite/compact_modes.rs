use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::protocol::CompactionMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::mount_compact_json_once;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexHarness;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;

#[test_case("vendor/DeepSeek-v4", None)]
#[test_case("moonshot/Kimi-K2", Some(CompactionMode::RemoteV1))]
#[test_case("zai/GLM-5", Some(CompactionMode::RemoteV2))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_weight_models_force_local_compaction(
    model: &str,
    requested_mode: Option<CompactionMode>,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let test = builder()
        .with_model(model)
        .with_config(|config| {
            config
                .features
                .disable(Feature::TokenBudget)
                .expect("disable token budget");
            config.model_provider.compact = Some(CompactionMode::RemoteV2);
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("summary", "LOCAL_SUMMARY"),
            ev_completed("compact-response"),
        ]),
    )
    .await;
    test.codex
        .submit(match requested_mode {
            Some(mode) => Op::CompactWithMode { mode },
            None => Op::Compact,
        })
        .await?;
    wait_for_turn_complete(&test.codex).await;
    let request = mock.single_request();
    assert_eq!(
        (
            request.path(),
            request.inputs_of_type("compaction_trigger"),
            request.body_json()["model"].clone()
        ),
        ("/v1/responses".to_string(), Vec::new(), json!(model))
    );
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text.contains("summary"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_weight_auto_compaction_uses_local_summary() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let test = builder()
        .with_model("moonshot/Kimi-K2")
        .with_config(|config| {
            config
                .features
                .disable(Feature::TokenBudget)
                .expect("disable token budget");
            config.model_provider.compact = Some(CompactionMode::RemoteV2);
            config.model_auto_compact_token_limit = Some(200_000);
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("first", "reply"),
                ev_completed_with_tokens("first-response", /*total_tokens*/ 330_000),
            ]),
            sse(vec![
                ev_assistant_message("summary", "LOCAL_SUMMARY"),
                ev_completed_with_tokens("summary-response", /*total_tokens*/ 200),
            ]),
            sse(vec![
                ev_assistant_message("last", "done"),
                ev_completed_with_tokens("last-response", /*total_tokens*/ 100),
            ]),
        ],
    )
    .await;
    test.submit_turn("first prompt").await?;
    test.submit_turn("follow-up prompt").await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1]
            .message_input_texts("user")
            .last()
            .map(String::as_str),
        Some(codex_core::compact::SUMMARIZATION_PROMPT)
    );
    assert!(requests[1].inputs_of_type("compaction_trigger").is_empty());
    Ok(())
}

async fn wait_for_turn_complete(codex: &codex_core::CodexThread) {
    wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
}

fn builder() -> core_test_support::test_codex::TestCodexBuilder {
    test_codex().with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_local_compaction_uses_responses_prompt() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(builder()).await?;
    let response_mock = mount_sse_once(
        harness.server(),
        sse(vec![
            ev_assistant_message("summary", "LOCAL_SUMMARY"),
            ev_completed("compact-response"),
        ]),
    )
    .await;

    harness
        .test()
        .codex
        .submit(Op::CompactWithMode {
            mode: CompactionMode::Local,
        })
        .await?;
    wait_for_turn_complete(&harness.test().codex).await;

    let request = response_mock.single_request();
    assert_eq!(request.path(), "/v1/responses");
    assert_eq!(request.inputs_of_type("compaction_trigger").len(), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_remote_v1_compaction_uses_compact_endpoint() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(builder()).await?;
    let compact_mock = mount_compact_json_once(
        harness.server(),
        json!({ "output": [{
            "type": "compaction",
            "encrypted_content": "REMOTE_V1_SUMMARY"
        }] }),
    )
    .await;
    let _response_mock = mount_sse_once(
        harness.server(),
        sse(vec![
            ev_assistant_message("seed", "SEED_REPLY"),
            ev_completed("seed-response"),
        ]),
    )
    .await;

    harness
        .test()
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "seed remote compaction history".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_turn_complete(&harness.test().codex).await;

    harness
        .test()
        .codex
        .submit(Op::CompactWithMode {
            mode: CompactionMode::RemoteV1,
        })
        .await?;
    wait_for_turn_complete(&harness.test().codex).await;

    assert_eq!(
        compact_mock.single_request().path(),
        "/v1/responses/compact"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_remote_v2_compaction_uses_compaction_trigger() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(builder()).await?;
    let response_mock = mount_sse_once(
        harness.server(),
        sse(vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "REMOTE_V2_SUMMARY"
                }
            }),
            ev_completed("compact-response"),
        ]),
    )
    .await;

    harness
        .test()
        .codex
        .submit(Op::CompactWithMode {
            mode: CompactionMode::RemoteV2,
        })
        .await?;
    wait_for_turn_complete(&harness.test().codex).await;

    let request = response_mock.single_request();
    assert_eq!(request.path(), "/v1/responses");
    assert_eq!(request.inputs_of_type("compaction_trigger").len(), 1);
    Ok(())
}
