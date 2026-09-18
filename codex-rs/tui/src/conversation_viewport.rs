//! Retained conversation rendering for an application-owned terminal viewport.
//!
//! This module intentionally owns only transcript projection and bottom-follow state. The app
//! decides when the terminal is owned and how much space remains after the bottom pane is laid
//! out. Committed cells use their main-viewport representation; the more detailed `Ctrl+T`
//! representation remains owned by `pager_overlay`.

use std::cell::Cell;
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::chatwidget::ActiveCellDisplay;
use crate::chatwidget::ActiveCellRenderKey;
use crate::chatwidget::ActiveToolGroupState;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::keymap::PagerKeymap;
use crate::pager_overlay::PagerContent;
use crate::render::Insets;
use crate::render::renderable::InsetRenderable;
use crate::render::renderable::Renderable;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::HyperlinkParagraph;
use crate::tui::MouseScrollDirection;

mod file_changes;
mod tool_groups;

pub(crate) struct ConversationViewport {
    content: PagerContent,
    cells: Vec<Arc<dyn HistoryCell>>,
    render_mode: HistoryRenderMode,
    live_tail_key: Option<LiveTailKey>,
    live_display: Option<ActiveCellDisplay>,
    live_tool_group_state: ActiveToolGroupState,
    live_renderable_count: usize,
    hovered_tool_group: Option<usize>,
    expanded_tool_group: Option<usize>,
    hovered_file_change: Option<usize>,
    expanded_file_change: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveTailKey {
    width: u16,
    revision: u64,
    is_stream_continuation: bool,
    animation_tick: Option<u64>,
    tool_group_accepting_content: bool,
    tool_group_animation_tick: Option<u64>,
}

impl ConversationViewport {
    pub(crate) fn new(
        cells: Vec<Arc<dyn HistoryCell>>,
        render_mode: HistoryRenderMode,
        keymap: PagerKeymap,
    ) -> Self {
        let renderables = Self::render_cells(
            &cells,
            render_mode,
            /*hovered_tool_group*/ None,
            /*expanded_tool_group*/ None,
            /*hovered_file_change*/ None,
            /*expanded_file_change*/ None,
        );
        Self {
            content: PagerContent::new(renderables, keymap),
            cells,
            render_mode,
            live_tail_key: None,
            live_display: None,
            live_tool_group_state: ActiveToolGroupState::default(),
            live_renderable_count: 0,
            hovered_tool_group: None,
            expanded_tool_group: None,
            hovered_file_change: None,
            expanded_file_change: None,
        }
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        self.content.render(area, buf);
    }

    pub(crate) fn handle_navigation_key(&mut self, area: Rect, key_event: KeyEvent) -> bool {
        self.content.handle_navigation_key(area, key_event)
    }

    pub(crate) fn scroll_rows(&mut self, direction: MouseScrollDirection, rows: usize) {
        self.content.scroll_rows(direction, rows);
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        self.content.scroll_to_bottom();
    }

    pub(crate) fn handle_mouse_move(&mut self, area: Rect, position: Position) -> bool {
        if self.render_mode != HistoryRenderMode::Rich {
            let group_changed = self.set_hovered_tool_group(/*next*/ None, area.width);
            let file_change_changed = self.set_hovered_file_change(/*next*/ None, area.width);
            return group_changed || file_change_changed;
        }
        let tool_group_hit = self
            .tool_group_hit(area, position)
            .filter(|start| Some(*start) != self.expanded_tool_group);
        let file_change_hit = if tool_group_hit.is_none() {
            self.file_change_hit(area, position)
        } else {
            None
        };
        let group_changed = self.set_hovered_tool_group(tool_group_hit, area.width);
        let file_change_changed = self.set_hovered_file_change(file_change_hit, area.width);
        group_changed || file_change_changed
    }

    pub(crate) fn handle_left_click(&mut self, area: Rect, position: Position) -> bool {
        if let Some(group_start) = self.tool_group_hit(area, position) {
            let previous = self.expanded_tool_group;
            let previous_hover = self.hovered_tool_group.take();
            self.expanded_tool_group = (previous != Some(group_start)).then_some(group_start);
            self.refresh_tool_groups(
                [previous, previous_hover, Some(group_start)]
                    .into_iter()
                    .flatten(),
                area.width,
            );
            return true;
        }
        self.toggle_file_change_at(area, position)
    }

    fn tool_group_hit(&mut self, area: Rect, position: Position) -> Option<usize> {
        let (index, row) = self.content.renderable_at_position(area, position)?;
        if self.synthetic_live_tool_group_index() == Some(index) {
            if self.expanded_tool_group == Some(index) {
                return Some(index);
            }
            return (row >= self.synthetic_live_tool_group_top_padding()).then_some(index);
        }
        let range = self.tool_group_at(index)?;
        if self.expanded_tool_group == Some(range.start) {
            return Some(range.start);
        }
        let top_padding = Self::tool_group_top_padding(&self.cells, range.start);
        (row >= top_padding).then_some(range.start)
    }

    pub(crate) fn push_cell(&mut self, cell: Arc<dyn HistoryCell>) {
        let follow_bottom = self.content.is_following_bottom();
        let closes_trailing_tool_group = self.render_mode == HistoryRenderMode::Rich
            && cell.tool_activity().is_none()
            && self
                .cells
                .last()
                .is_some_and(|cell| cell.tool_activity().is_some());
        let inserted_before_synthetic = self.synthetic_live_tool_group_index()
            == Some(self.cells.len())
            && cell.tool_activity().is_none();
        self.remove_live_renderables();
        if inserted_before_synthetic {
            self.shift_tool_group_state(self.cells.len(), /*inserted_count*/ 1);
        }
        self.cells.push(cell);
        let mut rebuild_start = self.cells.len().saturating_sub(1);
        if self.render_mode == HistoryRenderMode::Rich
            && self.cells[rebuild_start].tool_activity().is_some()
        {
            while rebuild_start > 0 && self.cells[rebuild_start - 1].tool_activity().is_some() {
                rebuild_start -= 1;
            }
        } else if closes_trailing_tool_group {
            rebuild_start = rebuild_start.saturating_sub(1);
            while rebuild_start > 0 && self.cells[rebuild_start - 1].tool_activity().is_some() {
                rebuild_start -= 1;
            }
        }
        let renderables = self.render_cell_range(rebuild_start..self.cells.len());
        self.content.replace_tail(rebuild_start, renderables);
        self.append_live_renderables();
        if follow_bottom {
            self.content.scroll_to_bottom();
        }
    }

    pub(crate) fn replace_cells(&mut self, cells: Vec<Arc<dyn HistoryCell>>) {
        let follow_bottom = self.content.is_following_bottom();
        self.remove_live_renderables();
        self.live_tail_key = None;
        self.live_display = None;
        self.live_tool_group_state = ActiveToolGroupState::default();
        self.hovered_tool_group = None;
        self.expanded_tool_group = None;
        self.hovered_file_change = None;
        self.expanded_file_change = None;
        self.cells = cells;
        self.content.replace(Self::render_cells(
            &self.cells,
            self.render_mode,
            self.hovered_tool_group,
            self.expanded_tool_group,
            self.hovered_file_change,
            self.expanded_file_change,
        ));
        if follow_bottom {
            self.content.scroll_to_bottom();
        }
    }

    pub(crate) fn insert_cells(
        &mut self,
        index: usize,
        cells: Vec<Arc<dyn HistoryCell>>,
        width: u16,
    ) {
        if cells.is_empty() {
            return;
        }
        let follow_bottom = self.content.is_following_bottom();
        let index = index.min(self.cells.len());
        self.remove_live_renderables();
        let inserted_count = cells.len();
        let mut rebuild_start = index;
        while rebuild_start > 0 && self.cells[rebuild_start - 1].tool_activity().is_some() {
            rebuild_start -= 1;
        }
        let mut old_rebuild_end = index;
        while old_rebuild_end < self.cells.len()
            && self.cells[old_rebuild_end].tool_activity().is_some()
        {
            old_rebuild_end += 1;
        }
        if index == 0 && old_rebuild_end == 0 && !self.cells.is_empty() {
            old_rebuild_end = 1;
        }
        self.shift_tool_group_state(index, inserted_count);
        self.shift_file_change_state(index, inserted_count);
        self.cells.splice(index..index, cells);
        self.validate_tool_group_state();
        self.validate_file_change_state();
        let new_rebuild_end = old_rebuild_end.saturating_add(inserted_count);
        let renderables = self.render_cell_range(rebuild_start..new_rebuild_end);
        self.content.splice_above_viewport(
            rebuild_start,
            old_rebuild_end.saturating_sub(rebuild_start),
            renderables,
            width,
        );
        self.append_live_renderables();
        if follow_bottom {
            self.content.scroll_to_bottom();
        }
    }

    pub(crate) fn set_render_mode(&mut self, render_mode: HistoryRenderMode) {
        if self.render_mode == render_mode {
            return;
        }
        let follow_bottom = self.content.is_following_bottom();
        self.remove_live_renderables();
        self.live_tail_key = None;
        self.live_display = None;
        self.live_tool_group_state = ActiveToolGroupState::default();
        self.hovered_tool_group = None;
        self.expanded_tool_group = None;
        self.hovered_file_change = None;
        self.expanded_file_change = None;
        self.render_mode = render_mode;
        self.content.replace(Self::render_cells(
            &self.cells,
            self.render_mode,
            self.hovered_tool_group,
            self.expanded_tool_group,
            self.hovered_file_change,
            self.expanded_file_change,
        ));
        if follow_bottom {
            self.content.scroll_to_bottom();
        }
    }

    pub(crate) fn sync_live_tail(
        &mut self,
        width: u16,
        active_key: Option<ActiveCellRenderKey>,
        tool_group_state: ActiveToolGroupState,
        compute_display: impl FnOnce(u16) -> Option<ActiveCellDisplay>,
    ) {
        let next_key = (active_key.is_some() || tool_group_state.accepting_content).then(|| {
            let active_key = active_key.unwrap_or(ActiveCellRenderKey {
                revision: 0,
                is_stream_continuation: false,
                animation_tick: None,
            });
            LiveTailKey {
                width,
                revision: active_key.revision,
                is_stream_continuation: active_key.is_stream_continuation,
                animation_tick: active_key.animation_tick,
                tool_group_accepting_content: tool_group_state.accepting_content,
                tool_group_animation_tick: tool_group_state.animation_tick,
            }
        });
        if self.live_tail_key == next_key {
            return;
        }

        let follow_bottom = self.content.is_following_bottom();
        self.remove_live_renderables();
        self.live_tail_key = next_key;
        self.live_display = next_key.and_then(|_| compute_display(width));
        self.live_tool_group_state = tool_group_state;
        self.refresh_latest_tool_group(width);
        self.validate_tool_group_state();
        self.append_live_renderables();
        if follow_bottom {
            self.content.scroll_to_bottom();
        }
    }

    pub(crate) fn is_following_bottom(&self) -> bool {
        self.content.is_following_bottom()
    }

    #[cfg(test)]
    pub(crate) fn committed_cell_count(&self) -> usize {
        self.cells.len()
    }

    fn render_cells(
        cells: &[Arc<dyn HistoryCell>],
        render_mode: HistoryRenderMode,
        hovered_tool_group: Option<usize>,
        expanded_tool_group: Option<usize>,
        hovered_file_change: Option<usize>,
        expanded_file_change: Option<usize>,
    ) -> Vec<Box<dyn Renderable>> {
        Self::render_cell_range_from(
            cells,
            render_mode,
            0..cells.len(),
            hovered_tool_group,
            expanded_tool_group,
            hovered_file_change,
            expanded_file_change,
        )
    }

    fn cell_renderable(
        cell: Arc<dyn HistoryCell>,
        render_mode: HistoryRenderMode,
        has_prior_cells: bool,
        hovered_file_change: bool,
        expanded_file_change: bool,
    ) -> Box<dyn Renderable> {
        let is_stream_continuation = cell.is_stream_continuation();
        let renderable: Box<dyn Renderable> = Box::new(ConversationCellRenderable {
            cell,
            render_mode,
            cached_height: Cell::new(None),
            hovered_file_change,
            expanded_file_change,
        });
        if has_prior_cells && !is_stream_continuation {
            Self::with_leading_spacing(renderable)
        } else {
            renderable
        }
    }

    fn live_tail_renderable(
        lines: Vec<HyperlinkLine>,
        has_prior_cells: bool,
        is_stream_continuation: bool,
    ) -> Box<dyn Renderable> {
        let renderable: Box<dyn Renderable> = Box::new(HyperlinkLinesRenderable { lines });
        if has_prior_cells && !is_stream_continuation {
            Self::with_leading_spacing(renderable)
        } else {
            renderable
        }
    }

    fn with_leading_spacing(renderable: Box<dyn Renderable>) -> Box<dyn Renderable> {
        Box::new(InsetRenderable::new(
            renderable,
            Insets::tlbr(
                /*top*/ 1, /*left*/ 0, /*bottom*/ 0, /*right*/ 0,
            ),
        ))
    }

    fn remove_live_renderables(&mut self) {
        for _ in 0..self.live_renderable_count {
            self.content.pop();
        }
        self.live_renderable_count = 0;
    }

    fn append_live_renderables(&mut self) {
        let Some(display) = self.live_display.as_ref() else {
            return;
        };
        if self.render_mode == HistoryRenderMode::Raw {
            if !display.lines.is_empty() {
                let is_stream_continuation = self
                    .live_tail_key
                    .is_some_and(|key| key.is_stream_continuation);
                self.content.push(Self::live_tail_renderable(
                    display.lines.clone(),
                    !self.cells.is_empty(),
                    is_stream_continuation,
                ));
                self.live_renderable_count = 1;
            }
            return;
        }

        if display.tool.is_some()
            && !self.trailing_cells_form_tool_group()
            && let Some(renderable) = self.synthetic_live_tool_group_renderable()
        {
            self.content.push(renderable);
            self.live_renderable_count = self.live_renderable_count.saturating_add(1);
        }

        let lines = if display.tool.is_some() {
            &display.auxiliary_lines
        } else {
            &display.lines
        };
        if !lines.is_empty() {
            self.content.push(Self::live_tail_renderable(
                lines.clone(),
                !self.cells.is_empty() || self.live_renderable_count > 0,
                display.tool.is_none()
                    && self
                        .live_tail_key
                        .is_some_and(|key| key.is_stream_continuation),
            ));
            self.live_renderable_count = self.live_renderable_count.saturating_add(1);
        }
    }
}

struct ConversationCellRenderable {
    cell: Arc<dyn HistoryCell>,
    render_mode: HistoryRenderMode,
    cached_height: Cell<Option<(u16, u16)>>,
    hovered_file_change: bool,
    expanded_file_change: bool,
}

impl Renderable for ConversationCellRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.render_file_change(area, buf, /*scroll_offset*/ 0) {
            return;
        }
        let hyperlink_lines = self
            .cell
            .display_hyperlink_lines_for_mode(area.width, self.render_mode);
        let block_style = match self.render_mode {
            HistoryRenderMode::Rich => self.cell.rich_block_style().unwrap_or_default(),
            HistoryRenderMode::Raw => Default::default(),
        };
        HyperlinkParagraph::new(&hyperlink_lines, block_style).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        if let Some(height) = self.file_change_desired_height(width) {
            return height;
        }
        if let Some((cached_width, height)) = self.cached_height.get()
            && cached_width == width
        {
            return height;
        }
        let height = self.cell.desired_height_for_mode(width, self.render_mode);
        self.cached_height.set(Some((width, height)));
        height
    }

    fn render_scrolled(&self, area: Rect, buf: &mut Buffer, scroll_offset: u16) -> bool {
        if self.render_file_change(area, buf, scroll_offset) {
            return true;
        }
        let hyperlink_lines = self
            .cell
            .display_hyperlink_lines_for_mode(area.width, self.render_mode);
        let block_style = match self.render_mode {
            HistoryRenderMode::Rich => self.cell.rich_block_style().unwrap_or_default(),
            HistoryRenderMode::Raw => Default::default(),
        };
        HyperlinkParagraph::new(&hyperlink_lines, block_style)
            .scroll(scroll_offset)
            .render(area, buf);
        true
    }
}

struct HyperlinkLinesRenderable {
    lines: Vec<HyperlinkLine>,
}

impl Renderable for HyperlinkLinesRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        HyperlinkParagraph::new(&self.lines, Default::default()).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        HyperlinkParagraph::new(&self.lines, Default::default())
            .line_count(width)
            .try_into()
            .unwrap_or(/*default*/ 0)
    }
}

#[cfg(test)]
#[path = "conversation_viewport_tests.rs"]
mod tests;
