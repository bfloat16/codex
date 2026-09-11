//! Application-owned alternate-screen rendering.
//!
//! The owned mode keeps committed conversation cells in a retained viewport and reserves the
//! bottom of every frame for the composer. Inline mode continues to use terminal scrollback.

use std::time::Duration;
use std::time::Instant;

use crossterm::cursor::SetCursorStyle;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Alignment;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use super::*;
use crate::AltScreenBehavior;
use crate::tui::MouseInteractionEvent;
use crate::tui::MouseScrollDirection;
use crate::tui::MouseScrollEvent;

const BASE_SCROLL_ROWS: usize = 3;
const SCROLL_ACCELERATION_HALF_LIFE_MS: usize = 150;
const SCROLL_ACCELERATION_RESET_AFTER: Duration = Duration::from_millis(250);
const SCROLL_ACCELERATION_BOOST_PER_MILLE: usize = 400;
const MAX_SCROLL_MULTIPLIER_PER_MILLE: usize = 5_000;
const PER_MILLE: usize = 1_000;

pub(super) struct OwnedScreen {
    pub(super) viewport: ConversationViewport,
    replay_in_progress: bool,
    last_conversation_area: Rect,
    scroll_frame_pending: bool,
    scroll_acceleration: ScrollAcceleration,
}

/// Decaying scroll multiplier modeled after native desktop wheel acceleration.
#[derive(Debug)]
struct ScrollAcceleration {
    direction: Option<MouseScrollDirection>,
    last_at: Option<Instant>,
    multiplier_per_mille: usize,
}

impl Default for ScrollAcceleration {
    fn default() -> Self {
        Self {
            direction: None,
            last_at: None,
            multiplier_per_mille: PER_MILLE,
        }
    }
}

impl ScrollAcceleration {
    fn rows(&mut self, direction: MouseScrollDirection) -> usize {
        self.rows_at(direction, Instant::now())
    }

    fn rows_at(&mut self, direction: MouseScrollDirection, now: Instant) -> usize {
        let elapsed = self
            .last_at
            .and_then(|last_at| now.checked_duration_since(last_at));
        if self.direction == Some(direction)
            && elapsed.is_some_and(|elapsed| elapsed <= SCROLL_ACCELERATION_RESET_AFTER)
        {
            let elapsed_ms = elapsed
                .and_then(|elapsed| usize::try_from(elapsed.as_millis()).ok())
                .unwrap_or(usize::MAX);
            let decay_denominator = SCROLL_ACCELERATION_HALF_LIFE_MS
                .saturating_add(elapsed_ms)
                .max(1);
            // This rational decay reaches one half at the configured half-life without putting
            // floating-point work in the input hot path.
            let retained = self
                .multiplier_per_mille
                .saturating_sub(PER_MILLE)
                .saturating_mul(SCROLL_ACCELERATION_HALF_LIFE_MS)
                / decay_denominator;
            let boost = SCROLL_ACCELERATION_BOOST_PER_MILLE
                .saturating_mul(SCROLL_ACCELERATION_HALF_LIFE_MS)
                / decay_denominator;
            self.multiplier_per_mille = PER_MILLE
                .saturating_add(retained)
                .saturating_add(boost)
                .min(MAX_SCROLL_MULTIPLIER_PER_MILLE);
        } else {
            self.multiplier_per_mille = PER_MILLE;
        }
        self.direction = Some(direction);
        self.last_at = Some(now);

        BASE_SCROLL_ROWS
            .saturating_mul(self.multiplier_per_mille)
            .saturating_add(PER_MILLE / 2)
            / PER_MILLE
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

struct RenderedOwnedScreen {
    cursor: Option<(u16, u16)>,
    cursor_style: SetCursorStyle,
}

impl OwnedScreen {
    fn new(chat_widget: &ChatWidget, keymap: crate::keymap::PagerKeymap) -> Self {
        Self {
            viewport: ConversationViewport::new(
                Vec::new(),
                chat_widget.history_render_mode(),
                keymap,
            ),
            replay_in_progress: false,
            last_conversation_area: Rect::default(),
            scroll_frame_pending: false,
            scroll_acceleration: ScrollAcceleration::default(),
        }
    }

    fn render(
        &mut self,
        chat_widget: &ChatWidget,
        area: Rect,
        buffer: &mut Buffer,
    ) -> RenderedOwnedScreen {
        self.scroll_frame_pending = false;
        Clear.render(area, buffer);

        let bottom_pane = chat_widget.bottom_pane_renderable();
        let bottom_height = bottom_pane.desired_height(area.width).min(area.height);
        let conversation_height = area.height.saturating_sub(bottom_height);
        let conversation_area = Rect::new(
            area.x,
            area.y,
            chat_widget.history_wrap_width(area.width),
            conversation_height,
        );
        let bottom_area = Rect::new(
            area.x,
            area.y.saturating_add(conversation_height),
            area.width,
            bottom_height,
        );
        self.last_conversation_area = conversation_area;

        self.viewport
            .set_render_mode(chat_widget.history_render_mode());
        let active_key = chat_widget.active_cell_render_key();
        self.viewport
            .sync_live_tail(conversation_area.width, active_key, |width| {
                chat_widget.active_cell_display_hyperlink_lines(width)
            });
        self.viewport.render(conversation_area, buffer);
        if !self.viewport.is_following_bottom() {
            Self::render_jump_to_bottom_hint(conversation_area, buffer);
        }
        bottom_pane.render(bottom_area, buffer);

        RenderedOwnedScreen {
            cursor: bottom_pane.cursor_pos(bottom_area),
            cursor_style: bottom_pane.cursor_style(bottom_area),
        }
    }

    fn handle_navigation_key(&mut self, key_event: KeyEvent) -> bool {
        if crate::key_hint::ctrl(KeyCode::End).is_press(key_event) {
            self.scroll_acceleration.reset();
            self.viewport.scroll_to_bottom();
            return true;
        }
        // Alternate-scroll wheel events arrive as arrow keys. The app-level guard leaves arrows
        // with the composer whenever it contains a draft.
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            || !matches!(
                key_event.code,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            )
        {
            return false;
        }
        if self.scroll_frame_pending {
            return true;
        }
        let handled = match key_event.code {
            KeyCode::Up => {
                self.scroll(MouseScrollDirection::Up);
                true
            }
            KeyCode::Down => {
                self.scroll(MouseScrollDirection::Down);
                true
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                self.scroll_acceleration.reset();
                self.viewport
                    .handle_navigation_key(self.last_conversation_area, key_event)
            }
            _ => false,
        };
        self.scroll_frame_pending = handled;
        handled
    }

    fn handle_mouse_scroll(&mut self, event: MouseScrollEvent) -> bool {
        if !self
            .last_conversation_area
            .contains(Position::new(event.column, event.row))
        {
            return false;
        }
        if self.scroll_frame_pending {
            return true;
        }
        self.scroll_frame_pending = true;
        self.scroll(event.direction);
        true
    }

    fn handle_mouse_interaction(&mut self, event: MouseInteractionEvent) -> bool {
        self.viewport.handle_mouse_interaction(
            self.last_conversation_area,
            Position::new(event.column, event.row),
            event.kind,
        )
    }

    fn scroll(&mut self, direction: MouseScrollDirection) {
        let rows = self.scroll_acceleration.rows(direction);
        self.viewport.scroll_rows(direction, rows);
    }

    fn render_jump_to_bottom_hint(area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let shortcut = crate::key_hint::ctrl(KeyCode::End).display_label();
        let line: Line<'static> = vec![shortcut.into(), " jump to bottom ".dim(), "↓".dim()].into();
        let content_width = u16::try_from(line.width()).unwrap_or(/*default*/ u16::MAX);
        let hint_width = content_width.saturating_add(/*rhs*/ 2).min(area.width);
        let hint_area = Rect::new(
            area.x
                .saturating_add(area.width.saturating_sub(hint_width) / 2),
            area.bottom().saturating_sub(/*rhs*/ 1),
            hint_width,
            1,
        );
        Clear.render(hint_area, buffer);
        Paragraph::new(line)
            .style(crate::style::user_message_style())
            .alignment(Alignment::Center)
            .render(hint_area, buffer);
    }
}

impl App {
    pub(super) fn owned_screen_for_behavior(
        alt_screen_behavior: AltScreenBehavior,
        chat_widget: &ChatWidget,
        keymap: crate::keymap::PagerKeymap,
    ) -> Option<OwnedScreen> {
        match alt_screen_behavior {
            AltScreenBehavior::Disabled | AltScreenBehavior::OverlayOnly => None,
            AltScreenBehavior::Owned => Some(OwnedScreen::new(chat_widget, keymap)),
        }
    }

    pub(super) fn has_owned_screen(&self) -> bool {
        self.owned_screen.is_some()
    }

    pub(super) fn owned_screen_push_cell(&mut self, cell: Arc<dyn HistoryCell>) {
        if let Some(screen) = &mut self.owned_screen {
            screen.viewport.push_cell(cell);
        }
    }

    pub(super) fn owned_screen_insert_cells(
        &mut self,
        index: usize,
        cells: Vec<Arc<dyn HistoryCell>>,
        width: u16,
    ) {
        if let Some(screen) = &mut self.owned_screen {
            screen.viewport.insert_cells(index, cells, width);
        }
    }

    pub(super) fn begin_owned_screen_replay(&mut self) {
        if let Some(screen) = &mut self.owned_screen {
            screen.replay_in_progress = true;
        }
    }

    pub(super) fn finish_owned_screen_replay(&mut self) {
        if let Some(screen) = &mut self.owned_screen {
            screen.replay_in_progress = false;
        }
    }

    pub(super) fn owned_screen_replay_in_progress(&self) -> bool {
        self.owned_screen
            .as_ref()
            .is_some_and(|screen| screen.replay_in_progress)
    }

    pub(super) fn handle_owned_screen_navigation_key(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> bool {
        if !self.chat_widget.no_modal_or_popup_active()
            || (!self.chat_widget.composer_is_empty()
                && !crate::key_hint::ctrl(KeyCode::End).is_press(key_event))
        {
            return false;
        }
        let handled = self
            .owned_screen
            .as_mut()
            .is_some_and(|screen| screen.handle_navigation_key(key_event));
        if handled {
            tui.frame_requester().schedule_frame();
        }
        handled
    }

    pub(super) fn handle_owned_screen_mouse_scroll(
        &mut self,
        tui: &mut tui::Tui,
        event: MouseScrollEvent,
    ) -> bool {
        if !self.chat_widget.no_modal_or_popup_active() {
            return false;
        }
        let handled = self
            .owned_screen
            .as_mut()
            .is_some_and(|screen| screen.handle_mouse_scroll(event));
        if handled {
            tui.frame_requester().schedule_frame();
        }
        handled
    }

    pub(super) fn handle_owned_screen_mouse_interaction(
        &mut self,
        tui: &mut tui::Tui,
        event: MouseInteractionEvent,
    ) -> bool {
        if !self.chat_widget.no_modal_or_popup_active() {
            return false;
        }
        let handled = self
            .owned_screen
            .as_mut()
            .is_some_and(|screen| screen.handle_mouse_interaction(event));
        if handled {
            tui.frame_requester().schedule_frame();
        }
        handled
    }

    pub(crate) fn sync_owned_screen_cells(&mut self) {
        if let Some(screen) = &mut self.owned_screen {
            screen.viewport.replace_cells(self.transcript_cells.clone());
        }
    }

    pub(super) fn sync_owned_screen_render_mode(&mut self) {
        if let Some(screen) = &mut self.owned_screen {
            screen
                .viewport
                .set_render_mode(self.chat_widget.history_render_mode());
        }
    }

    pub(super) fn handle_owned_draw_pre_render(&mut self, tui: &mut tui::Tui) -> Result<bool> {
        if self.owned_screen.is_none() {
            return Ok(false);
        }
        let size = tui.terminal.size()?;
        if size.width != tui.terminal.last_known_screen_size.width {
            self.chat_widget.on_terminal_resize(size.width);
        }
        if size != tui.terminal.last_known_screen_size {
            self.refresh_status_line();
        }
        self.transcript_reflow.clear();
        tui.clear_pending_history_lines();
        Ok(true)
    }

    pub(super) fn render_owned_screen_frame(&mut self, tui: &mut tui::Tui) -> Result<Option<Rect>> {
        let Some(screen) = &mut self.owned_screen else {
            return Ok(None);
        };
        self.chat_widget
            .update_owned_screen_width(tui.terminal.size()?.width);
        let chat_widget = &self.chat_widget;
        let mut rendered_area = Rect::default();
        tui.draw(/*height*/ u16::MAX, |frame| {
            rendered_area = frame.area();
            let rendered = screen.render(chat_widget, rendered_area, frame.buffer);
            if let Some((x, y)) = rendered.cursor {
                frame.set_cursor_style(rendered.cursor_style);
                frame.set_cursor_position((x, y));
            }
        })?;
        Ok(Some(rendered_area))
    }
}

#[cfg(test)]
#[path = "owned_screen_tests.rs"]
mod tests;
