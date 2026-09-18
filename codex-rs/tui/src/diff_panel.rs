//! Persistent right-side Git diff panel for the owned full-screen TUI.

use crate::diff_model::FileChange;
use crate::diff_render::create_file_diff_body;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::tui::MouseScrollDirection;
use crate::width::display_width;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::path::Path;
use std::path::PathBuf;

pub(crate) const MIN_DIFF_PANEL_TERMINAL_WIDTH: u16 = 110;
const MAX_VISIBLE_FILES: usize = 8;
const BODY_SCROLL_ROWS: usize = 3;

mod parser;
mod render;

use parser::parse_git_diff;
use parser::paths_match;
pub(crate) use render::diff_panel_width;
use render::summary_line;

#[derive(Debug)]
pub(crate) struct DiffPanel {
    content: DiffPanelContent,
    selected_file: Option<usize>,
    file_list_start: usize,
    body_scroll: usize,
    revision: u64,
    layout_cache: Option<DiffPanelLayoutCache>,
    close_area: Rect,
    file_list_area: Rect,
    file_list_content_area: Rect,
    body_area: Rect,
}

#[derive(Debug)]
enum DiffPanelContent {
    Loading,
    NotRepository,
    Failed(String),
    Ready(Vec<DiffPanelFile>),
}

#[derive(Debug)]
struct DiffPanelFile {
    path: PathBuf,
    added: usize,
    removed: usize,
    change: Option<FileChange>,
}

#[derive(Debug)]
struct DiffPanelLayoutCache {
    key: DiffPanelLayoutCacheKey,
    lines: Vec<Line<'static>>,
    file_offsets: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffPanelLayoutCacheKey {
    width: u16,
    revision: u64,
    syntax_theme_revision: u64,
    terminal_fg: Option<(u8, u8, u8)>,
    terminal_bg: Option<(u8, u8, u8)>,
    color_level: crate::terminal_palette::StdoutColorLevel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffPanelClick {
    Ignored,
    Handled,
    Close,
}

impl DiffPanel {
    pub(crate) fn loading() -> Self {
        Self {
            content: DiffPanelContent::Loading,
            selected_file: None,
            file_list_start: 0,
            body_scroll: 0,
            revision: 0,
            layout_cache: None,
            close_area: Rect::default(),
            file_list_area: Rect::default(),
            file_list_content_area: Rect::default(),
            body_area: Rect::default(),
        }
    }

    pub(crate) fn set_result(&mut self, result: Result<(bool, String), String>) {
        self.content = match result {
            Ok((false, _)) => DiffPanelContent::NotRepository,
            Ok((true, text)) => DiffPanelContent::Ready(parse_git_diff(&text)),
            Err(error) => DiffPanelContent::Failed(error),
        };
        self.selected_file = self
            .files()
            .is_some_and(|files| !files.is_empty())
            .then_some(0);
        self.file_list_start = 0;
        self.body_scroll = 0;
        self.revision = self.revision.wrapping_add(1);
        self.layout_cache = None;
    }

    pub(crate) fn set_loading(&mut self) {
        self.content = DiffPanelContent::Loading;
        self.revision = self.revision.wrapping_add(1);
        self.layout_cache = None;
    }

    pub(crate) fn jump_to_path(&mut self, path: &Path) -> bool {
        let Some(files) = self.files() else {
            return false;
        };
        let Some(index) = files
            .iter()
            .position(|file| paths_match(file.path.as_path(), path))
        else {
            return false;
        };
        self.jump_to_file(index);
        true
    }

    pub(crate) fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        self.close_area = Rect::default();
        self.file_list_area = Rect::default();
        self.file_list_content_area = Rect::default();
        self.body_area = Rect::default();
        if area.is_empty() {
            return;
        }

        Clear.render(area, buffer);
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::new().dim());
        let inner = block.inner(area);
        block.render(area, buffer);
        if inner.is_empty() {
            return;
        }

        self.render_header(inner, buffer);
        match &self.content {
            DiffPanelContent::Loading => self.render_message(inner, buffer, "Loading diff…", None),
            DiffPanelContent::NotRepository => self.render_message(
                inner,
                buffer,
                "Diff unavailable",
                Some("The current directory is not in a Git repository"),
            ),
            DiffPanelContent::Failed(error) => {
                let error = error.clone();
                self.render_message(inner, buffer, "Diff unavailable", Some(&error));
            }
            DiffPanelContent::Ready(_) => self.render_ready(inner, buffer),
        }
    }

    pub(crate) fn handle_left_click(&mut self, position: Position) -> DiffPanelClick {
        if self.close_area.contains(position) {
            return DiffPanelClick::Close;
        }
        if self.file_list_content_area.contains(position) {
            let row = usize::from(position.y.saturating_sub(self.file_list_content_area.y));
            let index = self.file_list_start.saturating_add(row);
            if self.files().is_some_and(|files| index < files.len()) {
                self.jump_to_file(index);
                return DiffPanelClick::Handled;
            }
        }
        if self.body_area.contains(position) {
            return DiffPanelClick::Handled;
        }
        DiffPanelClick::Ignored
    }

    pub(crate) fn handle_mouse_scroll(
        &mut self,
        position: Position,
        direction: MouseScrollDirection,
    ) -> bool {
        if self.file_list_area.contains(position) {
            let visible_files = usize::from(self.file_list_content_area.height).max(1);
            let max_start = self
                .files()
                .map_or(0, |files| files.len().saturating_sub(visible_files));
            self.file_list_start = match direction {
                MouseScrollDirection::Up => self.file_list_start.saturating_sub(1),
                MouseScrollDirection::Down => self.file_list_start.saturating_add(1).min(max_start),
            };
            return true;
        }
        if self.body_area.contains(position) {
            let max_scroll = self.max_body_scroll();
            self.body_scroll = match direction {
                MouseScrollDirection::Up => self.body_scroll.saturating_sub(BODY_SCROLL_ROWS),
                MouseScrollDirection::Down => self
                    .body_scroll
                    .saturating_add(BODY_SCROLL_ROWS)
                    .min(max_scroll),
            };
            return true;
        }
        false
    }

    fn render_header(&mut self, area: Rect, buffer: &mut Buffer) {
        let summary = summary_line(self.files());
        let summary_area = Rect::new(
            area.x.saturating_add(1),
            area.y,
            area.width.saturating_sub(3),
            1,
        );
        Paragraph::new(summary).render(summary_area, buffer);
        self.close_area = Rect::new(area.right().saturating_sub(2), area.y, 2.min(area.width), 1);
        Paragraph::new("×".dim()).render(self.close_area, buffer);
    }

    fn render_ready(&mut self, area: Rect, buffer: &mut Buffer) {
        let files_len = self.files().map_or(0, <[DiffPanelFile]>::len);
        let base_area = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            1,
        );
        Paragraph::new("uncommitted (vs HEAD)".dim()).render(base_area, buffer);

        if files_len == 0 {
            self.render_message(area, buffer, "No uncommitted changes", None);
            return;
        }

        let available_list_rows = usize::from(area.height.saturating_sub(7)).max(1);
        let visible_files = files_len
            .min(MAX_VISIBLE_FILES)
            .min(available_list_rows.saturating_sub(2).max(1));
        let max_start = files_len.saturating_sub(visible_files);
        self.file_list_start = self.file_list_start.min(max_start);
        let has_above = self.file_list_start > 0;
        let has_below = self.file_list_start.saturating_add(visible_files) < files_len;
        let list_rows = visible_files
            .saturating_add(usize::from(has_above))
            .saturating_add(usize::from(has_below));
        let list_height = u16::try_from(list_rows).unwrap_or(u16::MAX);
        self.file_list_area = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(3),
            area.width.saturating_sub(2),
            list_height,
        );
        self.file_list_content_area = Rect::new(
            self.file_list_area.x,
            self.file_list_area.y.saturating_add(u16::from(has_above)),
            self.file_list_area.width,
            u16::try_from(visible_files).unwrap_or(u16::MAX),
        );
        self.render_file_list(buffer);

        let body_y = self.file_list_area.bottom().saturating_add(1);
        self.body_area = Rect::new(
            area.x.saturating_add(1),
            body_y,
            area.width.saturating_sub(2),
            area.bottom().saturating_sub(body_y),
        );
        if self.body_area.is_empty() {
            return;
        }
        let body_width = self.body_area.width.max(1);
        let body_height = self.body_area.height;
        let lines = self
            .body_layout(body_width)
            .map(|layout| layout.lines.clone())
            .unwrap_or_default();
        let max_scroll = lines.len().saturating_sub(usize::from(body_height));
        self.body_scroll = self.body_scroll.min(max_scroll);
        Paragraph::new(lines)
            .scroll((u16::try_from(self.body_scroll).unwrap_or(u16::MAX), 0))
            .render(self.body_area, buffer);
    }

    fn render_file_list(&self, buffer: &mut Buffer) {
        let Some(files) = self.files() else {
            return;
        };
        if self.file_list_start > 0 {
            Paragraph::new(format!("↑ {} more above", self.file_list_start).dim()).render(
                Rect::new(
                    self.file_list_area.x,
                    self.file_list_area.y,
                    self.file_list_area.width,
                    1,
                ),
                buffer,
            );
        }
        for (row, file) in files
            .iter()
            .skip(self.file_list_start)
            .take(usize::from(self.file_list_content_area.height))
            .enumerate()
        {
            let index = self.file_list_start.saturating_add(row);
            let stats = format!("+{} -{}", file.added, file.removed);
            let prefix = if self.selected_file == Some(index) {
                "› "
            } else {
                "  "
            };
            let reserved = display_width(prefix).saturating_add(display_width(&stats));
            let path_width = usize::from(self.file_list_area.width).saturating_sub(reserved);
            let path = truncate_line_with_ellipsis_if_overflow(
                Line::from(file.path.display().to_string()).dim(),
                path_width,
            );
            let path_text = path
                .spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>();
            let padding = " ".repeat(path_width.saturating_sub(display_width(&path_text)));
            let line: Line<'static> = vec![
                prefix.into(),
                path_text.dim(),
                padding.into(),
                format!("+{}", file.added).green(),
                " ".into(),
                format!("-{}", file.removed).red(),
            ]
            .into();
            let row_area = Rect::new(
                self.file_list_content_area.x,
                self.file_list_content_area
                    .y
                    .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                self.file_list_content_area.width,
                1,
            );
            Paragraph::new(line).render(row_area, buffer);
        }
        let below = files.len().saturating_sub(
            self.file_list_start
                .saturating_add(usize::from(self.file_list_content_area.height)),
        );
        if below > 0 {
            Paragraph::new(format!("↓ {below} more below").dim()).render(
                Rect::new(
                    self.file_list_area.x,
                    self.file_list_area.bottom().saturating_sub(1),
                    self.file_list_area.width,
                    1,
                ),
                buffer,
            );
        }
    }

    fn render_message(&self, area: Rect, buffer: &mut Buffer, headline: &str, hint: Option<&str>) {
        let y = area.y.saturating_add(area.height / 2).saturating_sub(1);
        let headline_area = Rect::new(area.x.saturating_add(1), y, area.width.saturating_sub(2), 1);
        Paragraph::new(headline)
            .centered()
            .render(headline_area, buffer);
        if let Some(hint) = hint {
            let hint_area = Rect::new(
                area.x.saturating_add(1),
                y.saturating_add(1),
                area.width.saturating_sub(2),
                area.bottom().saturating_sub(y.saturating_add(1)).min(2),
            );
            Paragraph::new(hint.dim())
                .centered()
                .wrap(ratatui::widgets::Wrap { trim: true })
                .render(hint_area, buffer);
        }
    }

    fn body_layout(&mut self, width: u16) -> Option<&DiffPanelLayoutCache> {
        let key = DiffPanelLayoutCacheKey {
            width,
            revision: self.revision,
            syntax_theme_revision: crate::render::highlight::syntax_theme_revision(),
            terminal_fg: crate::terminal_palette::default_fg(),
            terminal_bg: crate::terminal_palette::default_bg(),
            color_level: crate::terminal_palette::stdout_color_level(),
        };
        if self
            .layout_cache
            .as_ref()
            .is_none_or(|cache| cache.key != key)
        {
            let mut lines = Vec::new();
            let mut file_offsets = Vec::new();
            if let Some(files) = self.files() {
                for (index, file) in files.iter().enumerate() {
                    if index > 0 {
                        lines.push("".into());
                    }
                    file_offsets.push(lines.len());
                    let heading: Line<'static> = vec![
                        file.path.display().to_string().bold(),
                        " ".into(),
                        format!("+{}", file.added).green(),
                        " ".into(),
                        format!("-{}", file.removed).red(),
                    ]
                    .into();
                    lines.push(truncate_line_with_ellipsis_if_overflow(
                        heading,
                        usize::from(width),
                    ));
                    lines.push("─".repeat(usize::from(width.max(1))).dim().into());
                    if let Some(change) = &file.change {
                        lines.extend(create_file_diff_body(
                            file.path.as_path(),
                            change,
                            usize::from(width),
                        ));
                    } else {
                        lines.push("Binary or mode-only change".dim().italic().into());
                    }
                }
            }
            self.layout_cache = Some(DiffPanelLayoutCache {
                key,
                lines,
                file_offsets,
            });
        }
        self.layout_cache.as_ref()
    }

    fn jump_to_file(&mut self, index: usize) {
        let files_len = self.files().map_or(0, <[DiffPanelFile]>::len);
        if index >= files_len {
            return;
        }
        self.selected_file = Some(index);
        let visible_files =
            usize::from(self.file_list_content_area.height).clamp(1, MAX_VISIBLE_FILES);
        if index < self.file_list_start {
            self.file_list_start = index;
        } else if index >= self.file_list_start.saturating_add(visible_files) {
            self.file_list_start = index.saturating_sub(visible_files.saturating_sub(1));
        }
        let body_width = self.body_area.width.max(1);
        self.body_scroll = self
            .body_layout(body_width)
            .and_then(|layout| layout.file_offsets.get(index))
            .copied()
            .unwrap_or(0);
    }

    fn max_body_scroll(&mut self) -> usize {
        let width = self.body_area.width.max(1);
        self.body_layout(width)
            .map_or(0, |layout| layout.lines.len())
            .saturating_sub(usize::from(self.body_area.height))
    }

    fn files(&self) -> Option<&[DiffPanelFile]> {
        match &self.content {
            DiffPanelContent::Ready(files) => Some(files),
            DiffPanelContent::Loading
            | DiffPanelContent::NotRepository
            | DiffPanelContent::Failed(_) => None,
        }
    }
}

#[cfg(test)]
#[path = "diff_panel_tests.rs"]
mod tests;
