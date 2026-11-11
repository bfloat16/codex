use super::*;
use crate::app_event::BacktrackRestoreMode;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ThreadRevertedNotification;
use pretty_assertions::assert_eq;

enum RewindDriveUntil {
    Output,
    Interrupted,
    Reverted,
}

async fn drive_rewind_until(
    app: &mut App,
    tui: &mut crate::tui::Tui,
    server: &mut AppServerSession,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    until: RewindDriveUntil,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            let event = server.next_event().await.expect("app-server event stream");
            let reached = match (&until, &event) {
                (RewindDriveUntil::Output, codex_app_server_client::AppServerEvent::ServerNotification(notification)) => matches!(notification.as_ref(), ServerNotification::AgentMessageDelta(_) | ServerNotification::ReasoningSummaryTextDelta(_)),
                (RewindDriveUntil::Interrupted, codex_app_server_client::AppServerEvent::ServerNotification(notification)) => matches!(notification.as_ref(), ServerNotification::TurnCompleted(notification) if notification.turn.status == TurnStatus::Interrupted),
                (RewindDriveUntil::Reverted, codex_app_server_client::AppServerEvent::ServerNotification(notification)) => matches!(notification.as_ref(), ServerNotification::ThreadReverted(_)),
                _ => false,
            };
            app.handle_app_server_event(server, event).await;
            app.drain_active_thread_events_until(tui, Instant::now() + Duration::from_secs(/*secs*/ 1)).await?;
            while let Ok(event) = events.try_recv() {
                app.handle_event(tui, server, event).await?;
            }
            if reached {
                return Ok(());
            }
        }
    }).await?
}

#[test]
fn repeated_esc_and_picker_rewind_allow_new_streaming_turns() -> Result<()> {
    use core_test_support::responses;
    use core_test_support::streaming_sse::StreamingSseChunk;
    use core_test_support::streaming_sse::start_streaming_sse_server;

    rewind_test_runtime()?.block_on(Box::pin(async {
        let modes = [
            FirstRewind::Automatic,
            FirstRewind::Automatic,
            FirstRewind::Picker,
            FirstRewind::Picker,
        ];
        let mut releases = Vec::new();
        let chunks = modes
            .iter()
            .enumerate()
            .map(|(index, mode)| {
                let response_id = format!("response-{index}");
                let (item, output) = match mode {
                    FirstRewind::Automatic => (
                        responses::ev_reasoning_item_added(&response_id, &[]),
                        responses::ev_reasoning_summary_text_delta("Thinking through the prompt"),
                    ),
                    FirstRewind::Picker => (
                        responses::ev_message_item_added(&response_id, ""),
                        responses::ev_output_text_delta("Partial answer"),
                    ),
                };
                let (release, gate) = tokio::sync::oneshot::channel();
                releases.push(release);
                vec![
                    StreamingSseChunk {
                        gate: None,
                        body: responses::sse(vec![
                            responses::ev_response_created(&response_id),
                            item,
                            output,
                        ]),
                    },
                    StreamingSseChunk {
                        gate: Some(gate),
                        body: responses::sse(vec![responses::ev_completed(&response_id)]),
                    },
                ]
            })
            .collect();
        let (model, _) = start_streaming_sse_server(chunks).await;
        let (mut app, mut events, _operations) = make_test_app_with_channels().await;
        app_test_support::MockResponsesConfig::new(model.uri())
            .with_model("gpt-5.2")
            .with_model_provider("rewind-test")
            .write(app.config.codex_home.as_path())?;
        app.config.model = Some("gpt-5.2".to_string());
        app.config.model_provider_id = "rewind-test".to_string();
        app.config.model_provider = codex_model_provider_info::ModelProviderInfo {
            name: "Rewind test".to_string(),
            base_url: Some(format!("{}/v1", model.uri())),
            requires_openai_auth: false,
            ..app.config.model_provider.clone()
        };
        let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let mut started = server.start_thread(&app.config).await?;
        let thread_id = started.session.thread_id;
        server
            .thread_set_name(thread_id, "Rewind regression".to_string())
            .await?;
        started.session.thread_name = Some("Rewind regression".to_string());
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.replace_chat_widget_with_app_server_thread(
            &mut tui,
            started,
            crate::app::session_lifecycle::ThreadAttachPresentation::SessionLineage,
            /*initial_user_message*/ None,
        )
        .await?;
        while events.try_recv().is_ok() {}
        for (index, mode) in modes.into_iter().enumerate() {
            let prompt = format!("prompt-{index}");
            app.chat_widget.apply_external_edit(prompt.clone());
            app.chat_widget
                .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            while let Ok(event) = events.try_recv() {
                app.handle_event(&mut tui, &mut server, event).await?;
            }
            drive_rewind_until(
                &mut app,
                &mut tui,
                &mut server,
                &mut events,
                RewindDriveUntil::Output,
            )
            .await?;
            app.chat_widget
                .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            while let Ok(event) = events.try_recv() {
                app.handle_event(&mut tui, &mut server, event).await?;
            }
            drive_rewind_until(
                &mut app,
                &mut tui,
                &mut server,
                &mut events,
                RewindDriveUntil::Interrupted,
            )
            .await?;
            if matches!(mode, FirstRewind::Picker) {
                app.apply_backtrack_selection(BacktrackSelection {
                    thread_id,
                    nth_user_message: 0,
                    newer_user_messages: 0,
                    prompt: prompt.clone().into(),
                });
                while let Ok(event) = events.try_recv() {
                    app.handle_event(&mut tui, &mut server, event).await?;
                }
                app.chat_widget
                    .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                while let Ok(event) = events.try_recv() {
                    app.handle_event(&mut tui, &mut server, event).await?;
                }
            }
            // Submit the next turn before draining the previous rewind's server notification.
            // This exercises the normal fast Esc/edit/Enter path through the client queue.
            assert_eq!(app.chat_widget.composer_text_with_pending(), prompt);
            assert_eq!(app.backtrack_transcript_prompts(), Vec::new());
            assert_eq!(app.active_turn_id_for_thread(thread_id).await, None);
            assert!(
                server
                    .thread_read(thread_id, /*include_turns*/ true)
                    .await?
                    .turns
                    .is_empty()
            );
            assert_eq!(model.requests().await.len(), index + 1);
        }
        drive_rewind_until(
            &mut app,
            &mut tui,
            &mut server,
            &mut events,
            RewindDriveUntil::Reverted,
        )
        .await?;
        for release in releases {
            let _ = release.send(());
        }
        server.shutdown().await?;
        model.shutdown().await;
        Ok(())
    }))
}

#[derive(Clone, Copy)]
enum FirstRewind {
    Automatic,
    Picker,
}

#[test]
fn repeated_automatic_rewind_discards_queued_events_from_removed_history() -> Result<()> {
    repeated_rewind_discards_queued_events_from_removed_history(FirstRewind::Automatic)
}

#[test]
fn repeated_picker_rewind_discards_queued_events_from_removed_history() -> Result<()> {
    repeated_rewind_discards_queued_events_from_removed_history(FirstRewind::Picker)
}

fn repeated_rewind_discards_queued_events_from_removed_history(mode: FirstRewind) -> Result<()> {
    rewind_test_runtime()?.block_on(Box::pin(async {
        let (mut app, mut events, _operations) = make_test_app_with_channels().await;
        let config = app.chat_widget.config_ref().clone();
        let id = app_test_support::create_fake_paginated_rollout(
            config.codex_home.as_path(),
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            "unused preview",
            Some("test-provider"),
            /*git_info*/ None,
        )
        .expect("create paginated rollout");
        let path = app_test_support::rollout_path(
            config.codex_home.as_path(),
            "2025-01-05T12-00-00",
            &id,
        );
        let metadata = std::fs::read_to_string(&path)?
            .lines()
            .next()
            .expect("session metadata")
            .to_string();
        std::fs::write(&path, format!("{metadata}\n"))?;
        for prompt in ["retained", "first", "second"] {
            for item in [
                RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
                    turn_id: prompt.to_string(),
                    trace_id: None,
                    started_at: None,
                    model_context_window: None,
                    collaboration_mode_kind: ModeKind::default(),
                })),
                RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                    message: prompt.to_string(),
                    ..Default::default()
                })),
                RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
                    turn_id: prompt.to_string(),
                    last_agent_message: None,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                    time_to_first_token_ms: None,
                })),
            ] {
                codex_rollout::append_rollout_item_to_path(&path, &item).await?;
            }
        }
        let thread_id = ThreadId::from_string(&id)?;
        let mut server = crate::start_embedded_app_server_for_picker(&config).await?;
        let mut started = server
            .resume_thread(
                &crate::local_settings::LocalSettings::from(&config),
                config,
                thread_id,
                crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
            )
            .await?;
        started.session.thread_name = Some("Rewind regression".to_string());
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await?;
        while events.try_recv().is_ok() {}
        app.transcript_cells = ["retained", "first", "second"]
            .into_iter()
            .map(|prompt| {
                Arc::new(UserHistoryCell {
                    message: prompt.to_string(),
                    text_elements: Vec::new(),
                    local_image_paths: Vec::new(),
                    remote_image_urls: Vec::new(),
                }) as Arc<dyn HistoryCell>
            })
            .collect();
        let stale_prompt = || {
            ServerNotification::ItemCompleted(ItemCompletedNotification {
                thread_id: thread_id.to_string(),
                turn_id: "second".to_string(),
                completed_at_ms: 0,
                item: ThreadItem::UserMessage {
                    id: "late-user-message".to_string(),
                    client_id: None,
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "second".to_string(),
                        text_elements: Vec::new(),
                    }],
                },
            })
        };
        // Rewind can run while notifications are already in the UI channel but have not
        // rendered yet. Clearing the replay store alone does not remove those notifications.
        app.enqueue_thread_notification(thread_id, turn_started_notification(thread_id, "second"))
            .await?;
        app.enqueue_thread_notification(thread_id, stale_prompt()).await?;
        let old_sender = app.thread_event_channels[&thread_id].sender.clone();
        let mut tui = crate::tui::test_support::make_test_tui()?;
        match mode {
            FirstRewind::Automatic => {
                app.chat_widget.restore_user_message_to_composer("second".into());
                app.handle_event(
                    &mut tui,
                    &mut server,
                    AppEvent::RollbackOutputFreeTurnForPromptRestore {
                        thread_id,
                        turn_id: "second".to_string(),
                    },
                )
                .await?;
            }
            FirstRewind::Picker => {
                app.apply_backtrack_selection(BacktrackSelection {
                    thread_id,
                    nth_user_message: 2,
                    newer_user_messages: 0,
                    prompt: "second".into(),
                });
                while let Ok(event) = events.try_recv() {
                    app.handle_event(&mut tui, &mut server, event).await?;
                }
                app.chat_widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                while let Ok(event) = events.try_recv() {
                    app.handle_event(&mut tui, &mut server, event).await?;
                }
            }
        }
        app.drain_active_thread_events_until(&mut tui, Instant::now() + Duration::from_secs(/*secs*/ 1)).await?;
        while let Ok(event) = events.try_recv() {
            app.handle_event(&mut tui, &mut server, event).await?;
        }
        assert_eq!(app.backtrack_transcript_prompts(), vec!["retained".into(), "first".into()]);
        assert!(!app.chat_widget.is_user_turn_pending_or_running());
        assert!(old_sender.try_send(ThreadBufferedEvent::Notification(Box::new(stale_prompt()))).is_err());

        // Old-runtime notifications can also still be in the app-server client's queue.
        app.enqueue_thread_notification(thread_id, stale_prompt()).await?;
        app.enqueue_thread_notification(thread_id, ServerNotification::ThreadReverted(ThreadRevertedNotification {
            thread_id: thread_id.to_string(),
        })).await?;
        app.drain_active_thread_events_until(&mut tui, Instant::now() + Duration::from_secs(/*secs*/ 1)).await?;
        while let Ok(event) = events.try_recv() {
            app.handle_event(&mut tui, &mut server, event).await?;
        }
        // The prompt picker opens from an empty composer; discard the restored draft before
        // choosing another historical prompt so it is not merged into the next restore.
        app.chat_widget.apply_external_edit(String::new());
        app.apply_backtrack_selection(BacktrackSelection {
            thread_id,
            nth_user_message: 1,
            newer_user_messages: 0,
            prompt: "first".into(),
        });
        while let Ok(event) = events.try_recv() {
            app.handle_event(&mut tui, &mut server, event).await?;
        }
        app.chat_widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(&event, AppEvent::ApplyBacktrackRestore { mode, .. } if *mode != BacktrackRestoreMode::Conversation));
            app.handle_event(&mut tui, &mut server, event).await?;
        }
        assert_eq!(app.backtrack_transcript_prompts(), vec!["retained".into()]);
        assert_eq!(app.chat_widget.composer_text_with_pending(), "first");
        let rendered = render_bottom_popup(&app.chat_widget, /*width*/ 80)
            .replace(&app.chat_widget.config_ref().cwd.display().to_string(), "[cwd]");
        insta::assert_snapshot!("repeated_rewind_restores_prompt", rendered);
        assert_eq!(server.thread_read(thread_id, /*include_turns*/ true).await?.turns.iter().map(|turn| turn.id.as_str()).collect::<Vec<_>>(), vec!["retained"]);
        server.shutdown().await?;
        Ok(())
    }))
}
