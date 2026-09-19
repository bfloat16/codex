use ratatui::style::Stylize;
use ratatui::text::Line;

use super::DiffPanelFile;
use super::MIN_DIFF_PANEL_TERMINAL_WIDTH;

const PANEL_WIDTH_PERCENT: u16 = 40;
pub(crate) const DIFF_PANEL_GAP: u16 = 1;

pub(crate) fn diff_panel_width(terminal_width: u16) -> Option<u16> {
    if terminal_width < MIN_DIFF_PANEL_TERMINAL_WIDTH {
        return None;
    }
    Some(
        terminal_width
            .saturating_mul(PANEL_WIDTH_PERCENT)
            .saturating_div(100),
    )
}

pub(super) fn summary_line(files: Option<&[DiffPanelFile]>) -> Line<'static> {
    let Some(files) = files else {
        return Line::default();
    };
    let added = files.iter().map(|file| file.added).sum::<usize>();
    let removed = files.iter().map(|file| file.removed).sum::<usize>();
    vec![
        format!(
            "{} {}",
            files.len(),
            if files.len() == 1 { "file" } else { "files" }
        )
        .bold(),
        " changed ".into(),
        format!("+{added}").green(),
        " ".into(),
        format!("-{removed}").red(),
    ]
    .into()
}
