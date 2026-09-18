use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::*;
use crate::chatwidget::ActiveToolDisplay;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::ToolActivity;
use crate::history_cell::UserHistoryCell;
use crate::tui::MouseScrollDirection;

#[derive(Debug)]
struct TestCell {
    display: &'static str,
    raw: &'static str,
    transcript: &'static str,
    is_stream_continuation: bool,
}

impl HistoryCell for TestCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.display.into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.raw.into()]
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.transcript.into()]
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_stream_continuation
    }
}

#[derive(Debug)]
struct BlockStyleCell;

impl HistoryCell for BlockStyleCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![
            "".into(),
            "› one two three".into(),
            "  four".into(),
            "  five six".into(),
            "  seven eight".into(),
            "".into(),
        ]
    }

    fn rich_block_style(&self) -> Option<Style> {
        Some(Style::default().bg(Color::Red))
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec!["raw source".into()]
    }
}

#[derive(Debug)]
struct DiffStyleCell;

impl HistoryCell for DiffStyleCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![
            Line::from("- removed").style(Style::default().bg(Color::Red)),
            Line::from("+ added").style(Style::default().bg(Color::Green)),
        ]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(/*width*/ 0)
    }
}

#[derive(Debug)]
struct HeightCountingCell {
    text: &'static str,
    height_calls: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct ToolTestCell {
    display: &'static str,
    transcript: &'static str,
    activity: ToolActivity,
}

#[derive(Debug)]
struct PreviewToolTestCell {
    detail: &'static str,
    preview: &'static str,
    activity: ToolActivity,
}

#[derive(Debug)]
struct DetailCountingToolCell {
    detail_calls: Arc<AtomicUsize>,
}

impl HistoryCell for ToolTestCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.display.into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.transcript.into()]
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.transcript.into()]
    }

    fn tool_activity(&self) -> Option<ToolActivity> {
        Some(self.activity)
    }
}

impl HistoryCell for PreviewToolTestCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.detail.into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.detail.into()]
    }

    fn tool_activity(&self) -> Option<ToolActivity> {
        Some(self.activity)
    }

    fn tool_group_preview_lines(&self) -> Vec<Line<'static>> {
        vec![self.preview.into()]
    }
}

impl HistoryCell for DetailCountingToolCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec!["tool".into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec!["tool".into()]
    }

    fn tool_activity(&self) -> Option<ToolActivity> {
        Some(ToolActivity {
            call_count: 1,
            shell_commands: 1,
            ..ToolActivity::default()
        })
    }

    fn tool_group_detail_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        self.detail_calls.fetch_add(1, Ordering::Relaxed);
        (0..40)
            .map(|index| HyperlinkLine::from(format!("output line {index}")))
            .collect()
    }
}

impl HistoryCell for HeightCountingCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.text.into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.text.into()]
    }

    fn desired_height_for_mode(&self, _width: u16, _mode: HistoryRenderMode) -> u16 {
        self.height_calls.fetch_add(1, Ordering::Relaxed);
        1
    }
}

fn cell(display: &'static str) -> Arc<dyn HistoryCell> {
    Arc::new(TestCell {
        display,
        raw: display,
        transcript: display,
        is_stream_continuation: false,
    })
}

fn viewport(cells: Vec<Arc<dyn HistoryCell>>) -> ConversationViewport {
    ConversationViewport::new(
        cells,
        HistoryRenderMode::Rich,
        crate::keymap::RuntimeKeymap::defaults().pager,
    )
}

fn tool_cell(
    display: &'static str,
    transcript: &'static str,
    activity: ToolActivity,
) -> Arc<dyn HistoryCell> {
    Arc::new(ToolTestCell {
        display,
        transcript,
        activity,
    })
}

fn preview_tool_cell(
    detail: &'static str,
    preview: &'static str,
    activity: ToolActivity,
) -> Arc<dyn HistoryCell> {
    Arc::new(PreviewToolTestCell {
        detail,
        preview,
        activity,
    })
}

fn live_display(lines: Vec<HyperlinkLine>) -> Option<ActiveCellDisplay> {
    Some(ActiveCellDisplay {
        lines,
        auxiliary_lines: Vec::new(),
        tool: None,
    })
}

#[test]
fn renders_main_display_and_live_tail_without_pager_chrome() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(TestCell {
        display: "compact display",
        raw: "raw source",
        transcript: "expanded transcript detail",
        is_stream_continuation: false,
    })];
    let mut viewport = viewport(cells);
    viewport.sync_live_tail(
        /*width*/ 32,
        Some(ActiveCellRenderKey {
            revision: 1,
            is_stream_continuation: false,
            animation_tick: None,
        }),
        ActiveToolGroupState::default(),
        |_| live_display(vec![HyperlinkLine::from("live tail")]),
    );
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 32, /*height*/ 6)).expect("create terminal");

    terminal
        .draw(|frame| viewport.render(frame.area(), frame.buffer_mut()))
        .expect("render conversation viewport");

    assert_snapshot!(terminal.backend(), @r###"
"compact display                 "
"                                "
"live tail                       "
"                                "
"                                "
"                                "
"###);
}

#[test]
fn switches_between_rich_and_raw_cell_representations() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(TestCell {
        display: "rich display",
        raw: "raw source",
        transcript: "expanded transcript detail",
        is_stream_continuation: false,
    })];
    let mut viewport = viewport(cells);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 3,
    );
    let mut rich = Buffer::empty(area);
    viewport.render(area, &mut rich);

    viewport.set_render_mode(HistoryRenderMode::Raw);
    let mut raw = Buffer::empty(area);
    viewport.render(area, &mut raw);

    assert!(buffer_text(&rich, area).contains("rich display"));
    assert!(buffer_text(&raw, area).contains("raw source"));
    assert!(!buffer_text(&raw, area).contains("expanded transcript detail"));
}

#[test]
fn adjacent_tools_collapse_then_expand_and_collapse_from_the_group_background() {
    let mut viewport = viewport(vec![
        cell("before"),
        tool_cell(
            "read display",
            "read transcript detail",
            ToolActivity {
                call_count: 1,
                read_files: 2,
                ..ToolActivity::default()
            },
        ),
        tool_cell(
            "shell display",
            "shell transcript detail",
            ToolActivity {
                call_count: 1,
                shell_commands: 1,
                ..ToolActivity::default()
            },
        ),
        cell("after"),
    ]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 44, /*height*/ 9,
    );

    let mut collapsed = Buffer::empty(area);
    let mut hovered = Buffer::empty(area);
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || {
            viewport.render(area, &mut collapsed);
            assert!(viewport.handle_mouse_move(area, Position::new(/*x*/ 4, /*y*/ 2)));
            viewport.render(area, &mut hovered);
        },
    );
    assert!((area.x..area.right()).all(|x| hovered[(x, 2)].bg == Color::Reset));
    assert_eq!(hovered[(0, 2)].fg, Color::Reset);
    assert_eq!(
        collapsed[(2, 2)].fg,
        crate::terminal_palette::rgb_color((173, 173, 173)),
    );
    assert_eq!(
        hovered[(2, 2)].fg,
        crate::terminal_palette::rgb_color((219, 219, 219)),
    );
    assert!(
        hovered[(7, 2)]
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert!(
        hovered[(20, 2)]
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );

    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 4, /*y*/ 2)));
    let mut expanded = Buffer::empty(area);
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || viewport.render(area, &mut expanded),
    );
    assert!(
        [1, 2, 3, 4, 5]
            .into_iter()
            .all(|y| { (area.x..area.right()).all(|x| expanded[(x, y)].style().bg.is_some()) }),
        "expanded group background should include top and bottom padding"
    );

    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 40, /*y*/ 5)));
    let mut collapsed_again = Buffer::empty(area);
    viewport.render(area, &mut collapsed_again);

    let trim_rows = |buffer: &Buffer| {
        buffer_text(buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_snapshot!(format!(
        "collapsed:\n{}\nexpanded:\n{}\ncollapsed again:\n{}",
        trim_rows(&collapsed),
        trim_rows(&expanded),
        trim_rows(&collapsed_again),
    ), @r###"
collapsed:
before

  Read 2 files, ran 1 shell command

after




expanded:
before

read transcript detail

shell transcript detail


after

collapsed again:
before

  Read 2 files, ran 1 shell command

after




"###);
}

#[test]
fn active_tool_group_merges_live_activity_and_hides_previews_when_closed() {
    let live_preview = "Call repo.inspect with a deliberately long argument list that would otherwise occupy more than two preview rows in the collapsed block";
    let live_activity = ToolActivity {
        call_count: 1,
        mcp_calls: 1,
        ..ToolActivity::default()
    };
    let mut viewport = viewport(vec![
        cell("before"),
        preview_tool_cell(
            "read detail",
            "Read src/lib.rs",
            ToolActivity {
                call_count: 1,
                read_files: 1,
                ..ToolActivity::default()
            },
        ),
        preview_tool_cell(
            "shell detail",
            "Run cargo check",
            ToolActivity {
                call_count: 1,
                shell_commands: 1,
                ..ToolActivity::default()
            },
        ),
        preview_tool_cell(
            "search detail",
            "Search hook discovery",
            ToolActivity {
                call_count: 1,
                searches: 1,
                ..ToolActivity::default()
            },
        ),
    ]);
    let active_state = ActiveToolGroupState {
        accepting_content: true,
        started_at: None,
        animations_enabled: false,
        animation_tick: None,
    };
    viewport.sync_live_tail(
        /*width*/ 64,
        Some(ActiveCellRenderKey {
            revision: 1,
            is_stream_continuation: false,
            animation_tick: None,
        }),
        active_state,
        |_| {
            Some(ActiveCellDisplay {
                lines: vec![HyperlinkLine::from("live MCP detail")],
                auxiliary_lines: Vec::new(),
                tool: Some(ActiveToolDisplay {
                    activity: live_activity,
                    preview_lines: vec![live_preview.into()],
                    detail_lines: vec![HyperlinkLine::from("live MCP detail")],
                    is_stream_continuation: false,
                }),
            })
        },
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 64, /*height*/ 12,
    );
    let render = |viewport: &mut ConversationViewport| {
        let mut buffer = Buffer::empty(area);
        viewport.render(area, &mut buffer);
        buffer_text(&buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };

    let active_collapsed = render(&mut viewport);
    assert!(!active_collapsed.contains("Read src/lib.rs"));
    assert!(!active_collapsed.contains("Run cargo check"));
    assert!(!active_collapsed.contains("Search hook discovery"));
    assert!(active_collapsed.contains("Call repo.inspect"));
    assert!(active_collapsed.contains('…'));
    assert!(!active_collapsed.contains("collapsed block"));

    assert!(viewport.handle_mouse_move(area, Position::new(/*x*/ 8, /*y*/ 4)));
    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 8, /*y*/ 4)));
    let active_expanded = render(&mut viewport);
    assert!(active_expanded.contains("read detail"));
    assert!(active_expanded.contains("live MCP detail"));
    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 40, /*y*/ 8)));

    viewport.push_cell(preview_tool_cell(
        "live MCP detail",
        live_preview,
        live_activity,
    ));
    viewport.sync_live_tail(
        /*width*/ 64,
        /*active_key*/ None,
        active_state,
        |_| None,
    );
    let active_between_tools = render(&mut viewport);
    assert!(active_between_tools.contains("Call repo.inspect"));
    assert!(active_between_tools.contains('…'));

    viewport.sync_live_tail(
        /*width*/ 64,
        /*active_key*/ None,
        ActiveToolGroupState::default(),
        |_| None,
    );
    let inactive = render(&mut viewport);
    assert!(!inactive.contains("Search hook discovery"));
    assert!(!inactive.contains("Call repo.inspect"));

    assert_snapshot!(format!(
        "active collapsed:\n{active_collapsed}\nactive expanded:\n{active_expanded}\nactive between tools:\n{active_between_tools}\ninactive:\n{inactive}",
    ), @r###"
active collapsed:
before

● Searched for 1 pattern, read 1 file, called 1 MCP tool, ran 1
  shell command
  └ Call repo.inspect with a deliberately long argument list
    that would otherwise occupy more than two preview rows in…






active expanded:
before

read detail

shell detail

search detail

live MCP detail



active between tools:
before

● Searched for 1 pattern, read 1 file, called 1 MCP tool, ran 1
  shell command
  └ Call repo.inspect with a deliberately long argument list
    that would otherwise occupy more than two preview rows in…






inactive:
before

  Searched for 1 pattern, read 1 file, called 1 MCP tool, ran 1
  shell command









"###);
}

#[test]
fn first_live_tool_uses_the_clickable_collapsed_group() {
    let mut viewport = viewport(vec![cell("before")]);
    viewport.sync_live_tail(
        /*width*/ 48,
        Some(ActiveCellRenderKey {
            revision: 1,
            is_stream_continuation: false,
            animation_tick: None,
        }),
        ActiveToolGroupState {
            accepting_content: true,
            started_at: None,
            animations_enabled: false,
            animation_tick: None,
        },
        |_| {
            Some(ActiveCellDisplay {
                lines: vec![HyperlinkLine::from("live detail")],
                auxiliary_lines: Vec::new(),
                tool: Some(ActiveToolDisplay {
                    activity: ToolActivity {
                        call_count: 1,
                        shell_commands: 1,
                        ..ToolActivity::default()
                    },
                    preview_lines: vec!["Run just test".into()],
                    detail_lines: vec![HyperlinkLine::from("live detail")],
                    is_stream_continuation: false,
                }),
            })
        },
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 48, /*height*/ 6,
    );
    let mut collapsed = Buffer::empty(area);
    viewport.render(area, &mut collapsed);
    assert!(buffer_text(&collapsed, area).contains("● Ran 1 shell command"));
    assert!(buffer_text(&collapsed, area).contains("└ Run just test"));

    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 8, /*y*/ 3)));
    let mut expanded = Buffer::empty(area);
    viewport.render(area, &mut expanded);
    assert!(buffer_text(&expanded, area).contains("live detail"));
    assert!(!buffer_text(&expanded, area).contains("Run just test"));
}

#[test]
fn single_tools_and_file_edits_are_collapsed_by_default() {
    let cwd = crate::test_support::test_path_buf("/tmp/project");
    let patch = crate::history_cell::new_patch_event(
        std::collections::HashMap::from([(
            std::path::PathBuf::from("src/lib.rs"),
            crate::diff_model::FileChange::Add {
                content: "pub fn added() {}\n".to_string(),
            },
        )]),
        &cwd,
    );
    let cells: Vec<Arc<dyn HistoryCell>> = vec![
        tool_cell(
            "read display",
            "read transcript detail",
            ToolActivity {
                call_count: 1,
                read_files: 1,
                ..ToolActivity::default()
            },
        ),
        Arc::new(patch),
        tool_cell(
            "shell display",
            "shell transcript detail",
            ToolActivity {
                call_count: 1,
                shell_commands: 1,
                ..ToolActivity::default()
            },
        ),
    ];
    let mut viewport = viewport(cells);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 44, /*height*/ 8,
    );
    let mut buffer = Buffer::empty(area);

    viewport.render(area, &mut buffer);

    assert_snapshot!(
        buffer_text(&buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
        @r###"
  Read 1 file

● Added src/lib.rs (+1 -0)

  Ran 1 shell command


"###
    );
}

#[test]
fn large_file_edits_cache_fold_hover_expand_and_collapse() {
    let cwd = crate::test_support::test_path_buf("/tmp/project");
    let content = (1..=55)
        .map(|line| format!("let value_{line:02} = {line};\n"))
        .collect::<String>();
    let patch = crate::history_cell::new_patch_event(
        std::collections::HashMap::from([(
            std::path::PathBuf::from("src/large.rs"),
            crate::diff_model::FileChange::Add { content },
        )]),
        &cwd,
    );
    let first_layout = patch
        .file_change_display_lines(/*width*/ 64)
        .expect("patch display lines");
    let cached_layout = patch
        .file_change_display_lines(/*width*/ 64)
        .expect("cached patch display lines");
    assert!(Arc::ptr_eq(&first_layout.lines, &cached_layout.lines));

    let mut viewport = viewport(vec![Arc::new(patch)]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 64, /*height*/ 6,
    );
    viewport.scroll_rows(MouseScrollDirection::Up, usize::MAX);
    let render = |viewport: &mut ConversationViewport| {
        let mut buffer = Buffer::empty(area);
        crate::terminal_palette::with_test_default_colors(
            crate::terminal_probe::DefaultColors {
                fg: (255, 255, 255),
                bg: (0, 0, 0),
            },
            || viewport.render(area, &mut buffer),
        );
        buffer
    };

    let collapsed_top = render(&mut viewport);
    assert_eq!(
        collapsed_top[(2, 0)].fg,
        crate::terminal_palette::rgb_color((173, 173, 173)),
    );
    assert_eq!(collapsed_top[(22, 0)].fg, Color::Green);
    assert_eq!(collapsed_top[(26, 0)].fg, Color::Red);
    assert!(viewport.handle_mouse_move(area, Position::new(/*x*/ 4, /*y*/ 0)));
    let hovered_top = render(&mut viewport);
    assert_eq!(
        hovered_top[(2, 0)].fg,
        crate::terminal_palette::rgb_color((219, 219, 219)),
    );
    assert_eq!(hovered_top[(22, 0)].fg, Color::Green);
    assert_eq!(hovered_top[(26, 0)].fg, Color::Red);
    assert!(!viewport.handle_left_click(area, Position::new(/*x*/ 12, /*y*/ 2)));

    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 4, /*y*/ 0)));
    let expanded_top = render(&mut viewport);
    viewport.scroll_to_bottom();
    let expanded_tail = render(&mut viewport);
    assert!(buffer_text(&expanded_tail, area).contains("Show less ↑"));
    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 4, /*y*/ 5)));
    let collapsed_tail = render(&mut viewport);
    assert!(!buffer_text(&collapsed_tail, area).contains("Show more"));

    let frame = |buffer: &Buffer| {
        buffer_text(buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_snapshot!(format!(
        "collapsed top:\n{}\nhovered top:\n{}\nexpanded top:\n{}\nexpanded tail:\n{}\ncollapsed tail:\n{}",
        frame(&collapsed_top),
        frame(&hovered_top),
        frame(&expanded_top),
        frame(&expanded_tail),
        frame(&collapsed_tail),
    ));
}

#[test]
fn small_file_edits_are_collapsed_and_interactive() {
    let cwd = crate::test_support::test_path_buf("/tmp/project");
    let content = (1..=48)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let patch = crate::history_cell::new_patch_event(
        std::collections::HashMap::from([(
            std::path::PathBuf::from("src/small.txt"),
            crate::diff_model::FileChange::Add { content },
        )]),
        &cwd,
    );
    let mut viewport = viewport(vec![Arc::new(patch)]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 64, /*height*/ 4,
    );
    viewport.scroll_rows(MouseScrollDirection::Up, usize::MAX);
    let mut collapsed = Buffer::empty(area);
    viewport.render(area, &mut collapsed);

    assert!(!buffer_text(&collapsed, area).contains("line 1"));
    assert!(!buffer_text(&collapsed, area).contains("Show more"));
    assert!(viewport.handle_mouse_move(area, Position::new(/*x*/ 4, /*y*/ 0)));
    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 4, /*y*/ 0)));
    let mut expanded = Buffer::empty(area);
    viewport.render(area, &mut expanded);
    assert!(buffer_text(&expanded, area).contains("line 1"));
}

#[test]
fn background_terminal_waits_fold_with_adjacent_tools() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(crate::history_cell::new_unified_exec_interaction(
            Some("worker command".to_string()),
            String::new(),
        )),
        tool_cell(
            "shell display",
            "shell transcript detail",
            ToolActivity {
                call_count: 1,
                shell_commands: 1,
                ..ToolActivity::default()
            },
        ),
    ];
    let mut viewport = viewport(cells);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 56, /*height*/ 6,
    );
    let mut collapsed = Buffer::empty(area);
    viewport.render(area, &mut collapsed);
    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 4, /*y*/ 0)));
    let mut expanded = Buffer::empty(area);
    viewport.render(area, &mut expanded);

    let trim_rows = |buffer: &Buffer| {
        buffer_text(buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_snapshot!(format!(
        "collapsed:\n{}\nexpanded:\n{}",
        trim_rows(&collapsed),
        trim_rows(&expanded),
    ), @r###"
collapsed:
  Waited for 1 background terminal, ran 1 shell command





expanded:

● Waited for background terminal · worker command

shell transcript detail


"###);
}

#[test]
fn raw_mode_keeps_adjacent_tool_details_unfolded() {
    let cells = vec![
        tool_cell(
            "read display",
            "read raw detail",
            ToolActivity {
                call_count: 1,
                read_files: 1,
                ..ToolActivity::default()
            },
        ),
        tool_cell(
            "shell display",
            "shell raw detail",
            ToolActivity {
                call_count: 1,
                shell_commands: 1,
                ..ToolActivity::default()
            },
        ),
    ];
    let mut viewport = ConversationViewport::new(
        cells,
        HistoryRenderMode::Raw,
        crate::keymap::RuntimeKeymap::defaults().pager,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 30, /*height*/ 4,
    );
    let mut buffer = Buffer::empty(area);

    viewport.render(area, &mut buffer);

    let rendered = buffer_text(&buffer, area)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    assert_snapshot!(rendered, @r###"
read raw detail

shell raw detail
"###);
}

#[test]
fn appending_and_backfilling_tools_rebuild_only_the_adjacent_group() {
    let read = || {
        tool_cell(
            "read detail",
            "read transcript",
            ToolActivity {
                call_count: 1,
                read_files: 1,
                ..ToolActivity::default()
            },
        )
    };
    let shell = || {
        tool_cell(
            "shell detail",
            "shell transcript",
            ToolActivity {
                call_count: 1,
                shell_commands: 1,
                ..ToolActivity::default()
            },
        )
    };
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 6,
    );
    let mut appended = viewport(vec![cell("before"), read()]);

    appended.push_cell(shell());
    let mut appended_buffer = Buffer::empty(area);
    appended.render(area, &mut appended_buffer);

    let mut backfilled = viewport(vec![cell("before"), shell(), cell("after")]);
    backfilled.insert_cells(/*index*/ 1, vec![read()], area.width);
    let mut backfilled_buffer = Buffer::empty(area);
    backfilled.render(area, &mut backfilled_buffer);

    let trim_rows = |buffer: &Buffer| {
        buffer_text(buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_snapshot!(format!(
        "appended:\n{}\nbackfilled:\n{}",
        trim_rows(&appended_buffer),
        trim_rows(&backfilled_buffer),
    ), @r###"
appended:
before

  Read 1 file, ran 1 shell command



backfilled:
before

  Read 1 file, ran 1 shell command

after

"###);
}

#[test]
fn expanded_tool_group_reuses_detail_layout_while_scrolling() {
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let tools = (0..2)
        .map(|_| {
            Arc::new(DetailCountingToolCell {
                detail_calls: Arc::clone(&detail_calls),
            }) as Arc<dyn HistoryCell>
        })
        .collect();
    let mut viewport = viewport(tools);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 30, /*height*/ 5,
    );

    viewport.render(area, &mut Buffer::empty(area));
    assert_eq!(detail_calls.load(Ordering::Relaxed), 0);
    assert!(viewport.handle_left_click(area, Position::new(/*x*/ 2, /*y*/ 0)));
    viewport.render(area, &mut Buffer::empty(area));
    for _ in 0..10 {
        viewport.scroll_rows(MouseScrollDirection::Down, /*rows*/ 3);
        viewport.render(area, &mut Buffer::empty(area));
    }
    assert_eq!(detail_calls.load(Ordering::Relaxed), 2);

    let narrow = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 20, /*height*/ 5,
    );
    viewport.render(narrow, &mut Buffer::empty(narrow));
    assert_eq!(detail_calls.load(Ordering::Relaxed), 4);
}

#[test]
fn rich_block_style_fills_owned_cell_without_leaking_to_raw_or_following_cells() {
    let user_cell = UserHistoryCell {
        message: "prompt".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    };
    assert!(user_cell.rich_block_style().is_some());

    let mut viewport = viewport(vec![Arc::new(BlockStyleCell), cell("assistant")]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 16, /*height*/ 8,
    );
    let mut rich = Buffer::empty(area);
    viewport.render(area, &mut rich);

    viewport.set_render_mode(HistoryRenderMode::Raw);
    let mut raw = Buffer::empty(area);
    viewport.render(area, &mut raw);

    let trim_rows = |buffer: &Buffer| {
        buffer_text(buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let background_mask = |buffer: &Buffer| {
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| match buffer[(x, y)].style().bg {
                        Some(Color::Red) => '#',
                        _ => '.',
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    assert_snapshot!(format!(
        "rich text:\n{}\nrich background:\n{}\nraw text:\n{}\nraw background:\n{}",
        trim_rows(&rich),
        background_mask(&rich),
        trim_rows(&raw),
        background_mask(&raw),
    ), @r###"
rich text:

› one two three
  four
  five six
  seven eight


assistant
rich background:
################
################
################
################
################
################
................
................
raw text:
raw source

assistant





raw background:
................
................
................
................
................
................
................
................
"###);
}

#[test]
fn line_backgrounds_fill_the_owned_viewport_width() {
    let mut viewport = viewport(vec![Arc::new(DiffStyleCell)]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 14, /*height*/ 2,
    );
    let mut buffer = Buffer::empty(area);

    viewport.render(area, &mut buffer);

    let background_mask = (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| match buffer[(x, y)].style().bg {
                    Some(Color::Red) => '-',
                    Some(Color::Green) => '+',
                    _ => '.',
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_snapshot!(background_mask, @r###"
    --------------
    ++++++++++++++
    "###);
}

#[test]
fn append_keeps_live_tail_after_committed_cells() {
    let mut viewport = viewport(Vec::new());
    viewport.sync_live_tail(
        /*width*/ 24,
        Some(ActiveCellRenderKey {
            revision: 1,
            is_stream_continuation: false,
            animation_tick: None,
        }),
        ActiveToolGroupState::default(),
        |_| live_display(vec![HyperlinkLine::from("live tail")]),
    );

    viewport.push_cell(cell("committed"));

    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 5,
    );
    let mut buffer = Buffer::empty(area);
    viewport.render(area, &mut buffer);
    let text = buffer_text(&buffer, area);
    let committed = text
        .find("committed")
        .expect("committed cell should render");
    let live_tail = text.find("live tail").expect("live tail should render");
    assert!(committed < live_tail);
    assert_eq!(viewport.committed_cell_count(), 1);
}

#[test]
fn replacing_cells_invalidates_and_respaces_the_live_tail() {
    let key = ActiveCellRenderKey {
        revision: 1,
        is_stream_continuation: false,
        animation_tick: None,
    };
    let mut viewport = viewport(Vec::new());
    viewport.sync_live_tail(
        /*width*/ 24,
        Some(key),
        ActiveToolGroupState::default(),
        |_| live_display(vec![HyperlinkLine::from("live tail")]),
    );

    viewport.replace_cells(vec![cell("replacement")]);
    viewport.sync_live_tail(
        /*width*/ 24,
        Some(key),
        ActiveToolGroupState::default(),
        |_| live_display(vec![HyperlinkLine::from("live tail")]),
    );

    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 4,
    );
    let mut buffer = Buffer::empty(area);
    viewport.render(area, &mut buffer);
    let rows = buffer_text(&buffer, area);
    assert_eq!(
        rows.lines().map(str::trim_end).collect::<Vec<_>>(),
        vec!["replacement", "", "live tail", ""]
    );
}

#[test]
fn inserting_older_cells_preserves_existing_layout_measurements() {
    let existing_height_calls = Arc::new(AtomicUsize::new(0));
    let older_height_calls = Arc::new(AtomicUsize::new(0));
    let mut viewport = viewport(vec![
        Arc::new(HeightCountingCell {
            text: "session header",
            height_calls: existing_height_calls.clone(),
        }),
        Arc::new(HeightCountingCell {
            text: "recent",
            height_calls: existing_height_calls.clone(),
        }),
    ]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 5,
    );

    viewport.render(area, &mut Buffer::empty(area));
    assert_eq!(existing_height_calls.load(Ordering::Relaxed), 2);

    viewport.insert_cells(
        /*index*/ 1,
        vec![Arc::new(HeightCountingCell {
            text: "older",
            height_calls: older_height_calls.clone(),
        })],
        area.width,
    );
    viewport.render(area, &mut Buffer::empty(area));

    assert_eq!(existing_height_calls.load(Ordering::Relaxed), 2);
    assert_eq!(older_height_calls.load(Ordering::Relaxed), 1);
    assert_eq!(viewport.committed_cell_count(), 3);
}

#[test]
fn inserting_older_cells_preserves_the_visible_scroll_anchor() {
    let mut viewport = viewport(
        [
            "header",
            "recent one",
            "recent two",
            "recent three",
            "latest",
        ]
        .map(cell)
        .to_vec(),
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 4,
    );
    viewport.render(area, &mut Buffer::empty(area));
    viewport.scroll_rows(MouseScrollDirection::Up, /*rows*/ 3);
    let mut before = Buffer::empty(area);
    viewport.render(area, &mut before);

    viewport.insert_cells(
        /*index*/ 1,
        vec![cell("older one"), cell("older two")],
        area.width,
    );
    let mut after = Buffer::empty(area);
    viewport.render(area, &mut after);

    assert_eq!(after, before);
}

#[test]
fn preserves_semantic_links_for_committed_and_live_content() {
    let committed_destination = "https://example.com/committed";
    let live_destination = "https://example.com/live";
    let committed: Arc<dyn HistoryCell> = Arc::new(AgentMarkdownCell::new(
        committed_destination.to_string(),
        std::path::Path::new("/tmp"),
    ));
    let live = AgentMarkdownCell::new(live_destination.to_string(), std::path::Path::new("/tmp"));
    let mut viewport = viewport(vec![committed]);
    viewport.sync_live_tail(
        /*width*/ 28,
        Some(ActiveCellRenderKey {
            revision: 1,
            is_stream_continuation: false,
            animation_tick: None,
        }),
        ActiveToolGroupState::default(),
        |width| live_display(live.display_hyperlink_lines(width)),
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 28, /*height*/ 6,
    );
    let mut buffer = Buffer::empty(area);

    viewport.render(area, &mut buffer);

    let rendered = buffer_text(&buffer, area);
    assert!(rendered.contains(&format!("\x1b]8;;{committed_destination}\x07")));
    assert!(rendered.contains(&format!("\x1b]8;;{live_destination}\x07")));
}

#[test]
fn narrower_resize_stays_pinned_to_the_latest_cell() {
    let mut viewport = viewport(vec![
        cell("first long row that wraps"),
        cell("second long row that wraps"),
        cell("LATEST SENTINEL"),
    ]);
    let wide = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 4,
    );
    viewport.render(wide, &mut Buffer::empty(wide));

    let narrow = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 12, /*height*/ 4,
    );
    let mut buffer = Buffer::empty(narrow);
    viewport.render(narrow, &mut buffer);

    assert!(viewport.is_following_bottom());
    assert!(buffer_text(&buffer, narrow).contains("SENTINEL"));
}

#[test]
fn page_navigation_leaves_and_restores_bottom_follow() {
    let mut viewport = viewport(vec![cell("oldest"), cell("middle"), cell("LATEST")]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 20, /*height*/ 2,
    );
    let mut bottom = Buffer::empty(area);
    viewport.render(area, &mut bottom);
    assert!(buffer_text(&bottom, area).contains("LATEST"));

    assert!(
        viewport.handle_navigation_key(area, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),)
    );
    let mut scrolled = Buffer::empty(area);
    viewport.render(area, &mut scrolled);
    assert!(!viewport.is_following_bottom());
    assert!(buffer_text(&scrolled, area).contains("middle"));

    assert!(
        viewport.handle_navigation_key(area, KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),)
    );
    let mut restored = Buffer::empty(area);
    viewport.render(area, &mut restored);
    assert!(viewport.is_following_bottom());
    let trim_rows = |buffer: &Buffer| {
        buffer_text(buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_snapshot!(format!(
        "bottom:\n{}\nscrolled:\n{}\nrestored:\n{}",
        trim_rows(&bottom),
        trim_rows(&scrolled),
        trim_rows(&restored),
    ), @r###"
bottom:

LATEST
scrolled:

middle
restored:

LATEST
"###);
}

#[test]
fn mouse_wheel_leaves_and_restores_bottom_follow() {
    let mut viewport = viewport(vec![
        cell("oldest"),
        cell("older"),
        cell("middle"),
        cell("newer"),
        cell("LATEST"),
    ]);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 20, /*height*/ 3,
    );
    let mut bottom = Buffer::empty(area);
    viewport.render(area, &mut bottom);

    viewport.scroll_rows(MouseScrollDirection::Up, /*rows*/ 3);
    let mut scrolled = Buffer::empty(area);
    viewport.render(area, &mut scrolled);
    assert!(!viewport.is_following_bottom());

    viewport.scroll_rows(MouseScrollDirection::Down, /*rows*/ 3);
    let mut restored = Buffer::empty(area);
    viewport.render(area, &mut restored);
    assert!(viewport.is_following_bottom());

    let trim_rows = |buffer: &Buffer| {
        buffer_text(buffer, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_snapshot!(format!(
        "bottom:\n{}\nscrolled:\n{}\nrestored:\n{}",
        trim_rows(&bottom),
        trim_rows(&scrolled),
        trim_rows(&restored),
    ), @r###"
bottom:
newer

LATEST
scrolled:

middle

restored:
newer

LATEST
"###);
}

fn buffer_text(buffer: &Buffer, area: Rect) -> String {
    let mut rows = Vec::new();
    for y in area.y..area.bottom() {
        let mut row = String::new();
        for x in area.x..area.right() {
            row.push_str(buffer[(x, y)].symbol());
        }
        rows.push(row);
    }
    rows.join("\n")
}
