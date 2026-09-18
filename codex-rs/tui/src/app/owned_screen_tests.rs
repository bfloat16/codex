use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Position;
use ratatui::style::Color;
use ratatui::text::Line;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::broadcast::error::TryRecvError;

use super::*;
use crate::chatwidget::tests::helpers::normalized_backend_snapshot;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use crate::tui::MouseInteractionEvent;
use crate::tui::MouseInteractionKind;
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

    screen
        .viewport
        .replace_cells(vec![Arc::new(TestCell("A中B"))]);
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render wide selection source");
    screen.handle_mouse_interaction(MouseInteractionEvent {
        kind: MouseInteractionKind::LeftDown,
        column: 2,
        row: 0,
    });
    screen.handle_mouse_interaction(MouseInteractionEvent {
        kind: MouseInteractionKind::LeftDrag,
        column: 3,
        row: 0,
    });
    let OwnedScreenMouseAction::Copy(copied) =
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftUp,
            column: 3,
            row: 0,
        })
    else {
        panic!("expected copied wide selection");
    };
    assert_eq!(copied, "中B");
}

#[tokio::test]
async fn double_click_selects_and_copies_a_word() {
    let (chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    screen
        .viewport
        .push_cell(Arc::new(TestCell("run foo_bar --flag now")));
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 40, /*height*/ 8)).expect("create terminal");
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render double-click source");

    for click in 0..2 {
        assert!(matches!(
            screen.handle_mouse_interaction(MouseInteractionEvent {
                kind: MouseInteractionKind::LeftDown,
                column: 7,
                row: 0,
            }),
            OwnedScreenMouseAction::Redraw
        ));
        let action = screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftUp,
            column: 7,
            row: 0,
        });
        if click == 0 {
            assert!(matches!(action, OwnedScreenMouseAction::Redraw));
        } else {
            assert!(matches!(action, OwnedScreenMouseAction::Copy(text) if text == "foo_bar"));
        }
    }

    screen.show_copy_notice(/*char_count*/ 7);
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render persistent double-click selection");
    let selected_background = selection::selection_background();
    assert!((4..=10).all(|column| {
        terminal.backend().buffer()[Position::new(column, 0)].bg == selected_background
    }));
}

#[tokio::test]
async fn patch_background_extends_past_the_reserved_wrap_width() {
    let (mut chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    chat_widget.set_pet_image_support_for_tests(crate::pets::PetImageSupport::Supported(
        crate::pets::ImageProtocol::Kitty,
    ));
    chat_widget.install_test_ambient_pet_for_tests(/*animations_enabled*/ false);
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    screen
        .viewport
        .push_cell(Arc::new(crate::history_cell::new_patch_event(
            std::path::PathBuf::from("src/new.rs"),
            crate::diff_model::FileChange::Add {
                content: "fn added() {}\n".to_string(),
            },
            std::path::Path::new("/tmp/project"),
        )));
    let width = 50;
    assert!(chat_widget.history_wrap_width(width) < width);
    let mut terminal =
        Terminal::new(TestBackend::new(width, /*height*/ 8)).expect("create terminal");

    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render collapsed patch with reserved columns");
    assert!(screen.viewport.handle_left_click(
        screen.last_conversation_area,
        Position::new(/*x*/ 4, screen.last_conversation_area.y),
    ));
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render expanded patch with reserved columns");

    let diff_background = terminal.backend().buffer()[Position::new(/*x*/ 4, /*y*/ 1)].bg;
    assert_ne!(diff_background, Color::Reset);
    assert!((0..width).all(|x| {
        terminal.backend().buffer()[Position::new(x, /*y*/ 1)].bg == diff_background
    }));
}

#[tokio::test]
async fn diff_panel_collapses_locks_and_navigates_file_changes() {
    let (chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    let cwd = std::path::Path::new("/tmp/project");
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    let patch_a = diffy::create_patch("old\n", "new\n").to_string();
    let patch_b = diffy::create_patch("before\n", "after\n").to_string();
    for cell in crate::history_cell::new_patch_events(
        std::collections::HashMap::from([
            (
                std::path::PathBuf::from("src/a.rs"),
                crate::diff_model::FileChange::Update {
                    unified_diff: patch_a,
                    move_path: None,
                },
            ),
            (
                std::path::PathBuf::from("src/b.rs"),
                crate::diff_model::FileChange::Update {
                    unified_diff: patch_b,
                    move_path: None,
                },
            ),
        ]),
        cwd,
    ) {
        screen.viewport.push_cell(Arc::new(cell));
    }
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 120, /*height*/ 24)).expect("create terminal");
    let render = |screen: &mut OwnedScreen, terminal: &mut Terminal<TestBackend>| {
        terminal
            .draw(|frame| {
                screen.render(&chat_widget, frame.area(), frame.buffer_mut());
            })
            .expect("render owned screen");
    };

    render(&mut screen, &mut terminal);
    assert!(screen.viewport.handle_left_click(
        screen.last_conversation_area,
        Position::new(/*x*/ 4, /*y*/ 0),
    ));
    render(&mut screen, &mut terminal);
    assert!(screen.viewport.handle_left_click(
        screen.last_conversation_area,
        Position::new(/*x*/ 4, /*y*/ 5),
    ));

    let panel_width = crate::diff_panel::diff_panel_width(/*terminal_width*/ 120)
        .expect("wide terminal supports panel");
    let conversation_width = chat_widget.history_wrap_width(120_u16.saturating_sub(panel_width));
    let generation = screen
        .open_diff_panel(/*terminal_width*/ 120, conversation_width)
        .expect("open diff panel");
    assert!(screen.apply_diff_panel_result(
        generation,
        Ok((
            true,
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n\
             diff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1 +1 @@\n-before\n+after\n"
                .to_string(),
        )),
    ));
    render(&mut screen, &mut terminal);
    let opened = normalized_backend_snapshot(terminal.backend());

    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftDown,
            column: 4,
            row: 2,
        }),
        OwnedScreenMouseAction::Redraw | OwnedScreenMouseAction::Ignored,
    ));
    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftUp,
            column: 4,
            row: 2,
        }),
        OwnedScreenMouseAction::Redraw,
    ));
    render(&mut screen, &mut terminal);
    let jumped = normalized_backend_snapshot(terminal.backend());

    screen.close_diff_panel();
    assert!(screen.viewport.handle_left_click(
        screen.last_conversation_area,
        Position::new(/*x*/ 4, /*y*/ 0),
    ));
    render(&mut screen, &mut terminal);
    let closed = normalized_backend_snapshot(terminal.backend());

    assert_snapshot!(format!(
        "opened:\n{opened}\njumped to b:\n{jumped}\nclosed and re-expanded:\n{closed}",
    ));
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
        (KeyCode::Up, false),
        (KeyCode::Down, false),
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

    assert!(!screen.handle_navigation_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE,)));
    assert!(!screen.handle_navigation_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE,)));
}

#[tokio::test]
async fn copy_notice_tracks_multiline_composer_top_below_scroll_banner() {
    let (mut chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    chat_widget
        .apply_external_edit("first draft line\nsecond draft line\nthird draft line".to_string());
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    for text in ["oldest", "older", "middle", "newer", "latest"] {
        screen.viewport.push_cell(Arc::new(TestCell(text)));
    }
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 50, /*height*/ 10)).expect("create terminal");
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
    screen.show_copy_notice(/*char_count*/ 12);

    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render scroll and copy hints");

    assert_snapshot!(normalized_backend_snapshot(terminal.backend()), @r###"
"                                                  "
"middle                                            "
"           ctrl + end jump to bottom ↓            "
"                    copied 12 chars to clipboard  "
"                                                  "
"› first draft line                                "
"  second draft line                               "
"  third draft line                                "
"                                                  "
"  Read Only        · gpt-5.6-sol default · /tmp/p…"
"###);
}

#[tokio::test]
async fn drag_selects_visible_text_and_copy_notice_renders_above_the_composer() {
    let (mut chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    chat_widget.apply_external_edit("draft sentinel".to_string());
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    screen
        .viewport
        .push_cell(Arc::new(TestCell("alpha beta gamma")));
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 50, /*height*/ 10)).expect("create terminal");
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render selection source");

    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftDown,
            column: 0,
            row: 0,
        }),
        OwnedScreenMouseAction::Redraw
    ));
    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftDrag,
            column: 4,
            row: 0,
        }),
        OwnedScreenMouseAction::Redraw
    ));
    let selected_background = crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || {
            terminal
                .draw(|frame| {
                    screen.render(&chat_widget, frame.area(), frame.buffer_mut());
                })
                .expect("render selection highlight");
            selection::selection_background()
        },
    );
    assert_eq!(
        selected_background,
        crate::terminal_palette::rgb_color((38, 79, 120)),
    );
    assert!((0..=4).all(|column| {
        terminal.backend().buffer()[Position::new(column, 0)].bg == selected_background
    }));
    let copied = screen.handle_mouse_interaction(MouseInteractionEvent {
        kind: MouseInteractionKind::LeftUp,
        column: 4,
        row: 0,
    });
    let OwnedScreenMouseAction::Copy(copied) = copied else {
        panic!("expected copied selection");
    };
    assert_eq!(copied, "alpha");

    screen.show_copy_notice(copied.chars().count());
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render copy notice");
    assert_eq!(
        terminal.backend().buffer()[Position::new(/*x*/ 21, /*y*/ 5)].fg,
        selected_background,
    );
    assert_snapshot!(normalized_backend_snapshot(terminal.backend()), @r###"
"alpha beta gamma                                  "
"                                                  "
"                                                  "
"                                                  "
"                                                  "
"                     copied 5 chars to clipboard  "
"                                                  "
"› draft sentinel                                  "
"                                                  "
"  Read Only        · gpt-5.6-sol default · /tmp/p…"
"###);

    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftDown,
            column: 2,
            row: 7,
        }),
        OwnedScreenMouseAction::Redraw
    ));
    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftDrag,
            column: 15,
            row: 7,
        }),
        OwnedScreenMouseAction::Redraw
    ));
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render composer selection");
    assert!((2..=15).all(|column| {
        terminal.backend().buffer()[Position::new(column, 7)].bg == selected_background
    }));
    let OwnedScreenMouseAction::Copy(copied) =
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftUp,
            column: 15,
            row: 7,
        })
    else {
        panic!("expected copied composer selection");
    };
    assert_eq!(copied, "draft sentinel");
}

#[tokio::test]
async fn selection_copies_wide_text_without_padding_spaces() {
    let (chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    let mut screen = OwnedScreen::new(&chat_widget, crate::keymap::RuntimeKeymap::defaults().pager);
    let text = "● Unicode 选择测试通过，第二轮 scoped Clippy 也完成。";
    screen.viewport.push_cell(Arc::new(TestCell(text)));
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 60, /*height*/ 8)).expect("create terminal");
    terminal
        .draw(|frame| {
            screen.render(&chat_widget, frame.area(), frame.buffer_mut());
        })
        .expect("render Unicode selection source");

    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftDown,
            column: 0,
            row: 0,
        }),
        OwnedScreenMouseAction::Redraw
    ));
    assert!(matches!(
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftDrag,
            column: u16::try_from(unicode_width::UnicodeWidthStr::width(text))
                .expect("selection width fits u16")
                .saturating_sub(1),
            row: 0,
        }),
        OwnedScreenMouseAction::Redraw
    ));
    let OwnedScreenMouseAction::Copy(copied) =
        screen.handle_mouse_interaction(MouseInteractionEvent {
            kind: MouseInteractionKind::LeftUp,
            column: u16::try_from(unicode_width::UnicodeWidthStr::width(text))
                .expect("selection width fits u16")
                .saturating_sub(1),
            row: 0,
        })
    else {
        panic!("expected copied Unicode selection");
    };

    assert_eq!(copied, text);
}
