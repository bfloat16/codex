use crossterm::event::KeyEvent;
use ratatui::text::Line;

use crate::app_event_sender::AppEventSender;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::KeymapContext;
use crate::keymap::KeymapContextSet;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;

use super::CancellationEvent;
use super::ViewCompletion;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::accept_cancel_hint_line;

mod render;

const PROMPT_ROW_HEIGHT: u16 = 3;
const PROMPT_SCROLL_HINT_ROWS: u16 = 2;
const MAX_PROMPT_LINES: usize = 4;
const MAX_REWIND_ROWS: usize = 3;

pub(crate) type RewindAction = Box<dyn Fn(&AppEventSender) + Send + Sync>;

pub(crate) struct RewindPromptItem {
    pub(crate) prompt: String,
    pub(crate) code_summary: Option<Line<'static>>,
    pub(crate) is_current: bool,
    pub(crate) action: RewindAction,
}

pub(crate) struct RewindRestoreOption {
    pub(crate) label: String,
    pub(crate) details: Vec<Line<'static>>,
    pub(crate) action: RewindAction,
}

pub(crate) enum RewindViewParams {
    Prompts {
        view_id: &'static str,
        items: Vec<RewindPromptItem>,
        on_cancel: RewindAction,
    },
    Restore {
        view_id: &'static str,
        prompt: String,
        options: Vec<RewindRestoreOption>,
        warning: Line<'static>,
        on_cancel: RewindAction,
    },
}

enum RewindViewKind {
    Prompts {
        items: Vec<RewindPromptItem>,
    },
    Restore {
        prompt: String,
        options: Vec<RewindRestoreOption>,
        warning: Line<'static>,
    },
}

pub(crate) struct RewindView {
    view_id: &'static str,
    kind: RewindViewKind,
    selected_idx: Option<usize>,
    completion: Option<ViewCompletion>,
    dismiss_after_child_accept: bool,
    app_event_tx: AppEventSender,
    on_cancel: RewindAction,
    keymap: ListKeymap,
    footer_hint: Line<'static>,
}

impl RewindView {
    pub(crate) fn new(
        params: RewindViewParams,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        let (view_id, kind, selected_idx, cancel_label, on_cancel) = match params {
            RewindViewParams::Prompts {
                view_id,
                items,
                on_cancel,
            } => {
                let selected_idx = items.len().checked_sub(1);
                (
                    view_id,
                    RewindViewKind::Prompts { items },
                    selected_idx,
                    "to cancel",
                    on_cancel,
                )
            }
            RewindViewParams::Restore {
                view_id,
                prompt,
                options,
                warning,
                on_cancel,
            } => {
                let selected_idx = (!options.is_empty()).then_some(0);
                (
                    view_id,
                    RewindViewKind::Restore {
                        prompt,
                        options,
                        warning,
                    },
                    selected_idx,
                    "to go back",
                    on_cancel,
                )
            }
        };
        let footer_hint = accept_cancel_hint_line(
            keymap.primary_hint(ListAction::Accept),
            "to continue",
            keymap.primary_hint(ListAction::Cancel),
            cancel_label,
        );
        Self {
            view_id,
            kind,
            selected_idx,
            completion: None,
            dismiss_after_child_accept: false,
            app_event_tx,
            on_cancel,
            keymap,
            footer_hint,
        }
    }

    fn item_count(&self) -> usize {
        match &self.kind {
            RewindViewKind::Prompts { items } => items.len(),
            RewindViewKind::Restore { options, .. } => options.len(),
        }
    }

    fn move_up(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        self.selected_idx = Some(selected_idx.saturating_sub(1));
    }

    fn move_down(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        self.selected_idx = Some(
            selected_idx
                .saturating_add(1)
                .min(self.item_count().saturating_sub(1)),
        );
    }

    fn jump_top(&mut self) {
        if self.item_count() > 0 {
            self.selected_idx = Some(0);
        }
    }

    fn jump_bottom(&mut self) {
        self.selected_idx = self.item_count().checked_sub(1);
    }

    fn page_up(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        self.selected_idx = Some(selected_idx.saturating_sub(MAX_REWIND_ROWS));
    }

    fn page_down(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        self.selected_idx = Some(
            selected_idx
                .saturating_add(MAX_REWIND_ROWS)
                .min(self.item_count().saturating_sub(1)),
        );
    }

    fn accept(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        let action = match &self.kind {
            RewindViewKind::Prompts { items } => items
                .get(selected_idx)
                .map(|item| (&item.action, !item.is_current)),
            RewindViewKind::Restore { options, .. } => options
                .get(selected_idx)
                .map(|option| (&option.action, false)),
        };
        let Some((action, waits_for_child)) = action else {
            return;
        };
        action(&self.app_event_tx);
        if waits_for_child {
            self.dismiss_after_child_accept = true;
        } else {
            self.completion = Some(ViewCompletion::Accepted);
        }
    }

    fn cancel(&mut self) {
        (self.on_cancel)(&self.app_event_tx);
        self.completion = Some(ViewCompletion::Cancelled);
    }
}

impl BottomPaneView for RewindView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            _ if self.keymap.move_up.is_pressed(key_event) => self.move_up(),
            _ if self.keymap.move_down.is_pressed(key_event) => self.move_down(),
            _ if self.keymap.page_up.is_pressed(key_event) => self.page_up(),
            _ if self.keymap.page_down.is_pressed(key_event) => self.page_down(),
            _ if self.keymap.jump_top.is_pressed(key_event) => self.jump_top(),
            _ if self.keymap.jump_bottom.is_pressed(key_event) => self.jump_bottom(),
            _ if self.keymap.accept.is_pressed(key_event) => self.accept(),
            _ if self.keymap.cancel.is_pressed(key_event) => self.cancel(),
            _ => {}
        }
    }

    fn keymap_contexts(&self) -> KeymapContextSet {
        KeymapContextSet::new(KeymapContext::List)
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn dismiss_after_child_accept(&self) -> bool {
        self.dismiss_after_child_accept
    }

    fn clear_dismiss_after_child_accept(&mut self) {
        self.dismiss_after_child_accept = false;
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(self.view_id)
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_idx
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.cancel();
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[path = "rewind_view_tests.rs"]
mod tests;
