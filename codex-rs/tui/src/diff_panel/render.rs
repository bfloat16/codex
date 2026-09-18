use ratatui::style::Stylize;
use ratatui::text::Line;

use super::DiffPanelFile;
use super::MIN_DIFF_PANEL_TERMINAL_WIDTH;

const MIN_CONVERSATION_WIDTH: u16 = 70;
const MAX_PANEL_WIDTH: u16 = 90;

pub(crate) fn diff_panel_width(terminal_width: u16) -> Option<u16> {
    if terminal_width < MIN_DIFF_PANEL_TERMINAL_WIDTH {
        return None;
    }
    Some(
        terminal_width
            .saturating_mul(45)
            .saturating_div(100)
            .min(MAX_PANEL_WIDTH)
            .min(terminal_width.saturating_sub(MIN_CONVERSATION_WIDTH)),
    )
}

pub(super) fn summary_line(files: Option<&[DiffPanelFile]>) -> Line<'static> {
    let Some(files) = files else {
        return "Diff".bold().into();
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
