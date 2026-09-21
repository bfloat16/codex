use super::*;
use crate::history_cell::LiveToolCell;
use crate::history_cell::ToolActivity;
use crate::history_cell::ToolPreview;
use pretty_assertions::assert_eq;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug)]
struct Commands(Vec<ToolPreview>);

impl HistoryCell for Commands {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.0.iter().map(|preview| preview.line.clone()).collect()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(/*width*/ 80)
    }

    fn tool_activity(&self) -> Option<ToolActivity> {
        Some(ToolActivity {
            call_count: self.0.len(),
            shell_commands: self.0.len(),
            ..ToolActivity::default()
        })
    }

    fn tool_group_previews(&self) -> Vec<ToolPreview> {
        self.0.clone()
    }

    fn tool_group_preview_is_active(&self) -> bool {
        self.0.iter().any(|preview| preview.running)
    }

    fn tool_render_state(&self, now: Instant) -> ToolRenderState {
        ToolRenderState {
            running: self.tool_group_preview_is_active(),
            next_expiry: self
                .0
                .iter()
                .filter_map(|preview| preview.completed_at)
                .map(|at| at + crate::history_cell::TOOL_COMPLETION_RETENTION)
                .filter(|at| *at > now)
                .min(),
            ..ToolRenderState::default()
        }
    }
}

#[test]
fn running_commands_remain_visible_and_completions_expire_independently() {
    let shared = LiveToolCell::new(Box::new(Commands(vec![
        ToolPreview {
            line: "Ran first very long command with arguments".cyan().into(),
            running: true,
            completed_at: None,
        },
        ToolPreview {
            line: "Ran second".cyan().into(),
            running: true,
            completed_at: None,
        },
    ])));
    let mut viewport = ConversationViewport::new(
        vec![Arc::new(shared.clone())],
        HistoryRenderMode::Rich,
        crate::keymap::RuntimeKeymap::defaults().pager,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 30, /*height*/ 5,
    );
    let mut snapshots = Vec::new();
    for stage in [
        "running",
        "second completed",
        "second expired",
        "appendable",
        "closed",
    ] {
        if stage != "running" {
            let mut cell = shared.lock();
            let commands = cell
                .as_any_mut()
                .downcast_mut::<Commands>()
                .expect("commands");
            match stage {
                "second completed" => {
                    commands.0[1].running = false;
                    commands.0[1].completed_at = Some(Instant::now());
                }
                "second expired" => {
                    commands.0[1].completed_at =
                        Some(Instant::now() - Duration::from_secs(/*secs*/ 3))
                }
                "appendable" => {
                    commands.0[0].running = false;
                    commands.0[0].completed_at = Some(Instant::now());
                }
                "closed" => {}
                _ => unreachable!(),
            }
        }
        viewport.sync_live_tail(
            area.width,
            /*active_key*/ None,
            ActiveToolGroupState {
                accepting_content: stage == "appendable",
                ..ActiveToolGroupState::default()
            },
            |_| None,
        );
        let mut buffer = Buffer::empty(area);
        viewport.render(area, &mut buffer);
        if stage == "second completed" {
            assert_eq!(buffer[(4, 1)].fg, Color::Cyan);
            assert_eq!(buffer[(4, 2)].fg, Color::Reset);
            assert!(buffer[(4, 2)].modifier.contains(Modifier::DIM));
        }
        let text = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        snapshots.push(format!("{stage}:\n{}", text.trim_end()));
    }
    insta::assert_snapshot!(snapshots.join("\n\n"));
}

#[test]
fn completion_deadline_is_exact_and_does_not_restart_on_render() {
    let now = Instant::now();
    let preview = ToolPreview {
        line: "Ran done".into(),
        running: false,
        completed_at: Some(now),
    };
    assert_eq!(
        [
            preview.visible_at(now),
            preview.visible_at(now + Duration::from_millis(/*millis*/ 1999)),
            preview.visible_at(now + Duration::from_secs(/*secs*/ 2)),
            preview.visible_at(now + Duration::from_secs(/*secs*/ 3)),
        ],
        [true, true, false, false]
    );
}

#[test]
fn all_running_commands_remain_visible_when_a_later_message_closes_the_group() {
    let commands = Commands(
        (0..12)
            .map(|index| ToolPreview {
                line: format!("Ran command {index}").cyan().into(),
                running: true,
                completed_at: None,
            })
            .collect(),
    );
    let mut viewport = ConversationViewport::new(
        vec![
            Arc::new(commands),
            Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
                "Later message".into(),
            ])),
        ],
        HistoryRenderMode::Rich,
        crate::keymap::RuntimeKeymap::defaults().pager,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 32, /*height*/ 16,
    );
    let mut buffer = Buffer::empty(area);
    viewport.render(area, &mut buffer);
    for index in 0..12 {
        let line = (0..area.width)
            .map(|x| buffer[(x, index + 1)].symbol())
            .collect::<String>();
        assert_eq!(line.trim(), format!("└ Ran command {index}"));
    }
}

#[derive(Debug)]
struct CountingCommands {
    commands: Commands,
    activity_calls: Arc<AtomicUsize>,
    preview_calls: Arc<AtomicUsize>,
}

impl HistoryCell for CountingCommands {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.commands.display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.commands.raw_lines()
    }

    fn tool_activity(&self) -> Option<ToolActivity> {
        self.activity_calls.fetch_add(/*val*/ 1, Ordering::Relaxed);
        self.commands.tool_activity()
    }

    fn tool_group_previews(&self) -> Vec<ToolPreview> {
        self.preview_calls.fetch_add(/*val*/ 1, Ordering::Relaxed);
        self.commands.tool_group_previews()
    }

    fn tool_group_preview_is_active(&self) -> bool {
        self.commands.tool_group_preview_is_active()
    }

    fn tool_render_state(&self, now: Instant) -> ToolRenderState {
        self.commands.tool_render_state(now)
    }
}

#[test]
fn long_tool_history_refreshes_linearly_and_stops_formatting_settled_commands() {
    let activity_calls = Arc::new(AtomicUsize::new(/*v*/ 0));
    let preview_calls = Arc::new(AtomicUsize::new(/*v*/ 0));
    let expired = Instant::now() - Duration::from_secs(/*secs*/ 3);
    let shared: Vec<_> = (0..512)
        .map(|index| {
            LiveToolCell::new(Box::new(CountingCommands {
                commands: Commands(vec![ToolPreview {
                    line: format!("Ran command {index}").into(),
                    running: index == 511,
                    completed_at: Some(expired),
                }]),
                activity_calls: activity_calls.clone(),
                preview_calls: preview_calls.clone(),
            }))
        })
        .collect();
    let mut viewport = ConversationViewport::new(
        shared
            .iter()
            .cloned()
            .map(|cell| Arc::new(cell) as Arc<dyn HistoryCell>)
            .collect(),
        HistoryRenderMode::Rich,
        crate::keymap::RuntimeKeymap::defaults().pager,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 10,
    );
    let mut buffer = Buffer::empty(area);
    viewport.render(area, &mut buffer);
    activity_calls.store(/*val*/ 0, Ordering::Relaxed);
    preview_calls.store(/*val*/ 0, Ordering::Relaxed);
    for _ in 0..10 {
        viewport.render(area, &mut buffer);
        assert!(viewport.tool_refresh_delay().is_some());
    }
    assert_eq!(preview_calls.load(Ordering::Relaxed), 10);
    assert!(activity_calls.load(Ordering::Relaxed) < 512 * 10 * 8);

    shared[511]
        .lock()
        .as_any_mut()
        .downcast_mut::<CountingCommands>()
        .expect("command")
        .commands
        .0[0]
        .running = false;
    viewport.render(area, &mut buffer);
    activity_calls.store(/*val*/ 0, Ordering::Relaxed);
    preview_calls.store(/*val*/ 0, Ordering::Relaxed);
    for _ in 0..10 {
        viewport.render(area, &mut buffer);
        assert_eq!(viewport.tool_refresh_delay(), None);
    }
    assert_eq!(
        (
            activity_calls.load(Ordering::Relaxed),
            preview_calls.load(Ordering::Relaxed)
        ),
        (0, 0)
    );
}

#[test]
fn expanded_shared_command_preserves_rich_output() {
    let mut command = crate::exec_cell::new_active_exec_command(
        "call".into(),
        vec!["echo output".into()],
        Vec::new(),
        codex_app_server_protocol::CommandExecutionSource::Agent,
        /*animations_enabled*/ false,
    );
    command.complete_call(
        "call",
        crate::exec_cell::CommandOutput::new(
            /*exit_code*/ 0,
            (0..10).map(|index| format!("output {index}\n")).collect(),
        ),
        Duration::from_millis(/*millis*/ 348),
    );
    let shared = LiveToolCell::new(Box::new(command));
    let mut viewport = ConversationViewport::new(
        vec![Arc::new(shared)],
        HistoryRenderMode::Rich,
        crate::keymap::RuntimeKeymap::defaults().pager,
    );
    viewport.expanded_tool_group = Some(0);
    viewport.refresh_tool_groups([0], /*width*/ 80);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 10,
    );
    let mut buffer = Buffer::empty(area);
    viewport.render(area, &mut buffer);
    let lines = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>();
    insta::assert_snapshot!(lines.join("\n").trim());
}
