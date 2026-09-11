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
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::ToolActivity;
use crate::history_cell::UserHistoryCell;
use crate::tui::MouseInteractionKind;
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
        |_| Some(vec![HyperlinkLine::from("live tail")]),
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
    viewport.render(area, &mut collapsed);
    assert!(viewport.handle_mouse_interaction(
        area,
        Position::new(/*x*/ 4, /*y*/ 2),
        MouseInteractionKind::Move,
    ));
    let mut hovered = Buffer::empty(area);
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || viewport.render(area, &mut hovered),
    );
    assert!(
        (area.x..area.right()).all(|x| hovered[(x, 2)].style().bg.is_some()),
        "hover should tint the full summary row"
    );

    assert!(viewport.handle_mouse_interaction(
        area,
        Position::new(/*x*/ 4, /*y*/ 2),
        MouseInteractionKind::LeftClick,
    ));
    let mut expanded = Buffer::empty(area);
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || viewport.render(area, &mut expanded),
    );
    assert!(
        (area.x..area.right()).all(|x| expanded[(x, 4)].style().bg.is_some()),
        "expanded group background should cover blank horizontal space"
    );

    assert!(viewport.handle_mouse_interaction(
        area,
        Position::new(/*x*/ 40, /*y*/ 4),
        MouseInteractionKind::LeftClick,
    ));
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

• Read 2 files, ran 1 shell command

after




expanded:
before

read transcript detail

shell transcript detail

after


collapsed again:
before

• Read 2 files, ran 1 shell command

after




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

• Read 1 file, ran 1 shell command



backfilled:
before

• Read 1 file, ran 1 shell command

after

"###);
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
        |_| Some(vec![HyperlinkLine::from("live tail")]),
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
    viewport.sync_live_tail(/*width*/ 24, Some(key), |_| {
        Some(vec![HyperlinkLine::from("live tail")])
    });

    viewport.replace_cells(vec![cell("replacement")]);
    viewport.sync_live_tail(/*width*/ 24, Some(key), |_| {
        Some(vec![HyperlinkLine::from("live tail")])
    });

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
        |width| Some(live.display_hyperlink_lines(width)),
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
