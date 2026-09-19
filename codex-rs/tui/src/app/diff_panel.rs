//! Application wiring for the owned-screen Git diff panel.

use super::*;
use crate::session_resume::cwds_differ;

impl App {
    pub(super) fn toggle_diff_panel(&mut self, tui: &mut tui::Tui) {
        let Some(screen) = self.owned_screen.as_mut() else {
            self.request_diff(/*panel_generation*/ None);
            return;
        };
        if screen.is_diff_panel_open() {
            screen.close_diff_panel();
            tui.frame_requester().schedule_frame();
            return;
        }

        let terminal_width = match tui.terminal.size() {
            Ok(size) => size.width,
            Err(error) => {
                self.chat_widget
                    .add_error_message(format!("Failed to read terminal size: {error}"));
                return;
            }
        };
        let Some(panel_width) = crate::diff_panel::diff_panel_width(terminal_width) else {
            self.chat_widget.add_error_message(format!(
                "Resize the terminal to at least {} columns to show the diff panel.",
                crate::diff_panel::MIN_DIFF_PANEL_TERMINAL_WIDTH,
            ));
            return;
        };
        let conversation_width = self.chat_widget.history_wrap_width(
            terminal_width
                .saturating_sub(panel_width)
                .saturating_sub(crate::diff_panel::DIFF_PANEL_GAP),
        );
        let Some(generation) = screen.open_diff_panel(terminal_width, conversation_width) else {
            return;
        };
        self.request_diff(Some(generation));
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn refresh_diff_panel(&mut self) {
        let Some(generation) = self
            .owned_screen
            .as_mut()
            .filter(|screen| screen.is_diff_panel_open())
            .map(OwnedScreen::begin_diff_panel_refresh)
        else {
            return;
        };
        self.request_diff(Some(generation));
    }

    pub(super) fn handle_diff_result(
        &mut self,
        tui: &mut tui::Tui,
        cwd: PathBuf,
        panel_generation: Option<u64>,
        result: Result<(bool, String), String>,
    ) {
        if cwds_differ(&cwd, self.chat_widget.current_working_directory()) {
            return;
        }
        if let Some(generation) = panel_generation {
            if self
                .owned_screen
                .as_mut()
                .is_some_and(|screen| screen.apply_diff_panel_result(generation, result))
            {
                tui.frame_requester().schedule_frame();
            }
            return;
        }

        let text = match result {
            Ok((true, text)) => text,
            Ok((false, _)) => "`/diff` — _not inside a git repository_".to_string(),
            Err(error) => format!("Failed to compute diff: {error}"),
        };
        let _ = tui.enter_alt_screen();
        let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
            vec!["No changes detected.".italic().into()]
        } else {
            text.lines().map(ansi_escape_line).collect()
        };
        self.overlay = Some(Overlay::new_static_with_lines(
            pager_lines,
            "D I F F".to_string(),
            self.keymap.pager.clone(),
        ));
        tui.frame_requester().schedule_frame();
    }

    fn request_diff(&self, panel_generation: Option<u64>) {
        let tx = self.app_event_tx.clone();
        let runner = self.workspace_command_runner.clone();
        let cwd = self.chat_widget.current_working_directory().to_path_buf();
        tokio::spawn(async move {
            let result = match runner {
                Some(runner) if panel_generation.is_some() => {
                    crate::get_git_diff::get_git_diff_for_panel(runner.as_ref(), &cwd).await
                }
                Some(runner) => {
                    crate::get_git_diff::get_git_diff(
                        runner.as_ref(),
                        &cwd,
                        crate::get_git_diff::GitDiffColor::Always,
                    )
                    .await
                }
                None => Err("workspace command runner unavailable".to_string()),
            };
            tx.send(AppEvent::DiffResult {
                cwd,
                panel_generation,
                result,
            });
        });
    }
}
