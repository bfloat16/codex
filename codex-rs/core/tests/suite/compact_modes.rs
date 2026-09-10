use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_login::CodexAuth;
use codex_protocol::protocol::CompactionMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_compact_json_once;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexHarness;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::json;

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
