use super::*;
use crate::app_event::BacktrackRestoreMode;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn rewind_keeps_history_and_checkpoints_when_file_restore_is_incomplete() -> Result<()> {
    rewind_test_runtime()?.block_on(Box::pin(async {
        let (mut app, mut events, _operations) = make_test_app_with_channels().await;
        let config = app.chat_widget.config_ref().clone();
        let id = app_test_support::create_fake_paginated_rollout(
            config.codex_home.as_path(),
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            "saved prompt",
            Some("test-provider"),
            /*git_info*/ None,
        ).expect("create paginated rollout");
        let path = app_test_support::rollout_path(
            config.codex_home.as_path(), "2025-01-05T12-00-00", &id,
        );
        let contents = std::fs::read_to_string(&path)?;
        let mut lines = contents.lines().map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        lines.insert(1, json!({
            "timestamp": "2025-01-05T12:00:00Z", "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "selected", "model_context_window": null}
        }));
        lines.push(json!({
            "timestamp": "2025-01-05T12:00:00Z", "type": "event_msg",
            "payload": {"type": "task_complete", "turn_id": "selected", "last_agent_message": null}
        }));
        for (ordinal, line) in lines.iter_mut().enumerate() {
            line["ordinal"] = json!(ordinal);
        }
        let contents = lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n") + "\n";
        std::fs::write(&path, contents)?;
        let checkpoint_dir = config.codex_home.join("file-checkpoints").join(&id);
        std::fs::create_dir_all(&checkpoint_dir)?;
        let file = json!({"environmentId": "offline", "path": config.cwd.join("tracked.txt")});
        let journal = [
            json!({"type": "begin_turn", "turn_id": "selected"}),
            json!({"type": "before_image", "turn_id": "selected", "file": file, "before": {"type": "absent"}}),
            json!({"type": "after_image", "file": file, "after": {"type": "present", "sha256": "0".repeat(64), "size": 1}}),
        ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n") + "\n";
        let journal_path = checkpoint_dir.join("journal.jsonl");
        std::fs::write(&journal_path, &journal)?;
        let thread_id = ThreadId::from_string(&id)?;
        let mut server = crate::start_embedded_app_server_for_picker(&config).await?;
        let started = server.resume_thread(
            &crate::local_settings::LocalSettings::from(&config), config, thread_id,
            crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
        ).await?;
        app.enqueue_primary_thread_session(started.session, started.turns).await?;
        while let Ok(event) = events.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                app.transcript_cells.push(cell.into());
            }
        }
        // Paginated replay is normally rendered by the app loop. Seed its visible prompt here
        // so this test exercises the restore action without also testing history hydration.
        app.transcript_cells = vec![Arc::new(UserHistoryCell {
            message: "saved prompt".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        })];
        let before = server.thread_read(thread_id, /*include_turns*/ true).await?.turns;
        assert_eq!(before.len(), 1);
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.backtrack.pending_rollback = Some(crate::app_backtrack::PendingBacktrackRollback {
            selection: crate::app_backtrack::BacktrackSelection {
                thread_id, nth_user_message: 0, newer_user_messages: 0,
                prompt: crate::chatwidget::UserMessage::from("saved prompt"),
            },
        });
        let restore = |mode| AppEvent::ApplyBacktrackRestore {
            thread_id,
            nth_user_message: 0,
            target: crate::app_backtrack::BacktrackRollbackTarget {
                before_turn_id: "selected".to_string(),
                legacy_num_turns: 1,
                removed_turn_ids: vec!["selected".to_string()],
            },
            prompt: crate::chatwidget::UserMessage::from("saved prompt"),
            mode,
        };
        app.handle_event(&mut tui, &mut server, restore(BacktrackRestoreMode::CodeAndConversation)).await?;
        // A stale picker action queued before the failure must not discard the retry checkpoint.
        app.handle_event(&mut tui, &mut server, restore(BacktrackRestoreMode::Conversation)).await?;
        assert_eq!(server.thread_read(thread_id, /*include_turns*/ true).await?.turns, before);
        assert_eq!(std::fs::read_to_string(journal_path)?, journal);
        assert_eq!(user_count(&app.transcript_cells), 1);
        let errors = std::iter::from_fn(|| events.try_recv().ok()).filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(lines_to_single_string(&cell.display_lines(/*width*/ 200))),
            _ => None,
        }).collect::<Vec<_>>().join("\n");
        insta::assert_snapshot!(errors);
        server.shutdown().await?;
        Ok(())
    }))
}
