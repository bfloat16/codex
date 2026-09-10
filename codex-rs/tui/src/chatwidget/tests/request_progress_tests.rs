use super::*;
use codex_app_server_protocol::ModelRequestProgressNotification;
use codex_app_server_protocol::ModelRequestProgressPhase;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn request_progress_preserves_retry_error_until_model_output() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    handle_turn_started(&mut chat, "turn-1");

    for attempt in 1..=2 {
        handle_stream_error(
            &mut chat,
            format!("Reconnecting... {attempt}/5"),
            Some("Connection reset by peer".to_string()),
        );
        let expected = chat.status_state.current_status.clone();
        for (phase, sent_bytes, received_bytes) in [
            (ModelRequestProgressPhase::Sending, 0, 0),
            (ModelRequestProgressPhase::Sending, 1024, 0),
            (ModelRequestProgressPhase::Receiving, 1024, 128),
        ] {
            chat.handle_server_notification(
                ServerNotification::ModelRequestProgress(ModelRequestProgressNotification {
                    thread_id: "thread-1".to_string(),
                    turn_id: "turn-1".to_string(),
                    phase,
                    sent_bytes,
                    received_bytes,
                }),
                /*replay_kind*/ None,
            );
            assert_eq!(chat.status_state.current_status, expected);
            assert_eq!(
                chat.status_state.retry_status_header.as_deref(),
                Some("Working")
            );
        }
    }

    assert_chatwidget_snapshot!(
        "request_progress_preserves_retry_error",
        render_bottom_popup(&chat, /*width*/ 80)
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join("\n"),
    );

    handle_agent_message_delta(&mut chat, "Recovered");
    assert_eq!(
        chat.status_state.current_status,
        StatusIndicatorState::working()
    );
    assert_eq!(chat.status_state.retry_status_header, None);
}
