//! Collapsible adjacent-tool projection for the owned full-screen transcript.

use std::ops::Range;
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

use super::ConversationViewport;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::ToolActivity;
use crate::render::renderable::Renderable;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::HyperlinkParagraph;
use crate::terminal_hyperlinks::plain_hyperlink_lines;

const MIN_GROUPED_TOOL_CALLS: usize = 2;

impl ConversationViewport {
    pub(super) fn render_cell_range(&self, range: Range<usize>) -> Vec<Box<dyn Renderable>> {
        Self::render_cell_range_from(
            &self.cells,
            self.render_mode,
            range,
            self.hovered_tool_group,
            self.expanded_tool_group,
        )
    }

    pub(super) fn render_cell_range_from(
        cells: &[Arc<dyn HistoryCell>],
        render_mode: HistoryRenderMode,
        range: Range<usize>,
        hovered_tool_group: Option<usize>,
        expanded_tool_group: Option<usize>,
    ) -> Vec<Box<dyn Renderable>> {
        let mut renderables = Vec::with_capacity(range.len());
        let mut index = range.start;
        while index < range.end {
            if render_mode == HistoryRenderMode::Rich
                && let Some(activity) = cells[index].tool_activity()
            {
                let start = index;
                let mut merged = activity;
                index += 1;
                while index < range.end
                    && let Some(activity) = cells[index].tool_activity()
                {
                    merged.merge(activity);
                    index += 1;
                }
                if merged.call_count >= MIN_GROUPED_TOOL_CALLS {
                    renderables.push(Box::new(ToolActivityGroupRenderable {
                        cells: cells[start..index].to_vec(),
                        activity: merged,
                        hovered: hovered_tool_group == Some(start),
                        expanded: expanded_tool_group == Some(start),
                        top_padding: Self::tool_group_top_padding(cells, start),
                    }) as Box<dyn Renderable>);
                    renderables.extend(
                        std::iter::repeat_with(|| Box::new(()) as Box<dyn Renderable>)
                            .take(index.saturating_sub(start).saturating_sub(1)),
                    );
                    continue;
                }
                for (cell_index, cell) in cells.iter().enumerate().take(index).skip(start) {
                    renderables.push(Self::cell_renderable(
                        cell.clone(),
                        render_mode,
                        /*has_prior_cells*/ cell_index > 0,
                    ));
                }
                continue;
            }

            renderables.push(Self::cell_renderable(
                cells[index].clone(),
                render_mode,
                /*has_prior_cells*/ index > 0,
            ));
            index += 1;
        }
        renderables
    }

    pub(super) fn tool_group_at(&self, index: usize) -> Option<Range<usize>> {
        if self.render_mode != HistoryRenderMode::Rich
            || index >= self.cells.len()
            || self.cells[index].tool_activity().is_none()
        {
            return None;
        }
        let mut start = index;
        while start > 0 && self.cells[start - 1].tool_activity().is_some() {
            start -= 1;
        }
        let mut end = index.saturating_add(1);
        let mut activity = ToolActivity::default();
        while end < self.cells.len() && self.cells[end].tool_activity().is_some() {
            end += 1;
        }
        for cell in &self.cells[start..end] {
            activity.merge(cell.tool_activity()?);
        }
        (activity.call_count >= MIN_GROUPED_TOOL_CALLS).then_some(start..end)
    }

    pub(super) fn tool_group_top_padding(cells: &[Arc<dyn HistoryCell>], start: usize) -> u16 {
        u16::from(start > 0 && !cells[start].is_stream_continuation())
    }

    pub(super) fn set_hovered_tool_group(&mut self, next: Option<usize>, width: u16) -> bool {
        if self.hovered_tool_group == next {
            return false;
        }
        let previous = self.hovered_tool_group;
        self.hovered_tool_group = next;
        self.refresh_tool_groups([previous, next].into_iter().flatten(), width);
        true
    }

    pub(super) fn refresh_tool_groups(
        &mut self,
        starts: impl IntoIterator<Item = usize>,
        width: u16,
    ) {
        let mut starts = starts.into_iter().collect::<Vec<_>>();
        starts.sort_unstable();
        starts.dedup();
        for start in starts.into_iter().rev() {
            let Some(range) = self.tool_group_at(start) else {
                continue;
            };
            let renderables = self.render_cell_range(range.clone());
            self.content
                .replace_range(range.start, range.len(), renderables, width);
        }
    }

    pub(super) fn shift_tool_group_state(&mut self, index: usize, inserted_count: usize) {
        for group_start in [&mut self.hovered_tool_group, &mut self.expanded_tool_group] {
            if let Some(start) = group_start.as_mut()
                && *start >= index
            {
                *start = start.saturating_add(inserted_count);
            }
        }
    }

    pub(super) fn validate_tool_group_state(&mut self) {
        self.hovered_tool_group = self
            .hovered_tool_group
            .and_then(|index| self.tool_group_at(index).map(|range| range.start));
        self.expanded_tool_group = self
            .expanded_tool_group
            .and_then(|index| self.tool_group_at(index).map(|range| range.start));
    }
}

pub(super) struct ToolActivityGroupRenderable {
    cells: Vec<Arc<dyn HistoryCell>>,
    activity: ToolActivity,
    hovered: bool,
    expanded: bool,
    top_padding: u16,
}

impl ToolActivityGroupRenderable {
    fn summary_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut parts = Vec::new();
        for (count, initial_verb, verb, singular, plural) in [
            (
                self.activity.edited_files,
                "Edited",
                "edited",
                "file",
                "files",
            ),
            (
                self.activity.searches,
                "Searched for",
                "searched for",
                "pattern",
                "patterns",
            ),
            (self.activity.read_files, "Read", "read", "file", "files"),
            (
                self.activity.listed_directories,
                "Listed",
                "listed",
                "directory",
                "directories",
            ),
            (
                self.activity.mcp_calls,
                "Called",
                "called",
                "MCP tool",
                "MCP tools",
            ),
            (
                self.activity.shell_commands,
                "Ran",
                "ran",
                "shell command",
                "shell commands",
            ),
        ] {
            if count > 0 {
                let verb = if parts.is_empty() { initial_verb } else { verb };
                let noun = if count == 1 { singular } else { plural };
                parts.push(format!("{verb} {count} {noun}"));
            }
        }
        let marker = if self.activity.has_failure {
            "✗ ".red()
        } else {
            "• ".dim()
        };
        let line: Line<'static> = vec![marker, parts.join(", ").dim()].into();
        crate::wrapping::word_wrap_line(&line, usize::from(width.max(1)))
            .into_iter()
            .map(|line| crate::render::line_utils::line_to_static(&line))
            .collect()
    }

    fn content_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if !self.expanded {
            return plain_hyperlink_lines(self.summary_lines(width));
        }
        let mut lines = Vec::new();
        for (index, cell) in self.cells.iter().enumerate() {
            if index > 0 && !cell.is_stream_continuation() {
                lines.push(HyperlinkLine::from(""));
            }
            lines.extend(cell.transcript_hyperlink_lines(width));
        }
        lines
    }
}

impl Renderable for ToolActivityGroupRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_scrolled(area, buf, /*scroll_offset*/ 0);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let style = if self.expanded {
            crate::style::user_message_style()
        } else {
            Default::default()
        };
        self.top_padding.saturating_add(
            HyperlinkParagraph::new(&self.content_lines(width), style)
                .line_count(width)
                .try_into()
                .unwrap_or(/*default*/ 0),
        )
    }

    fn render_scrolled(&self, area: Rect, buf: &mut Buffer, scroll_offset: u16) -> bool {
        Clear.render(area, buf);
        if scroll_offset >= self.desired_height(area.width) {
            return true;
        }
        let visible_padding = self.top_padding.saturating_sub(scroll_offset);
        let content_scroll = scroll_offset.saturating_sub(self.top_padding);
        let content_area = Rect::new(
            area.x,
            area.y.saturating_add(visible_padding),
            area.width,
            area.height.saturating_sub(visible_padding),
        );
        let style = if self.expanded || self.hovered {
            crate::style::user_message_style()
        } else {
            Default::default()
        };
        HyperlinkParagraph::new(&self.content_lines(area.width), style)
            .scroll(content_scroll)
            .render(content_area, buf);
        true
    }
}
