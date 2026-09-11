//! Paragraph rendering that keeps visible text and hyperlink annotations aligned.

use super::HyperlinkLine;
use super::mark_buffer_hyperlinks;
use super::visible_lines_ref;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

/// Word-wraps without trimming and applies the same vertical scroll to text and links.
pub(crate) struct HyperlinkParagraph<'a> {
    lines: &'a [HyperlinkLine],
    paragraph: Paragraph<'a>,
    scroll_rows: u16,
}

impl<'a> HyperlinkParagraph<'a> {
    pub(crate) fn new(lines: &'a [HyperlinkLine], style: Style) -> Self {
        Self {
            lines,
            paragraph: Paragraph::new(Text::from(visible_lines_ref(lines)))
                .style(style)
                .wrap(Wrap { trim: false }),
            scroll_rows: 0,
        }
    }

    pub(crate) fn line_count(&self, width: u16) -> usize {
        self.paragraph.line_count(width)
    }

    pub(crate) fn scroll(mut self, rows: u16) -> Self {
        self.scroll_rows = rows;
        self
    }
}

impl Widget for HyperlinkParagraph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        fill_line_backgrounds(self.lines, area, buf, usize::from(self.scroll_rows));
        self.paragraph
            .scroll((self.scroll_rows, 0))
            .render(area, buf);
        mark_buffer_hyperlinks(buf, area, self.lines, usize::from(self.scroll_rows));
    }
}

fn fill_line_backgrounds(
    lines: &[HyperlinkLine],
    area: Rect,
    buf: &mut Buffer,
    scroll_rows: usize,
) {
    if area.is_empty() {
        return;
    }
    let backgrounds = lines.iter().flat_map(|line| {
        let wrapped_rows = Paragraph::new(line.line.clone())
            .wrap(Wrap { trim: false })
            .line_count(area.width)
            .max(/*other*/ 1);
        std::iter::repeat_n(line.line.style.bg, wrapped_rows)
    });
    for (row, background) in backgrounds
        .skip(scroll_rows)
        .take(usize::from(area.height))
        .enumerate()
    {
        let Some(background) = background else {
            continue;
        };
        let Ok(row) = u16::try_from(row) else {
            break;
        };
        let y = area.y.saturating_add(row);
        for x in area.x..area.right() {
            buf[(x, y)].set_bg(background);
        }
    }
}
