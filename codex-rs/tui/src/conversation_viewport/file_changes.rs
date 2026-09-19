//! Cached, collapsible file-change rendering for the owned full-screen transcript.

use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use super::ConversationCellRenderable;
use super::ConversationViewport;
use super::tool_groups::summary_text_style;
use crate::history_cell::FileChangeDisplayLines;
use crate::history_cell::HistoryRenderMode;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::HyperlinkParagraph;

impl ConversationViewport {
    pub(super) fn file_change_hit(&mut self, area: Rect, position: Position) -> Option<usize> {
        let (index, row) = self.content.renderable_at_position(area, position)?;
        let cell = self.cells.get(index)?;
        let top_padding = u16::from(index > 0 && !cell.is_stream_continuation());
        if row < top_padding {
            return None;
        }
        let lines = cell.file_change_display_lines(area.width)?;
        if lines.lines.len() <= lines.heading_rows {
            return None;
        }
        let content_row = usize::from(row.saturating_sub(top_padding));
        let footer_row = lines.lines.len();
        (content_row < lines.heading_rows
            || (self.expanded_file_changes.contains(&index) && content_row == footer_row))
            .then_some(index)
    }

    pub(super) fn toggle_file_change_at(&mut self, area: Rect, position: Position) -> bool {
        let Some(index) = self.file_change_hit(area, position) else {
            return false;
        };
        if !self.expanded_file_changes.remove(&index) {
            self.expanded_file_changes.insert(index);
        }
        self.refresh_file_changes([index], area.width);
        true
    }

    pub(super) fn set_hovered_file_change(&mut self, next: Option<usize>, width: u16) -> bool {
        if self.hovered_file_change == next {
            return false;
        }
        let previous = self.hovered_file_change;
        self.hovered_file_change = next;
        self.refresh_file_changes([previous, next].into_iter().flatten(), width);
        true
    }

    fn refresh_file_changes(&mut self, indexes: impl IntoIterator<Item = usize>, width: u16) {
        let mut indexes = indexes.into_iter().collect::<Vec<_>>();
        indexes.sort_unstable();
        indexes.dedup();
        for index in indexes.into_iter().rev() {
            if !self
                .cells
                .get(index)
                .is_some_and(|cell| cell.is_file_change())
            {
                continue;
            }
            let renderables = self.render_cell_range(index..index.saturating_add(1));
            self.content.replace_range(index, 1, renderables, width);
        }
    }

    pub(super) fn shift_file_change_state(&mut self, index: usize, inserted_count: usize) {
        if let Some(cell_index) = self.hovered_file_change.as_mut()
            && *cell_index >= index
        {
            *cell_index = cell_index.saturating_add(inserted_count);
        }
        self.expanded_file_changes = std::mem::take(&mut self.expanded_file_changes)
            .into_iter()
            .map(|cell_index| {
                if cell_index >= index {
                    cell_index.saturating_add(inserted_count)
                } else {
                    cell_index
                }
            })
            .collect();
    }

    pub(super) fn validate_file_change_state(&mut self) {
        if !self.hovered_file_change.is_some_and(|index| {
            self.cells
                .get(index)
                .is_some_and(|cell| cell.is_file_change())
        }) {
            self.hovered_file_change = None;
        }
        self.expanded_file_changes.retain(|index| {
            self.cells
                .get(*index)
                .is_some_and(|cell| cell.is_file_change())
        });
    }
}

impl ConversationCellRenderable {
    pub(super) fn file_change_desired_height(&self, width: u16) -> Option<u16> {
        let lines = self.file_change_lines(width)?;
        Some(
            u16::try_from(visible_file_change_rows(
                lines.lines.len(),
                lines.heading_rows,
                self.expanded_file_change,
            ))
            .unwrap_or(u16::MAX),
        )
    }

    pub(super) fn render_file_change(
        &self,
        area: Rect,
        buf: &mut Buffer,
        scroll_offset: u16,
    ) -> bool {
        let Some(lines) = self.file_change_lines(area.width) else {
            return false;
        };
        let collapsible = lines.lines.len() > lines.heading_rows;
        let content_rows = if collapsible && !self.expanded_file_change {
            lines.heading_rows
        } else {
            lines.lines.len()
        };
        let total_rows =
            content_rows.saturating_add(usize::from(collapsible && self.expanded_file_change));
        let start = usize::from(scroll_offset).min(total_rows);
        let end = start
            .saturating_add(usize::from(area.height))
            .min(total_rows);
        let interaction_style = collapsible.then(|| summary_text_style(self.hovered_file_change));
        let mut visible = Vec::with_capacity(end.saturating_sub(start));
        for row in start..end {
            if row < content_rows {
                let mut line = lines.lines[row].clone();
                if row < lines.heading_rows
                    && let Some(style) = interaction_style
                {
                    let line_style = if line.line.style.fg.is_none() {
                        line.line.style.patch(style)
                    } else {
                        line.line.style
                    };
                    line.line = Line::from(
                        line.line
                            .spans
                            .into_iter()
                            .map(|span| {
                                if span.style.fg.is_some() {
                                    span
                                } else {
                                    span.patch_style(style)
                                }
                            })
                            .collect::<Vec<_>>(),
                    )
                    .style(line_style);
                }
                visible.push(line);
            } else if self.expanded_file_change {
                visible.push(HyperlinkLine::from(
                    Line::from("  Show less ↑").style(interaction_style.unwrap_or_default()),
                ));
            }
        }
        HyperlinkParagraph::new(&visible, Default::default()).render(area, buf);
        true
    }

    fn file_change_lines(&self, width: u16) -> Option<FileChangeDisplayLines> {
        if self.render_mode != HistoryRenderMode::Rich {
            return None;
        }
        self.cell.file_change_display_lines(width)
    }
}

fn visible_file_change_rows(line_count: usize, heading_rows: usize, expanded: bool) -> usize {
    if expanded {
        line_count.saturating_add(1)
    } else {
        heading_rows
    }
}
