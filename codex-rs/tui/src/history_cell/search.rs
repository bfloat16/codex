//! Web-search activity history cells.

use super::*;

fn web_search_action_detail(action: &WebSearchAction) -> String {
    match action {
        WebSearchAction::Search { query, queries } => {
            query.clone().filter(|q| !q.is_empty()).unwrap_or_else(|| {
                let items = queries.as_ref();
                let first = items
                    .and_then(|queries| queries.first())
                    .cloned()
                    .unwrap_or_default();
                if items.is_some_and(|queries| queries.len() > 1) && !first.is_empty() {
                    format!("{first} ...")
                } else {
                    first
                }
            })
        }
        WebSearchAction::OpenPage { url } => url.clone().unwrap_or_default(),
        WebSearchAction::FindInPage { url, pattern } => match (pattern, url) {
            (Some(pattern), Some(url)) => format!("'{pattern}' in {url}"),
            (Some(pattern), None) => format!("'{pattern}'"),
            (None, Some(url)) => url.clone(),
            (None, None) => String::new(),
        },
        WebSearchAction::Other => String::new(),
    }
}

fn web_search_detail(action: Option<&WebSearchAction>, query: &str) -> String {
    let detail = action.map(web_search_action_detail).unwrap_or_default();
    if detail.is_empty() {
        query.to_string()
    } else {
        detail
    }
}

#[derive(Debug)]
pub(crate) struct WebSearchCell {
    call_id: String,
    query: String,
    action: Option<WebSearchAction>,
    start_time: Instant,
    completed: bool,
    completed_at: Option<Instant>,
    animations_enabled: bool,
}

impl WebSearchCell {
    pub(crate) fn new(
        call_id: String,
        query: String,
        action: Option<WebSearchAction>,
        animations_enabled: bool,
    ) -> Self {
        Self {
            call_id,
            query,
            action,
            start_time: Instant::now(),
            completed: false,
            completed_at: None,
            animations_enabled,
        }
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.call_id
    }

    pub(crate) fn update(&mut self, action: WebSearchAction, query: String) {
        self.action = Some(action);
        self.query = query;
    }

    pub(crate) fn complete(&mut self) {
        self.completed = true;
        self.completed_at = Some(Instant::now());
    }
}

impl HistoryCell for WebSearchCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let bullet = if self.completed {
            "●".dim()
        } else {
            activity_indicator(
                Some(self.start_time),
                MotionMode::from_animations_enabled(self.animations_enabled),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "●".dim())
        };
        let header = "Searched the web";
        let detail = web_search_detail(self.action.as_ref(), &self.query);
        let text: Text<'static> = if detail.is_empty() {
            Line::from(vec![header.bold()]).into()
        } else {
            let separator = " for ";
            Line::from(vec![header.bold(), separator.into(), detail.into()]).into()
        };
        PrefixedWrappedHistoryCell::new(text, vec![bullet, " ".into()], "  ").display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let header = "Searched the web";
        let detail = web_search_detail(self.action.as_ref(), &self.query);
        if detail.is_empty() {
            vec![Line::from(header)]
        } else {
            let separator = " for ";
            vec![Line::from(format!("{header}{separator}{detail}"))]
        }
    }

    fn tool_activity(&self) -> Option<ToolActivity> {
        Some(ToolActivity {
            call_count: 1,
            searches: 1,
            ..ToolActivity::default()
        })
    }

    fn tool_group_preview_lines(&self) -> Vec<Line<'static>> {
        let detail = web_search_detail(self.action.as_ref(), &self.query);
        let mut spans = vec!["Searched".cyan(), " the web".into()];
        if !detail.is_empty() {
            spans.extend([" for ".into(), detail.into()]);
        }
        vec![spans.into()]
    }

    fn tool_group_preview_is_active(&self) -> bool {
        !self.completed
    }

    fn tool_group_previews(&self) -> Vec<ToolPreview> {
        self.tool_group_preview_lines()
            .into_iter()
            .map(|line| ToolPreview {
                line,
                running: !self.completed,
                completed_at: self.completed_at,
            })
            .collect()
    }

    fn tool_render_state(&self, now: Instant) -> ToolRenderState {
        ToolRenderState {
            running: self.tool_group_preview_is_active(),
            next_expiry: self
                .completed_at
                .map(|at| at + TOOL_COMPLETION_RETENTION)
                .filter(|at| *at > now),
            ..ToolRenderState::default()
        }
    }
}

pub(crate) fn new_active_web_search_call(
    call_id: String,
    query: String,
    animations_enabled: bool,
) -> WebSearchCell {
    WebSearchCell::new(call_id, query, /*action*/ None, animations_enabled)
}

pub(crate) fn new_web_search_call(
    call_id: String,
    query: String,
    action: WebSearchAction,
) -> WebSearchCell {
    let mut cell = WebSearchCell::new(
        call_id,
        query,
        Some(action),
        /*animations_enabled*/ false,
    );
    cell.complete();
    cell
}
