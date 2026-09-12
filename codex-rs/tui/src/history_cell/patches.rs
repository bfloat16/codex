//! Patch summaries and image-tool transcript helpers.

use super::*;
use crate::terminal_hyperlinks::remap_wrapped_line;
use crate::wrapping::word_wrap_line;
use codex_utils_path_uri::LegacyAppPathString;
use std::sync::Mutex;

#[derive(Debug)]
pub(crate) struct PatchHistoryCell {
    changes: HashMap<PathBuf, FileChange>,
    cwd: PathBuf,
    display_cache: Mutex<Option<(PatchDisplayCacheKey, FileChangeDisplayLines)>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PatchDisplayCacheKey {
    width: u16,
    syntax_theme_revision: u64,
    terminal_fg: Option<(u8, u8, u8)>,
    terminal_bg: Option<(u8, u8, u8)>,
    color_level: crate::terminal_palette::StdoutColorLevel,
}

impl PatchHistoryCell {
    fn cached_display_lines(&self, width: u16) -> FileChangeDisplayLines {
        let width = width.max(1);
        let key = PatchDisplayCacheKey {
            width,
            syntax_theme_revision: crate::render::highlight::syntax_theme_revision(),
            terminal_fg: crate::terminal_palette::default_fg(),
            terminal_bg: crate::terminal_palette::default_bg(),
            color_level: crate::terminal_palette::stdout_color_level(),
        };
        let mut cache = self
            .display_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached_key, lines)) = cache.as_ref()
            && *cached_key == key
        {
            return lines.clone();
        }

        let source = plain_hyperlink_lines(create_diff_summary(
            &self.changes,
            &self.cwd,
            usize::from(width),
        ));
        let mut wrapped = Vec::new();
        let mut heading_rows = 0;
        for (index, line) in source.iter().enumerate() {
            let rows = remap_wrapped_line(
                line,
                word_wrap_line(&line.line, usize::from(width))
                    .into_iter()
                    .map(|line| crate::render::line_utils::line_to_static(&line))
                    .collect(),
            );
            if index == 0 {
                heading_rows = rows.len();
            }
            wrapped.extend(rows);
        }
        let lines = FileChangeDisplayLines {
            lines: wrapped.into(),
            heading_rows,
        };
        *cache = Some((key, lines.clone()));
        lines
    }
}

impl HistoryCell for PatchHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.cached_display_lines(width)
            .lines
            .iter()
            .map(|line| line.line.clone())
            .collect()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(create_diff_summary(
            &self.changes,
            &self.cwd,
            RAW_DIFF_SUMMARY_WIDTH,
        ))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.cached_display_lines(width).lines.to_vec()
    }

    fn file_change_display_lines(&self, width: u16) -> Option<FileChangeDisplayLines> {
        Some(self.cached_display_lines(width))
    }

    fn is_file_change(&self) -> bool {
        true
    }
}
/// Create a new `PendingPatch` cell that lists the file‑level summary of
/// a proposed patch. The summary lines should already be formatted (e.g.
/// "A path/to/file.rs").
pub(crate) fn new_patch_event(
    changes: HashMap<PathBuf, FileChange>,
    cwd: &Path,
) -> PatchHistoryCell {
    PatchHistoryCell {
        changes,
        cwd: cwd.to_path_buf(),
        display_cache: Mutex::new(None),
    }
}

pub(crate) fn new_patch_apply_failure(stderr: String) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Failure title
    lines.push(Line::from("✘ Failed to apply patch".magenta().bold()));

    if !stderr.trim().is_empty() {
        let output = output_lines(
            Some(&CommandOutput::new(/*exit_code*/ 1, stderr)),
            OutputLinesParams {
                line_limit: TOOL_CALL_MAX_LINES,
                only_err: true,
                include_angle_pipe: true,
                include_prefix: true,
            },
        );
        lines.extend(output.lines);
    }

    PlainHistoryCell { lines }
}

pub(crate) fn new_view_image_tool_call(path: LegacyAppPathString, cwd: &Path) -> PlainHistoryCell {
    let display_path = path
        .to_inferred_path_uri()
        .and_then(|path| path.to_abs_path().ok())
        .map(|path| display_path_for(path.as_path(), cwd))
        .unwrap_or_else(|| path.into_string());

    let lines: Vec<Line<'static>> = vec![
        vec!["• ".dim(), "Viewed Image".bold()].into(),
        vec!["  └ ".dim(), display_path.dim()].into(),
    ];

    PlainHistoryCell { lines }
}

pub(crate) fn new_image_generation_call(
    call_id: String,
    status: &str,
    revised_prompt: Option<String>,
    saved_path: Option<AbsolutePathBuf>,
) -> PlainHistoryCell {
    let detail = revised_prompt.unwrap_or(call_id);
    let heading = if status == "failed" {
        vec!["✗ ".red().bold(), "Image generation failed".bold()].into()
    } else {
        vec!["• ".dim(), "Generated Image:".bold()].into()
    };
    let mut lines: Vec<Line<'static>> = vec![heading, vec!["  └ ".dim(), detail.dim()].into()];
    if let Some(saved_path) = saved_path {
        let saved_path = Url::from_file_path(saved_path.as_path())
            .map(|url| url.to_string())
            .unwrap_or_else(|_| saved_path.display().to_string());
        lines.push(vec!["  └ ".dim(), "Saved to: ".dim(), saved_path.into()].into());
    }

    PlainHistoryCell { lines }
}
