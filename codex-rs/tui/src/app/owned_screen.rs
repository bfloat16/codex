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
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use super::*;
use crate::AltScreenBehavior;
use crate::tui::MouseInteractionEvent;
use crate::tui::MouseInteractionKind;
use crate::tui::MouseScrollDirection;
use crate::tui::MouseScrollEvent;

mod scroll;
mod selection;

#[cfg(test)]
use scroll::PER_MILLE;
use scroll::ScrollAcceleration;
use selection::ScreenTextSelection;
use selection::SelectionRelease;

const COPY_NOTICE_DURATION: Duration = Duration::from_secs(2);

pub(super) struct OwnedScreen {
    pub(super) viewport: ConversationViewport,
    replay_in_progress: bool,
    last_conversation_area: Rect,
    last_selection_area: Rect,
    scroll_acceleration: ScrollAcceleration,
    selection: ScreenTextSelection,
    last_click: Option<LastClick>,
    copy_notice: Option<CopyNotice>,
}

struct LastClick {
    position: Position,
    at: Instant,
    count: u8,
}

struct CopyNotice {
    char_count: usize,
    expires_at: Instant,
}

enum OwnedScreenMouseAction {
    Ignored,
    Redraw,
    Copy(String),
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
            last_selection_area: Rect::default(),
            scroll_acceleration: ScrollAcceleration::default(),
            selection: ScreenTextSelection::default(),
            last_click: None,
            copy_notice: None,
        }
    }

    fn render(
        &mut self,
        chat_widget: &ChatWidget,
        area: Rect,
        buffer: &mut Buffer,
    ) -> RenderedOwnedScreen {
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
        self.last_selection_area = area;

        self.viewport
            .set_render_mode(chat_widget.history_render_mode());
        let active_key = chat_widget.active_cell_render_key();
        self.viewport
            .sync_live_tail(conversation_area.width, active_key, |width| {
                chat_widget.active_cell_display_hyperlink_lines(width)
            });
        self.viewport.render(conversation_area, buffer);
        Self::extend_conversation_backgrounds(conversation_area, area.right(), buffer);
        if !self.viewport.is_following_bottom() {
            Self::render_jump_to_bottom_hint(conversation_area, buffer);
        }
        bottom_pane.render(bottom_area, buffer);
        let cursor = bottom_pane.cursor_pos(bottom_area);
        let cursor_y = cursor.map(|(_, y)| y);
        self.selection.capture_and_render(area, buffer);
        self.render_copy_notice(bottom_area, cursor_y, buffer);

        RenderedOwnedScreen {
            cursor,
            cursor_style: bottom_pane.cursor_style(bottom_area),
        }
    }

    fn handle_navigation_key(&mut self, key_event: KeyEvent) -> bool {
        if crate::key_hint::ctrl(KeyCode::End).is_press(key_event) {
            self.selection.clear();
            self.scroll_acceleration.reset();
            self.viewport.scroll_to_bottom();
            return true;
        }
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            || !matches!(key_event.code, KeyCode::PageUp | KeyCode::PageDown)
        {
            return false;
        }
        let handled = match key_event.code {
            KeyCode::PageUp | KeyCode::PageDown => {
                self.scroll_acceleration.reset();
                self.viewport
                    .handle_navigation_key(self.last_conversation_area, key_event)
            }
            _ => false,
        };
        if handled {
            self.selection.clear();
        }
        handled
    }

    fn handle_mouse_scroll(&mut self, event: MouseScrollEvent) -> bool {
        if !self
            .last_conversation_area
            .contains(Position::new(event.column, event.row))
        {
            return false;
        }
        self.selection.clear();
        self.last_click = None;
        self.scroll(event.direction);
        true
    }

    fn handle_mouse_interaction(&mut self, event: MouseInteractionEvent) -> OwnedScreenMouseAction {
        let position = Position::new(event.column, event.row);
        match event.kind {
            MouseInteractionKind::Move => {
                if self
                    .viewport
                    .handle_mouse_move(self.last_conversation_area, position)
                {
                    OwnedScreenMouseAction::Redraw
                } else {
                    OwnedScreenMouseAction::Ignored
                }
            }
            MouseInteractionKind::LeftDown => {
                if self.selection.left_down(self.last_selection_area, position) {
                    OwnedScreenMouseAction::Redraw
                } else {
                    OwnedScreenMouseAction::Ignored
                }
            }
            MouseInteractionKind::LeftDrag => {
                if self.selection.left_drag(self.last_selection_area, position) {
                    OwnedScreenMouseAction::Redraw
                } else {
                    OwnedScreenMouseAction::Ignored
                }
            }
            MouseInteractionKind::LeftUp => {
                match self.selection.left_up(self.last_selection_area, position) {
                    SelectionRelease::Ignored => OwnedScreenMouseAction::Ignored,
                    SelectionRelease::Click(position) => {
                        let now = Instant::now();
                        let click_count = self
                            .last_click
                            .as_ref()
                            .filter(|last| {
                                last.position == position
                                    && now.saturating_duration_since(last.at)
                                        <= Duration::from_millis(500)
                            })
                            .map_or(1, |last| last.count.saturating_add(1));
                        self.last_click = Some(LastClick {
                            position,
                            at: now,
                            count: click_count,
                        });
                        if click_count >= 2
                            && let Some(text) = self
                                .selection
                                .select_word(self.last_selection_area, position)
                        {
                            self.last_click = None;
                            return OwnedScreenMouseAction::Copy(text);
                        }
                        self.viewport
                            .handle_left_click(self.last_conversation_area, position);
                        OwnedScreenMouseAction::Redraw
                    }
                    SelectionRelease::Copy(text) => {
                        self.last_click = None;
                        OwnedScreenMouseAction::Copy(text)
                    }
                    SelectionRelease::Redraw => OwnedScreenMouseAction::Redraw,
                }
            }
        }
    }

    fn scroll(&mut self, direction: MouseScrollDirection) {
        let rows = self.scroll_acceleration.rows(direction);
        self.viewport.scroll_rows(direction, rows);
    }

    fn show_copy_notice(&mut self, char_count: usize) {
        self.copy_notice = Some(CopyNotice {
            char_count,
            expires_at: Instant::now()
                .checked_add(COPY_NOTICE_DURATION)
                .unwrap_or_else(Instant::now),
        });
    }

    fn copy_notice_delay(&self) -> Option<Duration> {
        self.copy_notice
            .as_ref()?
            .expires_at
            .checked_duration_since(Instant::now())
    }

    fn render_copy_notice(&mut self, area: Rect, cursor_y: Option<u16>, buffer: &mut Buffer) {
        let Some(notice) = &self.copy_notice else {
            return;
        };
        if Instant::now() >= notice.expires_at {
            self.copy_notice = None;
            return;
        }
        if area.is_empty() {
            return;
        }
        let notice_y = cursor_y
            .map(|y| y.saturating_sub(1).max(area.y))
            .unwrap_or(area.y);
        let notice_area = Rect::new(area.x, notice_y, area.width.saturating_sub(/*rhs*/ 2), 1);
        Paragraph::new(format!("copied {} chars to clipboard", notice.char_count))
            .fg(selection::selection_background())
            .alignment(Alignment::Right)
            .render(notice_area, buffer);
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

    fn extend_conversation_backgrounds(area: Rect, right: u16, buffer: &mut Buffer) {
        if area.is_empty() || right <= area.right() {
            return;
        }
        let source_x = area.right().saturating_sub(/*rhs*/ 1);
        for y in area.y..area.bottom() {
            let background = buffer[(source_x, y)].bg;
            if background == Color::Reset {
                continue;
            }
            for x in area.right()..right {
                buffer[(x, y)].set_bg(background);
            }
        }
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
        let action = self
            .owned_screen
            .as_mut()
            .map_or(OwnedScreenMouseAction::Ignored, |screen| {
                screen.handle_mouse_interaction(event)
            });
        match action {
            OwnedScreenMouseAction::Ignored => false,
            OwnedScreenMouseAction::Redraw => {
                tui.frame_requester().schedule_frame();
                true
            }
            OwnedScreenMouseAction::Copy(text) => {
                let char_count = text.chars().count();
                match self.chat_widget.copy_owned_screen_selection(&text) {
                    Ok(()) => {
                        if let Some(screen) = &mut self.owned_screen {
                            screen.show_copy_notice(char_count);
                        }
                        tui.frame_requester().schedule_frame();
                    }
                    Err(error) => self
                        .chat_widget
                        .add_error_message(format!("Copy failed: {error}")),
                }
                true
            }
        }
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
        if let Some(delay) = screen.copy_notice_delay() {
            tui.frame_requester().schedule_frame_in(delay);
        }
        Ok(Some(rendered_area))
    }
}

#[cfg(test)]
#[path = "owned_screen_tests.rs"]
mod tests;
