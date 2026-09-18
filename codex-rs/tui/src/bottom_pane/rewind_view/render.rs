use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::line_utils::line_to_static;
use crate::render::line_utils::prefix_lines;
use crate::render::renderable::Renderable;
use crate::style::StatusTone;
use crate::style::accent_style;
use crate::style::status_style;
use crate::wrapping::word_wrap_line;

use super::MAX_PROMPT_LINES;
use super::PROMPT_ROW_HEIGHT;
use super::PROMPT_SCROLL_HINT_ROWS;
use super::RewindPromptItem;
use super::RewindRestoreOption;
use super::RewindView;
use super::RewindViewKind;
use crate::bottom_pane::popup_consts::MAX_POPUP_ROWS;
use crate::bottom_pane::selection_popup_common::render_menu_surface;
use crate::bottom_pane::selection_popup_common::wrap_styled_line;

impl RewindView {
    fn prompt_window(&self, visible_items: usize, item_count: usize) -> (usize, usize) {
        if item_count == 0 || visible_items == 0 {
            return (0, 0);
        }
        let selected_idx = self.selected_idx.unwrap_or(0).min(item_count - 1);
        let visible_items = visible_items.min(item_count);
        let start = selected_idx
            .saturating_sub(visible_items / 2)
            .min(item_count - visible_items);
        (start, start + visible_items)
    }

    fn prompt_content_height(item_count: usize) -> u16 {
        let visible_items = item_count.clamp(1, MAX_POPUP_ROWS) as u16;
        PROMPT_SCROLL_HINT_ROWS.saturating_add(visible_items.saturating_mul(PROMPT_ROW_HEIGHT))
    }

    fn render_prompt_picker(&self, area: Rect, buf: &mut Buffer, items: &[RewindPromptItem]) {
        let [title_area, subtitle_area, _, list_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);

        Line::from("Rewind")
            .style(accent_style())
            .render(title_area, buf);
        Line::from("Restore the code and/or conversation to the point before…")
            .render(subtitle_area, buf);

        if list_area.height == 0 {
            return;
        }
        let [above_area, rows_area, below_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(list_area);
        let visible_items = usize::from((rows_area.height / PROMPT_ROW_HEIGHT).max(1));
        let (start, end) = self.prompt_window(visible_items, items.len());

        if start > 0 {
            Line::from(format!("↑ {start} more above"))
                .dim()
                .render(above_area, buf);
        }

        for (row_index, item_index) in (start..end).enumerate() {
            let Some(item) = items.get(item_index) else {
                continue;
            };
            let y = rows_area
                .y
                .saturating_add((row_index as u16).saturating_mul(PROMPT_ROW_HEIGHT));
            if y >= rows_area.y.saturating_add(rows_area.height) {
                break;
            }
            let selected = self.selected_idx == Some(item_index);
            let prompt = if item.is_current {
                if selected {
                    "(current)".italic().set_style(accent_style())
                } else {
                    "(current)".italic()
                }
            } else if selected {
                item.prompt.clone().set_style(accent_style())
            } else {
                item.prompt.clone().into()
            };
            let prompt_line = truncate_line_with_ellipsis_if_overflow(
                Line::from(vec![
                    if selected {
                        Span::from("❯ ").style(accent_style())
                    } else {
                        "  ".into()
                    },
                    prompt,
                ]),
                usize::from(rows_area.width.saturating_sub(2)),
            );
            prompt_line.render(Rect::new(rows_area.x, y, rows_area.width, 1), buf);

            if let Some(summary) = &item.code_summary
                && y.saturating_add(1) < rows_area.y.saturating_add(rows_area.height)
            {
                let summary = truncate_line_with_ellipsis_if_overflow(
                    summary.clone(),
                    usize::from(rows_area.width.saturating_sub(2)),
                );
                let summary = if selected { summary } else { summary.dim() };
                summary.render(
                    Rect::new(
                        rows_area.x.saturating_add(2),
                        y.saturating_add(1),
                        rows_area.width.saturating_sub(2),
                        1,
                    ),
                    buf,
                );
            }
        }

        if end < items.len() {
            Line::from(format!("↓ {} more below", items.len() - end))
                .dim()
                .render(below_area, buf);
        }
    }

    fn restore_content_lines(
        &self,
        width: u16,
        prompt: &str,
        options: &[RewindRestoreOption],
        warning: &Line<'static>,
    ) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from("Rewind").style(accent_style())];
        lines.extend(
            wrap_styled_line(
                &Line::from(
                    "Confirm you want to restore to the point before you sent this message:",
                ),
                width,
            )
            .into_iter()
            .map(|line| line_to_static(&line)),
        );

        let prompt_line = Line::from(prompt.to_string());
        let prompt_width = usize::from(width.saturating_sub(2).max(1));
        let prompt_lines = word_wrap_line(&prompt_line, prompt_width)
            .into_iter()
            .take(MAX_PROMPT_LINES)
            .map(|line| line_to_static(&line))
            .collect();
        lines.extend(prefix_lines(prompt_lines, "│ ".dim(), "│ ".dim()));
        lines.push(Line::from(""));

        if let Some(option) = self.selected_idx.and_then(|index| options.get(index)) {
            for detail in &option.details {
                lines.extend(
                    wrap_styled_line(detail, width)
                        .into_iter()
                        .map(|line| line_to_static(&line)),
                );
            }
        }
        lines.push(Line::from(""));

        for (index, option) in options.iter().enumerate() {
            let selected = self.selected_idx == Some(index);
            lines.push(Line::from(vec![
                if selected {
                    Span::from("❯ ").style(accent_style())
                } else {
                    "  ".into()
                },
                format!("{}. ", index + 1).dim(),
                if selected {
                    Span::from(option.label.clone()).style(accent_style())
                } else {
                    option.label.clone().into()
                },
            ]));
        }
        lines.push(Line::from(""));

        let mut warning = warning.clone().dim();
        warning.spans.insert(
            0,
            Span::from("⚠ ").style(status_style(StatusTone::Attention)),
        );
        lines.extend(
            wrap_styled_line(&warning, width)
                .into_iter()
                .map(|line| line_to_static(&line)),
        );
        lines
    }

    fn render_restore_picker(
        &self,
        area: Rect,
        buf: &mut Buffer,
        prompt: &str,
        options: &[RewindRestoreOption],
        warning: &Line<'static>,
    ) {
        let lines = self.restore_content_lines(area.width.max(1), prompt, options, warning);
        for (index, line) in lines.into_iter().enumerate() {
            let y = area.y.saturating_add(index as u16);
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            line.render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }
}

impl Renderable for RewindView {
    fn desired_height(&self, width: u16) -> u16 {
        let inner_width = width.saturating_sub(4).max(1);
        let content_height = match &self.kind {
            RewindViewKind::Prompts { items } => {
                3_u16.saturating_add(Self::prompt_content_height(items.len()))
            }
            RewindViewKind::Restore {
                prompt,
                options,
                warning,
            } => self
                .restore_content_lines(inner_width, prompt, options, warning)
                .len()
                .try_into()
                .unwrap_or(u16::MAX),
        };
        content_height.saturating_add(/*surface padding*/ 2 + /*footer*/ 1)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        let content_area = render_menu_surface(content_area, buf);
        match &self.kind {
            RewindViewKind::Prompts { items } => {
                self.render_prompt_picker(content_area, buf, items)
            }
            RewindViewKind::Restore {
                prompt,
                options,
                warning,
            } => self.render_restore_picker(content_area, buf, prompt, options, warning),
        }

        self.footer_hint.clone().dim().render(
            Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            ),
            buf,
        );
    }
}
