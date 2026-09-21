use super::session_lifecycle_requests::start_recording_app_server;
use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn interrupt_after_server_completion_releases_pending_request_without_warning() -> Result<()>
{
    let mut app = make_test_app().await;
    let (mut server, _requests, proxy) = start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let started = server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    let channel = app.ensure_thread_channel(thread_id);
    let store = channel.store.clone();
    store
        .lock()
        .await
        .set_active_turn_id("already-finished".into());

    assert!(
        app.try_submit_active_thread_op_via_app_server(
            &mut server,
            thread_id,
            &AppCommand::Interrupt,
        )
        .await?
    );
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            if store.lock().await.pending_interrupt_turn_id.is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    // The queued terminal notification remains responsible for finishing the visible turn.
    assert_eq!(
        store.lock().await.active_turn_id(),
        Some("already-finished")
    );
    if let Some(receiver) = app.active_thread_rx.as_mut() {
        while let Ok(event) = receiver.try_recv() {
            if let ThreadBufferedEvent::Notification(notification) = event {
                assert!(!matches!(*notification, ServerNotification::Warning(_)));
            }
        }
    }
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}
