//! Mouse-driven text selection for the application-owned transcript.

use ratatui::buffer::Buffer;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

use crate::terminal_hyperlinks::strip_osc8;

const SELECTION_BLUE: (u8, u8, u8) = (38, 79, 120);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SelectionPoint {
    row: u16,
    column: u16,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ScreenSnapshot {
    rows: Vec<ScreenRow>,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ScreenRow {
    text: String,
    column_ranges: Vec<Range<usize>>,
}

#[derive(Debug, Default)]
pub(super) struct ScreenTextSelection {
    anchor: Option<SelectionPoint>,
    head: Option<SelectionPoint>,
    pointer_down: bool,
    dragged: bool,
    completed: bool,
    completed_text: Option<String>,
    snapshot: ScreenSnapshot,
}

pub(super) enum SelectionRelease {
    Ignored,
    Click(Position),
    Copy(String),
    Redraw,
}

impl ScreenTextSelection {
    pub(super) fn left_down(&mut self, area: Rect, position: Position) -> bool {
        let Some(point) = local_point(area, position) else {
            return false;
        };
        self.anchor = Some(point);
        self.head = Some(point);
        self.pointer_down = true;
        self.dragged = false;
        self.completed = false;
        self.completed_text = None;
        true
    }

    pub(super) fn left_drag(&mut self, area: Rect, position: Position) -> bool {
        if !self.pointer_down || area.is_empty() {
            return false;
        }
        let point = clamped_local_point(area, position);
        let changed = self.head != Some(point);
        self.head = Some(point);
        self.dragged |= self.anchor != self.head;
        changed
    }

    pub(super) fn left_up(&mut self, area: Rect, position: Position) -> SelectionRelease {
        if !self.pointer_down {
            return SelectionRelease::Ignored;
        }
        self.pointer_down = false;
        if !area.is_empty() {
            let point = clamped_local_point(area, position);
            self.head = Some(point);
            self.dragged |= self.anchor != self.head;
        }
        if self.dragged
            && let Some(text) = self.selected_text()
        {
            self.completed = true;
            self.completed_text = Some(text.clone());
            return SelectionRelease::Copy(text);
        }

        self.clear();
        if area.contains(position) {
            SelectionRelease::Click(position)
        } else {
            SelectionRelease::Redraw
        }
    }

    /// Select the word under a double-click, matching terminal selection semantics.
    pub(super) fn select_word(&mut self, area: Rect, position: Position) -> Option<String> {
        let point = local_point(area, position)?;
        let row = self.snapshot.rows.get(usize::from(point.row))?;
        let range = row.column_ranges.get(usize::from(point.column))?;
        if range.start == range.end {
            return None;
        }
        let ch = row.text[range.clone()].chars().next()?;
        let class = selection_class(ch);
        let column = usize::from(point.column);
        let mut start = column;
        while start > 0 {
            let Some(previous) = row.column_ranges.get(start - 1) else {
                break;
            };
            if previous.start == previous.end {
                break;
            }
            let Some(previous_char) = row.text[previous.clone()].chars().next() else {
                break;
            };
            if selection_class(previous_char) != class {
                break;
            }
            start -= 1;
        }
        let mut end = column;
        while end + 1 < row.column_ranges.len() {
            let Some(next) = row.column_ranges.get(end + 1) else {
                break;
            };
            if next.start == next.end {
                break;
            }
            let Some(next_char) = row.text[next.clone()].chars().next() else {
                break;
            };
            if selection_class(next_char) != class {
                break;
            }
            end += 1;
        }
        self.anchor = Some(SelectionPoint {
            row: point.row,
            column: u16::try_from(start).unwrap_or(u16::MAX),
        });
        self.head = Some(SelectionPoint {
            row: point.row,
            column: u16::try_from(end).unwrap_or(u16::MAX),
        });
        self.pointer_down = false;
        self.dragged = true;
        self.completed = true;
        let text = self.selected_text();
        self.completed_text = text.clone();
        text
    }

    pub(super) fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.pointer_down = false;
        self.dragged = false;
        self.completed = false;
        self.completed_text = None;
    }

    pub(super) fn capture_and_render(&mut self, area: Rect, buffer: &mut Buffer) {
        let snapshot = ScreenSnapshot::capture(area, buffer);
        self.snapshot = snapshot;
        if self.completed
            && !self.pointer_down
            && self.completed_text.as_deref() != self.selected_text().as_deref()
        {
            self.clear();
        }
        if !self.dragged && !self.completed {
            return;
        }
        let Some((start, end)) = self.normalized_range() else {
            return;
        };
        let background = selection_background();
        for row in start.row..=end.row {
            let start_column = if row == start.row { start.column } else { 0 };
            let end_column = if row == end.row {
                end.column
            } else {
                area.width.saturating_sub(1)
            };
            for column in start_column..=end_column {
                let x = area.x.saturating_add(column);
                let y = area.y.saturating_add(row);
                if x < area.right() && y < area.bottom() {
                    buffer[(x, y)].set_bg(background);
                }
            }
        }
    }

    fn normalized_range(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let anchor = self.anchor?;
        let head = self.head?;
        Some(if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        })
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.normalized_range()?;
        let row_count = usize::from(end.row)
            .saturating_sub(usize::from(start.row))
            .saturating_add(1);
        let mut selected_rows = Vec::with_capacity(row_count);
        for row in start.row..=end.row {
            let screen_row = self.snapshot.rows.get(usize::from(row))?;
            let start_column = if row == start.row {
                usize::from(start.column)
            } else {
                0
            };
            let end_column = if row == end.row {
                usize::from(end.column).saturating_add(1)
            } else {
                screen_row.column_ranges.len()
            }
            .min(screen_row.column_ranges.len());
            let start_byte = screen_row.column_ranges.get(start_column)?.start;
            let end_byte = screen_row
                .column_ranges
                .get(end_column.saturating_sub(1))?
                .end;
            let line = screen_row.text.get(start_byte..end_byte)?;
            selected_rows.push(line.trim_end_matches(' ').to_string());
        }
        let text = selected_rows.join("\n");
        (!text.is_empty()).then_some(text)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionClass {
    Whitespace,
    Word,
    Other,
}

fn selection_class(ch: char) -> SelectionClass {
    if ch.is_whitespace() {
        SelectionClass::Whitespace
    } else if ch.is_alphanumeric() || "_/.-+~\\".contains(ch) {
        SelectionClass::Word
    } else {
        SelectionClass::Other
    }
}

impl ScreenSnapshot {
    fn capture(area: Rect, buffer: &Buffer) -> Self {
        let rows = (area.y..area.bottom())
            .map(|y| {
                let mut text = String::with_capacity(usize::from(area.width));
                let mut column_ranges = Vec::with_capacity(usize::from(area.width));
                let mut previous_range = 0..0;
                let mut trailing_columns = 0usize;
                for x in area.x..area.right() {
                    let cell = &buffer[(x, y)];
                    let range = if cell.diff_option == CellDiffOption::Skip || trailing_columns > 0
                    {
                        trailing_columns = trailing_columns.saturating_sub(1);
                        previous_range.clone()
                    } else {
                        let start = text.len();
                        let symbol = cell.symbol();
                        let visible_symbol = if symbol.contains('\x1b') {
                            strip_osc8(symbol)
                        } else {
                            symbol.to_string()
                        };
                        text.push_str(&visible_symbol);
                        trailing_columns = UnicodeWidthStr::width(visible_symbol.as_str())
                            .max(1)
                            .saturating_sub(1);
                        start..text.len()
                    };
                    previous_range = range.clone();
                    column_ranges.push(range);
                }
                ScreenRow {
                    text,
                    column_ranges,
                }
            })
            .collect();
        Self { rows }
    }
}

fn local_point(area: Rect, position: Position) -> Option<SelectionPoint> {
    area.contains(position).then(|| SelectionPoint {
        row: position.y.saturating_sub(area.y),
        column: position.x.saturating_sub(area.x),
    })
}

fn clamped_local_point(area: Rect, position: Position) -> SelectionPoint {
    SelectionPoint {
        row: position
            .y
            .clamp(area.y, area.bottom().saturating_sub(1))
            .saturating_sub(area.y),
        column: position
            .x
            .clamp(area.x, area.right().saturating_sub(1))
            .saturating_sub(area.x),
    }
}

pub(super) fn selection_background() -> Color {
    match crate::terminal_palette::best_color(SELECTION_BLUE) {
        Color::Reset => Color::Blue,
        color => color,
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
