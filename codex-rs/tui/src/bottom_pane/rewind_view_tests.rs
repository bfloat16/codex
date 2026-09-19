use super::*;
use crate::app_event::AppEvent;
use crate::render::renderable::Renderable;
use assert_matches::assert_matches;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use tokio::sync::mpsc::unbounded_channel;

fn render_lines(view: &RewindView, width: u16) -> String {
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buf[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn test_sender() -> (
    AppEventSender,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let (tx, rx) = unbounded_channel();
    (AppEventSender::new(tx), rx)
}

#[test]
fn prompt_picker_defaults_to_current_without_rewinding() {
    let (tx, mut rx) = test_sender();
    let mut view = RewindView::new(
        RewindViewParams::Prompts {
            view_id: "rewind-test",
            items: vec![
                RewindPromptItem {
                    prompt: "change the parser".to_string(),
                    code_summary: Some("Edited parser.rs (+3 -1)".into()),
                    is_current: false,
                    action: Box::new(|_| panic!("historical prompt should not be selected")),
                },
                RewindPromptItem {
                    prompt: String::new(),
                    code_summary: None,
                    is_current: true,
                    action: Box::new(|tx| tx.send(AppEvent::CancelBacktrackRestore)),
                },
            ],
            on_cancel: Box::new(|tx| tx.send(AppEvent::CancelBacktrackRestore)),
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    assert_eq!(view.selected_index(), Some(1));
    view.handle_key_event(KeyEvent::from(crossterm::event::KeyCode::Enter));
    assert_matches!(rx.try_recv(), Ok(AppEvent::CancelBacktrackRestore));
    assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
}

#[test]
fn historical_prompt_waits_for_restore_picker() {
    let (tx, mut rx) = test_sender();
    let mut view = RewindView::new(
        RewindViewParams::Prompts {
            view_id: "rewind-test",
            items: vec![
                RewindPromptItem {
                    prompt: "change the parser".to_string(),
                    code_summary: Some("Edited parser.rs (+3 -1)".into()),
                    is_current: false,
                    action: Box::new(|tx| tx.send(AppEvent::BacktrackRestoreBack)),
                },
                RewindPromptItem {
                    prompt: String::new(),
                    code_summary: None,
                    is_current: true,
                    action: Box::new(|_| {}),
                },
            ],
            on_cancel: Box::new(|_| {}),
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    view.handle_key_event(KeyEvent::from(crossterm::event::KeyCode::Up));
    view.handle_key_event(KeyEvent::from(crossterm::event::KeyCode::Enter));

    assert_matches!(rx.try_recv(), Ok(AppEvent::BacktrackRestoreBack));
    assert_eq!(view.completion(), None);
    assert!(view.dismiss_after_child_accept());

    view.clear_dismiss_after_child_accept();
    assert!(!view.dismiss_after_child_accept());
}

#[test]
fn restore_picker_cancel_returns_to_prompt_picker() {
    let (tx, mut rx) = test_sender();
    let mut view = RewindView::new(
        RewindViewParams::Restore {
            view_id: "rewind-test",
            prompt: "change the parser".to_string(),
            options: vec![RewindRestoreOption {
                label: "Restore conversation".to_string(),
                details: vec!["The code will be unchanged.".into()],
                action: Box::new(|_| {}),
            }],
            warning: "Shell and manual edits are left untouched.".into(),
            on_cancel: Box::new(|tx| tx.send(AppEvent::BacktrackRestoreBack)),
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    view.handle_key_event(KeyEvent::from(crossterm::event::KeyCode::Esc));

    assert_matches!(rx.try_recv(), Ok(AppEvent::BacktrackRestoreBack));
    assert_eq!(view.completion(), Some(ViewCompletion::Cancelled));
}

#[test]
fn rewind_prompt_picker_snapshot() {
    let (tx, _rx) = test_sender();
    let view = RewindView::new(
        RewindViewParams::Prompts {
            view_id: "rewind-test",
            items: vec![
                RewindPromptItem {
                    prompt: "change the parser".to_string(),
                    code_summary: Some("Edited parser.rs (+3 -1)".into()),
                    is_current: false,
                    action: Box::new(|_| {}),
                },
                RewindPromptItem {
                    prompt: "add regression tests".to_string(),
                    code_summary: Some("No code changes".into()),
                    is_current: false,
                    action: Box::new(|_| {}),
                },
                RewindPromptItem {
                    prompt: "update the footer".to_string(),
                    code_summary: Some(
                        vec![
                            "2 files changed (".into(),
                            "+8".green(),
                            " ".into(),
                            "-3".red(),
                            ")".into(),
                        ]
                        .into(),
                    ),
                    is_current: false,
                    action: Box::new(|_| {}),
                },
                RewindPromptItem {
                    prompt: "polish the diff panel".to_string(),
                    code_summary: Some("Edited diff_panel.rs (+4 -1)".into()),
                    is_current: false,
                    action: Box::new(|_| {}),
                },
                RewindPromptItem {
                    prompt: String::new(),
                    code_summary: None,
                    is_current: true,
                    action: Box::new(|_| {}),
                },
            ],
            on_cancel: Box::new(|_| {}),
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    assert_snapshot!("rewind_prompt_picker", render_lines(&view, 72));
}

#[test]
fn rewind_restore_picker_snapshot() {
    let (tx, _rx) = test_sender();
    let view = RewindView::new(
        RewindViewParams::Restore {
            view_id: "rewind-test",
            prompt: "change the parser".to_string(),
            options: vec![
                RewindRestoreOption {
                    label: "Restore code and conversation".to_string(),
                    details: vec![
                        "The selected message and everything after it will be removed.".into(),
                        "The code in 2 tracked files will be restored.".into(),
                    ],
                    action: Box::new(|_| {}),
                },
                RewindRestoreOption {
                    label: "Restore conversation".to_string(),
                    details: vec![
                        "The selected message and everything after it will be removed.".into(),
                        "The code will be unchanged.".into(),
                    ],
                    action: Box::new(|_| {}),
                },
                RewindRestoreOption {
                    label: "Restore code".to_string(),
                    details: vec![
                        "The conversation will be unchanged.".into(),
                        "The code in 2 tracked files will be restored.".into(),
                    ],
                    action: Box::new(|_| {}),
                },
                RewindRestoreOption {
                    label: "Never mind".to_string(),
                    details: vec!["The code and conversation will be unchanged.".into()],
                    action: Box::new(|_| {}),
                },
            ],
            warning: "Only changes made through Codex apply_patch are restored; shell and manual edits are left untouched.".into(),
            on_cancel: Box::new(|_| {}),
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    assert_snapshot!("rewind_restore_picker", render_lines(&view, 72));
}
