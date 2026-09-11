use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Line;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::broadcast::error::TryRecvError;

use super::*;
use crate::chatwidget::tests::helpers::normalized_backend_snapshot;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use crate::tui::MouseScrollDirection;
use crate::tui::MouseScrollEvent;

#[derive(Debug)]
struct TestCell(&'static str);

impl HistoryCell for TestCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.0.into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.0.into()]
    }
}

#[test]
fn continuous_scroll_accelerates_and_pause_or_reversal_resets_it() {
    let mut acceleration = ScrollAcceleration::default();
    let started_at = Instant::now();
    let rows = (0..12)
        .map(|step| {
            acceleration.rows_at(
                MouseScrollDirection::Down,
                started_at + Duration::from_millis(step * 17),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(rows, vec![3, 4, 5, 6, 7, 7, 8, 9, 9, 10, 10, 10]);
    assert_eq!(
        (
            acceleration.rows_at(
                MouseScrollDirection::Down,
                started_at + Duration::from_millis(/*millis*/ 500),
            ),
            acceleration.rows_at(
                MouseScrollDirection::Up,
                started_at + Duration::from_millis(/*millis*/ 517),
            ),
        ),
        (3, 3),
    );

    let mut rapid = ScrollAcceleration::default();
    let capped_rows = (0..100)
        .map(|step| {
            rapid.rows_at(
                MouseScrollDirection::Down,
                started_at + Duration::from_millis(step),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(capped_rows.last(), Some(&15));
    assert!(capped_rows.iter().all(|rows| *rows <= 15));
}

#[tokio::test]
async fn renders_committed_conversation_above_fixed_composer() {
    let (mut chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    chat_widget.apply_external_edit("draft sentinel".to_string());
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    screen
        .viewport
        .push_cell(Arc::new(TestCell("committed response")));
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 50, /*height*/ 10)).expect("create terminal");

    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render owned screen");

    assert_snapshot!(normalized_backend_snapshot(terminal.backend()), @r###"
"committed response                                "
"                                                  "
"                                                  "
"                                                  "
"                                                  "
"                                                  "
"                                                  "
"› draft sentinel                                  "
"                                                  "
"  gpt-5.6-sol default · /tmp/project              "
"###);
}

#[tokio::test]
async fn committed_cell_updates_viewport_without_queuing_terminal_history() {
    let mut app = super::super::test_support::make_test_app().await;
    app.owned_screen = App::owned_screen_for_behavior(
        AltScreenBehavior::Owned,
        &app.chat_widget,
        app.keymap.pager.clone(),
    );
    let mut tui = crate::tui::test_support::make_test_tui().expect("create test TUI");

    app.insert_history_cell(&mut tui, Box::new(TestCell("retained")));

    let screen = app.owned_screen.as_ref().expect("owned screen");
    assert_eq!(screen.viewport.committed_cell_count(), 1);
    assert_eq!(app.transcript_cells.len(), 1);
    assert!(!app.has_emitted_history_lines);
    assert!(!tui.has_pending_history_lines());
}

#[tokio::test]
async fn replay_retains_cells_while_draw_scheduling_is_deferred() {
    let mut app = super::super::test_support::make_test_app().await;
    app.owned_screen = App::owned_screen_for_behavior(
        AltScreenBehavior::Owned,
        &app.chat_widget,
        app.keymap.pager.clone(),
    );
    let mut tui = crate::tui::test_support::make_test_tui().expect("create test TUI");
    let mut draw_rx = tui.subscribe_draws_for_test();

    app.begin_initial_history_replay_buffer();
    app.insert_history_cell(&mut tui, Box::new(TestCell("first")));
    app.insert_history_cell(&mut tui, Box::new(TestCell("second")));

    tokio::time::sleep(Duration::from_millis(/*millis*/ 50)).await;
    assert!(matches!(draw_rx.try_recv(), Err(TryRecvError::Empty)));

    assert!(app.owned_screen_replay_in_progress());
    assert_eq!(
        app.owned_screen
            .as_ref()
            .expect("owned screen")
            .viewport
            .committed_cell_count(),
        2
    );

    app.finish_initial_history_replay_buffer(&mut tui);

    assert!(!app.owned_screen_replay_in_progress());
    tokio::time::timeout(Duration::from_secs(/*secs*/ 1), draw_rx.recv())
        .await
        .expect("timed out waiting for replay completion draw")
        .expect("draw channel closed");
}

#[tokio::test]
async fn navigation_does_not_steal_printable_or_draft_input() {
    let mut app = super::super::test_support::make_test_app().await;
    app.owned_screen = App::owned_screen_for_behavior(
        AltScreenBehavior::Owned,
        &app.chat_widget,
        app.keymap.pager.clone(),
    );
    let mut tui = crate::tui::test_support::make_test_tui().expect("create test TUI");

    let cases = [
        (KeyCode::Char('k'), false),
        (KeyCode::Up, true),
        (KeyCode::Down, true),
        (KeyCode::Home, false),
        (KeyCode::End, false),
        (KeyCode::PageUp, true),
        (KeyCode::PageDown, true),
    ];
    for (code, expected) in cases {
        assert_eq!(
            app.handle_owned_screen_navigation_key(
                &mut tui,
                KeyEvent::new(code, KeyModifiers::NONE),
            ),
            expected,
        );
    }

    app.chat_widget.apply_external_edit("draft".to_string());
    assert!(!app.handle_owned_screen_navigation_key(
        &mut tui,
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
    ));
    assert!(app.handle_owned_screen_navigation_key(
        &mut tui,
        KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
    ));
}

#[tokio::test]
async fn mouse_wheel_scrolls_transcript_without_changing_draft() {
    let (mut chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    chat_widget.apply_external_edit("draft sentinel".to_string());
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    for text in ["oldest", "older", "middle", "newer", "LATEST"] {
        screen.viewport.push_cell(Arc::new(TestCell(text)));
    }
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 40, /*height*/ 8)).expect("create terminal");
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render bottom");

    assert!(screen.handle_mouse_scroll(MouseScrollEvent {
        direction: MouseScrollDirection::Up,
        column: 2,
        row: 2,
    }));
    assert!(screen.handle_mouse_scroll(MouseScrollEvent {
        direction: MouseScrollDirection::Up,
        column: 2,
        row: 2,
    }));
    assert_eq!(screen.scroll_acceleration.multiplier_per_mille, PER_MILLE);
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render scrolled");

    let single_frame_scroll = normalized_backend_snapshot(terminal.backend());
    assert_snapshot!(single_frame_scroll, @r###"
"                                        "
"middle                                  "
"      ctrl + end jump to bottom ↓       "
"                                        "
"                                        "
"› draft sentinel                        "
"                                        "
"  gpt-5.6-sol default · /tmp/project    "
"###);
    assert!(!screen.viewport.is_following_bottom());
    assert!(!screen.handle_mouse_scroll(MouseScrollEvent {
        direction: MouseScrollDirection::Up,
        column: 2,
        row: 7,
    }));

    assert!(screen.handle_navigation_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL,)));
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render restored bottom");
    assert!(screen.viewport.is_following_bottom());

    assert!(screen.handle_navigation_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE,)));
    assert!(screen.handle_navigation_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE,)));
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render coalesced key scroll");
    assert_eq!(
        normalized_backend_snapshot(terminal.backend()),
        single_frame_scroll,
    );
}
