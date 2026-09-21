//! Shared tool entries retain their position when later messages arrive before completion.

use super::*;
use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;

pub(crate) const TOOL_COMPLETION_RETENTION: Duration = Duration::from_secs(/*secs*/ 2);

/// Cheap invalidation state; checking a tool must not format or highlight its command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolRenderState {
    pub(crate) revision: u64,
    pub(crate) running: bool,
    pub(crate) next_expiry: Option<Instant>,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolPreview {
    pub(crate) line: Line<'static>,
    pub(crate) running: bool,
    pub(crate) completed_at: Option<Instant>,
}

impl ToolPreview {
    pub(crate) fn visible_at(&self, now: Instant) -> bool {
        self.running
            || self
                .completed_at
                .is_some_and(|at| now.saturating_duration_since(at) < TOOL_COMPLETION_RETENTION)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LiveToolCell(Arc<Mutex<LiveToolState>>);

#[derive(Debug)]
struct LiveToolState {
    cell: Box<dyn HistoryCell>,
    revision: u64,
}

pub(crate) struct LiveToolGuard<'a>(MutexGuard<'a, LiveToolState>);

impl Deref for LiveToolGuard<'_> {
    type Target = Box<dyn HistoryCell>;

    fn deref(&self) -> &Self::Target {
        &self.0.cell
    }
}

impl DerefMut for LiveToolGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.revision = self.0.revision.wrapping_add(1);
        &mut self.0.cell
    }
}

impl LiveToolCell {
    pub(crate) fn new(cell: Box<dyn HistoryCell>) -> Self {
        Self(Arc::new(Mutex::new(LiveToolState { cell, revision: 0 })))
    }

    pub(crate) fn lock(&self) -> LiveToolGuard<'_> {
        LiveToolGuard(self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl HistoryCell for LiveToolCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.lock().display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.lock().raw_lines()
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.lock().transcript_lines(width)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.lock().transcript_hyperlink_lines(width)
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.lock().display_hyperlink_lines(width)
    }

    fn tool_group_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.lock().tool_group_detail_lines(width)
    }

    fn tool_render_state(&self, now: Instant) -> ToolRenderState {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ToolRenderState {
            revision: state.revision,
            ..state.cell.tool_render_state(now)
        }
    }

    fn tool_activity(&self) -> Option<ToolActivity> {
        self.lock().tool_activity()
    }

    fn tool_group_previews(&self) -> Vec<ToolPreview> {
        self.lock().tool_group_previews()
    }

    fn tool_group_preview_is_active(&self) -> bool {
        self.lock().tool_group_preview_is_active()
    }

    fn has_stable_transcript_height(&self) -> bool {
        false
    }
}
