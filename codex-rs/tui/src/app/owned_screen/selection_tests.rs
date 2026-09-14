use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::*;

#[test]
fn double_click_selection_matches_terminal_word_boundaries() {
    let area = Rect::new(0, 0, 24, 1);
    let mut buffer = Buffer::empty(area);
    buffer.set_string(0, 0, "run foo_bar --flag now", Style::default());

    let mut selection = ScreenTextSelection::default();
    selection.capture_and_render(area, &mut buffer);

    assert_eq!(
        selection.select_word(area, Position::new(7, 0)),
        Some("foo_bar".to_string())
    );

    selection.clear();
    assert_eq!(
        selection.select_word(area, Position::new(14, 0)),
        Some("--flag".to_string())
    );
}
