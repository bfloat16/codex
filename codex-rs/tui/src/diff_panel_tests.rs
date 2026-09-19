use super::*;

use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn buffer_text(buffer: &Buffer, area: Rect) -> String {
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn sample_diff() -> String {
    "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n\
     diff --git a/src/b.rs b/src/b.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/b.rs\n@@ -0,0 +1 @@\n+fn b() {}\n"
        .to_string()
}

#[test]
fn panel_width_is_forty_percent() {
    assert_eq!(diff_panel_width(/*terminal_width*/ 109), None);
    assert_eq!(diff_panel_width(/*terminal_width*/ 110), Some(44));
    assert_eq!(diff_panel_width(/*terminal_width*/ 144), Some(57));
    assert_eq!(diff_panel_width(/*terminal_width*/ 200), Some(80));
    assert_eq!(diff_panel_width(/*terminal_width*/ 400), Some(160));
}

#[test]
fn parses_file_sections_and_line_counts() {
    let files = parse_git_diff(&sample_diff());

    assert_eq!(
        files
            .iter()
            .map(|file| (file.path.clone(), file.added, file.removed))
            .collect::<Vec<_>>(),
        vec![
            (PathBuf::from("src/a.rs"), 1, 1),
            (PathBuf::from("src/b.rs"), 1, 0),
        ],
    );
}

#[test]
fn parses_windows_untracked_diff_path() {
    let files = parse_git_diff(
        "diff --git a/NUL b/a.txt\nnew file mode 100644\n--- NUL\n+++ b/a.txt\n@@ -0,0 +1 @@\n+new\n",
    );

    assert_eq!(
        files.into_iter().map(|file| file.path).collect::<Vec<_>>(),
        vec![PathBuf::from("a.txt")],
    );
}

#[test]
fn renders_file_navigation_and_continuous_diff() {
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 50, /*height*/ 18,
    );
    let mut panel = DiffPanel::loading();
    let render = |panel: &mut DiffPanel| {
        let mut buffer = Buffer::empty(area);
        panel.render(area, &mut buffer);
        buffer_text(&buffer, area)
    };

    let loading = render(&mut panel);
    panel.set_result(Ok((true, sample_diff())));
    let first = render(&mut panel);
    assert_eq!(
        panel.handle_left_click(Position::new(/*x*/ 47, /*y*/ 1)),
        DiffPanelClick::Close,
    );
    assert!(panel.jump_to_path(Path::new("src/b.rs")));
    let second = render(&mut panel);

    assert_snapshot!(format!(
        "loading:\n{loading}\nfirst file:\n{first}\nsecond file:\n{second}"
    ));
}

#[test]
fn hover_and_body_scroll_keep_file_navigation_in_sync() {
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 60, /*height*/ 18,
    );
    let mut panel = DiffPanel::loading();
    panel.set_result(Ok((true, sample_diff())));
    let mut buffer = Buffer::empty(area);
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || panel.render(area, &mut buffer),
    );

    let close_position = Position::new(panel.close_area.x, panel.close_area.y);
    assert!(panel.handle_mouse_move(close_position));
    let mut close_hovered = Buffer::empty(area);
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || panel.render(area, &mut close_hovered),
    );
    assert_eq!(buffer[close_position].symbol(), "✕");
    assert_eq!(close_hovered[close_position].symbol(), "✕");
    assert_eq!(close_hovered[close_position].bg, buffer[close_position].bg);
    assert_ne!(close_hovered[close_position].fg, buffer[close_position].fg);

    let first_file_row = panel.file_list_content_area.y;
    assert!(panel.handle_mouse_move(Position::new(4, first_file_row)));
    let mut hovered = Buffer::empty(area);
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
        },
        || panel.render(area, &mut hovered),
    );
    let row_x = panel.file_list_content_area.x;
    let panel_background = crate::terminal_palette::rgb_color((15, 15, 15));
    assert_eq!(buffer[(area.x, area.y)].bg, panel_background);
    assert!((row_x..panel.file_list_content_area.right()).all(|x| {
        buffer[(x, first_file_row)].bg == panel_background
            && hovered[(x, first_file_row)].bg == panel_background
    }));
    assert_eq!(
        hovered[(row_x, first_file_row)].fg,
        buffer[(row_x, first_file_row)].fg,
    );
    assert_ne!(
        hovered[(row_x.saturating_add(2), first_file_row)].fg,
        buffer[(row_x.saturating_add(2), first_file_row)].fg,
    );
    let delete_row = panel.body_area.y.saturating_add(2);
    let insert_row = panel.body_area.y.saturating_add(3);
    let delete_background = buffer[(area.x, delete_row)].bg;
    let insert_background = buffer[(area.x, insert_row)].bg;
    assert_ne!(delete_background, panel_background);
    assert_ne!(insert_background, panel_background);
    assert_ne!(delete_background, insert_background);
    assert!((area.x..area.right()).all(|x| {
        buffer[(x, delete_row)].bg == delete_background
            && buffer[(x, insert_row)].bg == insert_background
    }));

    let second_offset = panel
        .body_layout(panel.body_area.width)
        .and_then(|layout| layout.file_offsets.get(1))
        .copied()
        .expect("second file offset");
    panel.body_scroll = second_offset;
    panel.sync_selected_file_to_body_scroll();
    assert_eq!(panel.selected_file, Some(1));
}

#[test]
fn refresh_preserves_selected_file_scroll_and_last_ready_diff() {
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 60, /*height*/ 18,
    );
    let mut panel = DiffPanel::loading();
    panel.set_result(Ok((true, sample_diff())));
    panel.render(area, &mut Buffer::empty(area));
    assert!(panel.jump_to_path(Path::new("src/b.rs")));
    let selected_offset = panel
        .body_layout(panel.body_area.width)
        .and_then(|layout| layout.file_offsets.get(1))
        .copied()
        .expect("selected file offset");
    panel.body_scroll = selected_offset.saturating_add(1);

    panel.set_result(Ok((
        true,
        format!(
            "diff --git a/src/0.rs b/src/0.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/0.rs\n@@ -0,0 +1 @@\n+zero\n{}",
            sample_diff(),
        ),
    )));

    let selected_file = panel.selected_file.expect("selected file");
    let selected_path = panel
        .files()
        .and_then(|files| files.get(selected_file))
        .map(|file| file.path.clone());
    let refreshed_offset = panel
        .body_layout(panel.body_area.width)
        .and_then(|layout| layout.file_offsets.get(selected_file))
        .copied()
        .expect("refreshed file offset");
    assert_eq!(
        (
            selected_path,
            panel.body_scroll.saturating_sub(refreshed_offset),
        ),
        (Some(PathBuf::from("src/b.rs")), 1),
    );

    let mut before_failure = Buffer::empty(area);
    panel.render(area, &mut before_failure);
    panel.set_result(Err("git diff failed".to_string()));
    let mut after_failure = Buffer::empty(area);
    panel.render(area, &mut after_failure);
    assert_eq!(
        buffer_text(&after_failure, area),
        buffer_text(&before_failure, area),
    );
}

#[test]
fn path_matching_accepts_absolute_history_paths() {
    assert!(paths_match(
        Path::new("src/parser.rs"),
        Path::new(r"H:\Project\Software\codex\src\parser.rs"),
    ));
}

#[test]
fn long_file_list_scrolls_with_edge_cues() {
    let diff = (0..10)
        .map(|index| {
            format!(
                "diff --git a/src/{index}.rs b/src/{index}.rs\n--- a/src/{index}.rs\n+++ b/src/{index}.rs\n@@ -1 +1 @@\n-old\n+new\n"
            )
        })
        .collect::<String>();
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 50, /*height*/ 24,
    );
    let mut panel = DiffPanel::loading();
    panel.set_result(Ok((true, diff)));
    let mut buffer = Buffer::empty(area);
    panel.render(area, &mut buffer);
    assert!(buffer_text(&buffer, area).contains("↓ 2 more below"));

    assert!(
        panel.handle_mouse_scroll(Position::new(/*x*/ 4, /*y*/ 4), MouseScrollDirection::Down,)
    );
    let mut buffer = Buffer::empty(area);
    panel.render(area, &mut buffer);
    let text = buffer_text(&buffer, area);
    assert!(text.contains("↑ 1 more above"));
    assert!(text.contains("↓ 1 more below"));
}
