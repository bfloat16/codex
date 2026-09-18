//! Collapsible adjacent-tool projection for the owned full-screen transcript.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

use super::ConversationViewport;
use crate::chatwidget::ActiveToolDisplay;
use crate::chatwidget::ActiveToolGroupState;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::ToolActivity;
use crate::line_truncation::truncate_line_to_width;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::render::renderable::Renderable;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::HyperlinkParagraph;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::remap_wrapped_line;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use crate::wrapping::word_wrap_line;

const MIN_GROUPED_TOOL_CALLS: usize = 1;
const SUMMARY_TEXT_ALPHA: f32 = 0.68;
const HOVERED_SUMMARY_TEXT_ALPHA: f32 = 0.86;
const ACTIVE_PREVIEW_MAX_ROWS: usize = 2;

#[derive(Clone, Copy, Default)]
struct ToolGroupTail<'a> {
    tool: Option<&'a ActiveToolDisplay>,
    state: ActiveToolGroupState,
}

#[derive(Clone, Copy, Default)]
struct CellRangeRenderState<'a> {
    hovered_tool_group: Option<usize>,
    expanded_tool_group: Option<usize>,
    hovered_file_change: Option<usize>,
    expanded_file_change: Option<usize>,
    tail: ToolGroupTail<'a>,
}

impl ConversationViewport {
    pub(super) fn render_cell_range(&self, range: Range<usize>) -> Vec<Box<dyn Renderable>> {
        let tail = (range.end == self.cells.len()).then_some(ToolGroupTail {
            tool: self
                .live_display
                .as_ref()
                .and_then(|display| display.tool.as_ref()),
            state: self.live_tool_group_state,
        });
        Self::render_cell_range_with_tail(
            &self.cells,
            self.render_mode,
            range,
            CellRangeRenderState {
                hovered_tool_group: self.hovered_tool_group,
                expanded_tool_group: self.expanded_tool_group,
                hovered_file_change: self.hovered_file_change,
                expanded_file_change: self.expanded_file_change,
                tail: tail.unwrap_or_default(),
            },
        )
    }

    pub(super) fn render_cell_range_from(
        cells: &[Arc<dyn HistoryCell>],
        render_mode: HistoryRenderMode,
        range: Range<usize>,
        hovered_tool_group: Option<usize>,
        expanded_tool_group: Option<usize>,
        hovered_file_change: Option<usize>,
        expanded_file_change: Option<usize>,
    ) -> Vec<Box<dyn Renderable>> {
        Self::render_cell_range_with_tail(
            cells,
            render_mode,
            range,
            CellRangeRenderState {
                hovered_tool_group,
                expanded_tool_group,
                hovered_file_change,
                expanded_file_change,
                tail: ToolGroupTail::default(),
            },
        )
    }

    fn render_cell_range_with_tail(
        cells: &[Arc<dyn HistoryCell>],
        render_mode: HistoryRenderMode,
        range: Range<usize>,
        state: CellRangeRenderState<'_>,
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
                let live_tool = (index == cells.len()).then_some(state.tail.tool).flatten();
                if let Some(live_tool) = live_tool {
                    merged.merge(live_tool.activity);
                }
                if merged.call_count >= MIN_GROUPED_TOOL_CALLS {
                    renderables.push(Box::new(ToolActivityGroupRenderable {
                        cells: cells[start..index].to_vec(),
                        live_tool: live_tool.cloned(),
                        activity: merged,
                        active: index == cells.len() && state.tail.state.accepting_content,
                        active_started_at: state.tail.state.started_at,
                        animations_enabled: state.tail.state.animations_enabled,
                        hovered: state.hovered_tool_group == Some(start),
                        expanded: state.expanded_tool_group == Some(start),
                        top_padding: Self::tool_group_top_padding(cells, start),
                        layout: RefCell::new(None),
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
                        /*hovered_file_change*/
                        state.hovered_file_change == Some(cell_index),
                        /*expanded_file_change*/
                        state.expanded_file_change == Some(cell_index),
                    ));
                }
                continue;
            }

            renderables.push(Self::cell_renderable(
                cells[index].clone(),
                render_mode,
                /*has_prior_cells*/ index > 0,
                /*hovered_file_change*/ state.hovered_file_change == Some(index),
                /*expanded_file_change*/ state.expanded_file_change == Some(index),
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

    pub(super) fn trailing_cells_form_tool_group(&self) -> bool {
        self.render_mode == HistoryRenderMode::Rich
            && self
                .cells
                .last()
                .is_some_and(|cell| cell.tool_activity().is_some())
    }

    pub(super) fn synthetic_live_tool_group_index(&self) -> Option<usize> {
        (self.render_mode == HistoryRenderMode::Rich
            && self
                .live_display
                .as_ref()
                .and_then(|display| display.tool.as_ref())
                .is_some()
            && !self.trailing_cells_form_tool_group())
        .then_some(self.cells.len())
    }

    pub(super) fn synthetic_live_tool_group_top_padding(&self) -> u16 {
        u16::from(
            !self.cells.is_empty()
                && self
                    .live_display
                    .as_ref()
                    .and_then(|display| display.tool.as_ref())
                    .is_some_and(|tool| !tool.is_stream_continuation),
        )
    }

    pub(super) fn synthetic_live_tool_group_renderable(&self) -> Option<Box<dyn Renderable>> {
        let display = self.live_display.as_ref()?;
        let tool = display.tool.as_ref()?.clone();
        let start = self.cells.len();
        Some(Box::new(ToolActivityGroupRenderable {
            cells: Vec::new(),
            activity: tool.activity,
            live_tool: Some(tool),
            active: self.live_tool_group_state.accepting_content,
            active_started_at: self.live_tool_group_state.started_at,
            animations_enabled: self.live_tool_group_state.animations_enabled,
            hovered: self.hovered_tool_group == Some(start),
            expanded: self.expanded_tool_group == Some(start),
            top_padding: self.synthetic_live_tool_group_top_padding(),
            layout: RefCell::new(None),
        }))
    }

    /// Refresh the newest committed tool group even when a later non-tool cell ended it.
    pub(super) fn refresh_latest_tool_group(&mut self, width: u16) {
        let Some(last) = self
            .cells
            .iter()
            .rposition(|cell| cell.tool_activity().is_some())
        else {
            return;
        };
        let Some(range) = self.tool_group_at(last) else {
            return;
        };
        let renderables = self.render_cell_range(range.clone());
        self.content
            .replace_range(range.start, range.len(), renderables, width);
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
            if self.synthetic_live_tool_group_index() == Some(start) {
                if let Some(renderable) = self.synthetic_live_tool_group_renderable() {
                    self.content
                        .replace_range(start, 1, vec![renderable], width);
                }
                continue;
            }
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
        let synthetic = self.synthetic_live_tool_group_index();
        self.hovered_tool_group = self.hovered_tool_group.and_then(|index| {
            (synthetic == Some(index))
                .then_some(index)
                .or_else(|| self.tool_group_at(index).map(|range| range.start))
        });
        self.expanded_tool_group = self.expanded_tool_group.and_then(|index| {
            (synthetic == Some(index))
                .then_some(index)
                .or_else(|| self.tool_group_at(index).map(|range| range.start))
        });
    }
}

pub(super) struct ToolActivityGroupRenderable {
    cells: Vec<Arc<dyn HistoryCell>>,
    live_tool: Option<ActiveToolDisplay>,
    activity: ToolActivity,
    active: bool,
    active_started_at: Option<std::time::Instant>,
    animations_enabled: bool,
    hovered: bool,
    expanded: bool,
    top_padding: u16,
    layout: RefCell<Option<ToolGroupLayout>>,
}

struct ToolGroupLayout {
    width: u16,
    lines: Vec<HyperlinkLine>,
}

impl ToolActivityGroupRenderable {
    fn summary_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut spans = Vec::new();
        for (count, initial_verb, verb, singular, plural) in [
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
                self.activity.background_terminal_interactions,
                "Interacted with",
                "interacted with",
                "background terminal",
                "background terminals",
            ),
            (
                self.activity.background_terminal_waits,
                "Waited for",
                "waited for",
                "background terminal",
                "background terminals",
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
                if !spans.is_empty() {
                    spans.push(", ".into());
                }
                let verb = if spans.is_empty() { initial_verb } else { verb };
                let noun = if count == 1 { singular } else { plural };
                spans.extend([
                    format!("{verb} ").into(),
                    count.to_string().bold(),
                    format!(" {noun}").into(),
                ]);
            }
        }
        let style = summary_text_style(self.hovered);
        let marker = if self.active {
            activity_indicator(
                self.active_started_at,
                MotionMode::from_animations_enabled(self.animations_enabled),
                ReducedMotionIndicator::StaticBullet,
            )
            .map(|indicator| ratatui::text::Span::styled("●", indicator.style))
            .unwrap_or_else(|| " ".into())
        } else {
            " ".into()
        };
        let mut line_spans = vec![marker, " ".into()];
        line_spans.extend(spans.into_iter().map(|span| span.patch_style(style)));
        let line = Line::from(line_spans);
        adaptive_wrap_lines(
            [line],
            RtOptions::new(usize::from(width.max(1))).subsequent_indent("  ".into()),
        )
    }

    fn preview_lines(&self, width: u16) -> Vec<Line<'static>> {
        if !self.active {
            return Vec::new();
        }
        let mut lines = self
            .cells
            .iter()
            .flat_map(|cell| cell.tool_group_preview_lines())
            .collect::<Vec<_>>();
        if let Some(live_tool) = &self.live_tool {
            lines.extend(live_tool.preview_lines.clone());
        }
        let Some(latest) = lines.pop() else {
            return Vec::new();
        };
        let width = usize::from(width.max(1));
        let mut wrapped = adaptive_wrap_lines(
            [latest],
            RtOptions::new(width)
                .initial_indent("  └ ".dim().into())
                .subsequent_indent("    ".into()),
        );
        if wrapped.len() > ACTIVE_PREVIEW_MAX_ROWS {
            wrapped.truncate(ACTIVE_PREVIEW_MAX_ROWS);
            if let Some(last) = wrapped.last_mut() {
                let truncated = truncate_line_to_width(last.clone(), width.saturating_sub(1));
                let ellipsis_style = truncated
                    .spans
                    .last()
                    .map(|span| span.style)
                    .unwrap_or_default();
                *last = truncated;
                last.push_span(ratatui::text::Span::styled("…", ellipsis_style));
            }
        }
        wrapped
    }

    fn unwrapped_content_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if !self.expanded {
            let mut lines = self.summary_lines(width);
            lines.extend(self.preview_lines(width));
            return plain_hyperlink_lines(lines);
        }
        let mut lines = Vec::new();
        for (index, cell) in self.cells.iter().enumerate() {
            if index > 0 && !cell.is_stream_continuation() {
                lines.push(HyperlinkLine::from(""));
            }
            lines.extend(cell.tool_group_detail_lines(width));
        }
        if let Some(live_tool) = &self.live_tool {
            if !lines.is_empty() && !live_tool.is_stream_continuation {
                lines.push(HyperlinkLine::from(""));
            }
            lines.extend(live_tool.detail_lines.clone());
        }
        lines
    }

    fn update_layout(&self, width: u16) {
        if self
            .layout
            .borrow()
            .as_ref()
            .is_some_and(|layout| layout.width == width)
        {
            return;
        }
        let lines = self
            .unwrapped_content_lines(width)
            .into_iter()
            .flat_map(|line| {
                let wrapped = word_wrap_line(&line.line, usize::from(width.max(1)))
                    .into_iter()
                    .map(|line| crate::render::line_utils::line_to_static(&line))
                    .collect();
                remap_wrapped_line(&line, wrapped)
            })
            .collect();
        *self.layout.borrow_mut() = Some(ToolGroupLayout { width, lines });
    }
}

impl Renderable for ToolActivityGroupRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_scrolled(area, buf, /*scroll_offset*/ 0);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.update_layout(width);
        let line_count = self
            .layout
            .borrow()
            .as_ref()
            .map_or(/*default*/ 0, |layout| layout.lines.len());
        let top_padding = if self.expanded { 1 } else { self.top_padding };
        top_padding
            .saturating_add(u16::try_from(line_count).unwrap_or(/*default*/ u16::MAX))
            .saturating_add(u16::from(self.expanded))
    }

    fn render_scrolled(&self, area: Rect, buf: &mut Buffer, scroll_offset: u16) -> bool {
        Clear.render(area, buf);
        if self.expanded {
            buf.set_style(area, crate::style::user_message_style());
        }
        if scroll_offset >= self.desired_height(area.width) {
            return true;
        }
        let top_padding = if self.expanded { 1 } else { self.top_padding };
        let visible_padding = top_padding.saturating_sub(scroll_offset);
        let content_scroll = scroll_offset.saturating_sub(top_padding);
        let content_area = Rect::new(
            area.x,
            area.y.saturating_add(visible_padding),
            area.width,
            area.height.saturating_sub(visible_padding),
        );
        self.update_layout(area.width);
        let style = if self.expanded {
            crate::style::user_message_style()
        } else {
            Default::default()
        };
        let layout = self.layout.borrow();
        let lines = layout
            .as_ref()
            .map(|layout| layout.lines.as_slice())
            .unwrap_or_default();
        let start = usize::from(content_scroll).min(lines.len());
        let end = start
            .saturating_add(usize::from(content_area.height))
            .min(lines.len());
        HyperlinkParagraph::new(&lines[start..end], style).render(content_area, buf);
        true
    }
}

pub(super) fn summary_text_style(hovered: bool) -> Style {
    let alpha = if hovered {
        HOVERED_SUMMARY_TEXT_ALPHA
    } else {
        SUMMARY_TEXT_ALPHA
    };
    let color = crate::terminal_palette::default_fg()
        .zip(crate::terminal_palette::default_bg())
        .map(|(foreground, background)| {
            crate::terminal_palette::best_color(crate::color::blend(foreground, background, alpha))
        });
    match color {
        Some(color) if color != Color::Reset => Style::default().fg(color),
        _ if hovered => Style::default(),
        _ => Style::default().dim(),
    }
}
