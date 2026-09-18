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
fn panel_width_matches_claude_stable_layout() {
    assert_eq!(diff_panel_width(/*terminal_width*/ 109), None);
    assert_eq!(diff_panel_width(/*terminal_width*/ 110), Some(40));
    assert_eq!(diff_panel_width(/*terminal_width*/ 144), Some(64));
    assert_eq!(diff_panel_width(/*terminal_width*/ 200), Some(90));
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
    panel.set_result(Ok((true, sample_diff())));
    let render = |panel: &mut DiffPanel| {
        let mut buffer = Buffer::empty(area);
        panel.render(area, &mut buffer);
        buffer_text(&buffer, area)
    };

    let first = render(&mut panel);
    assert_eq!(
        panel.handle_left_click(Position::new(/*x*/ 48, /*y*/ 0)),
        DiffPanelClick::Close,
    );
    assert!(panel.jump_to_path(Path::new("src/b.rs")));
    let second = render(&mut panel);

    assert_snapshot!(format!("first file:\n{first}\nsecond file:\n{second}"));
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
