//! A live task status row rendered above the composer while the agent is busy.
//!
//! The row renders a separately owned clock and short inline
//! context (for example, the unified-exec background-process summary). Keeping
//! these pieces on one line avoids vertical layout churn in the bottom pane.
//! Hook activity uses the remaining space or its own line on overflow, so it
//! never displaces background-process status.

use std::time::Duration;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::app_event_sender::AppEventSender;
use crate::line_truncation::line_width;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::motion::shimmer_text;
use crate::render::renderable::Renderable;
use crate::text_formatting::capitalize_first;
use crate::tui::FrameRequester;
use crate::width::display_width;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

mod timer;
pub(crate) use timer::StatusTimer;

pub(crate) const STATUS_DETAILS_DEFAULT_MAX_LINES: usize = 3;
const DETAILS_PREFIX: &str = "  └ ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusDetailsCapitalization {
    CapitalizeFirst,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelTransferPhase {
    Sending,
    Receiving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModelTransferStatus {
    pub(crate) phase: ModelTransferPhase,
    pub(crate) sent_bytes: u64,
    pub(crate) received_bytes: u64,
}

/// Displays a single-line in-progress status with optional wrapped details.
pub(crate) struct StatusIndicatorWidget {
    /// Animated header text (defaults to "Working").
    header: String,
    details: Option<String>,
    details_max_lines: usize,
    model_transfer: Option<ModelTransferStatus>,
    /// Hook activity may move below the status row when it cannot fit in full.
    hook_status_message: Option<String>,
    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    animations_enabled: bool,
    waiting_animation_started_at: Option<Instant>,
    waiting_animation_duration: Duration,
    api_error: bool,
}

// Format elapsed seconds into a compact human-friendly form used by the status line.
// Examples: 0s, 59s, 1m 00s, 59m 59s, 1h 00m 00s, 2h 03m 09s
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

impl StatusIndicatorWidget {
    pub(crate) fn new(
        app_event_tx: AppEventSender,
        frame_requester: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            header: String::from("Working"),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
            model_transfer: None,
            hook_status_message: None,
            app_event_tx,
            frame_requester,
            animations_enabled,
            waiting_animation_started_at: None,
            waiting_animation_duration: Duration::from_secs(30),
            api_error: false,
        }
    }

    pub(crate) fn interrupt(&self) {
        self.app_event_tx.interrupt();
    }

    /// Update the animated header label (left of the brackets).
    pub(crate) fn update_header(&mut self, header: String) {
        self.api_error = false;
        if header != "Waiting" && !header.starts_with("Waiting ") {
            self.waiting_animation_started_at = None;
        }
        self.header = header;
    }

    pub(crate) fn reset_waiting_animation(&mut self, duration: Duration) {
        self.waiting_animation_duration = duration;
        self.waiting_animation_started_at = Some(Instant::now());
        self.frame_requester.schedule_frame();
    }

    pub(crate) fn reset_api_error_animation(&mut self, duration: Duration) {
        self.api_error = true;
        self.waiting_animation_duration = duration;
        self.waiting_animation_started_at = Some(Instant::now());
        self.frame_requester.schedule_frame();
    }

    /// Update the details text shown below the header.
    pub(crate) fn update_details(
        &mut self,
        details: Option<String>,
        capitalization: StatusDetailsCapitalization,
        max_lines: usize,
    ) {
        self.details_max_lines = max_lines.max(1);
        self.details = details
            .filter(|details| !details.is_empty())
            .map(|details| {
                let trimmed = details.trim_start();
                match capitalization {
                    StatusDetailsCapitalization::CapitalizeFirst => capitalize_first(trimmed),
                    StatusDetailsCapitalization::Preserve => trimmed.to_string(),
                }
            });
    }

    pub(crate) fn update_model_transfer(&mut self, status: Option<ModelTransferStatus>) {
        self.model_transfer = status;
        self.frame_requester.schedule_frame();
    }

    pub(crate) fn update_hook_status_message(&mut self, message: Option<String>) {
        self.hook_status_message = message;
    }

    pub(crate) fn header(&self) -> &str {
        &self.header
    }

    #[cfg(test)]
    pub(crate) fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }

    pub(crate) fn with_timer<'a>(&'a self, timer: &'a StatusTimer) -> impl Renderable + 'a {
        StatusIndicator { row: self, timer }
    }

    /// Wrap the details text into a fixed width and return the lines, truncating if necessary.
    fn wrapped_details_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(details) = self.details.as_deref() else {
            return Vec::new();
        };
        if width == 0 {
            return Vec::new();
        }

        let prefix_width = UnicodeWidthStr::width(DETAILS_PREFIX);
        let opts = RtOptions::new(usize::from(width))
            .initial_indent(Line::from(DETAILS_PREFIX.dim()))
            .subsequent_indent(Line::from(Span::from(" ".repeat(prefix_width)).dim()))
            .break_words(/*break_words*/ true);

        let mut out = word_wrap_lines(details.lines().map(|line| vec![line.dim()]), opts);

        if out.len() > self.details_max_lines {
            out.truncate(self.details_max_lines);
            let content_width = usize::from(width).saturating_sub(prefix_width).max(1);
            let max_base_len = content_width.saturating_sub(1);
            if let Some(last) = out.last_mut()
                && let Some(span) = last.spans.last_mut()
            {
                let trimmed: String = span.content.as_ref().chars().take(max_base_len).collect();
                *span = format!("{trimmed}…").dim();
            }
        }

        out
    }
}

struct StatusIndicator<'a> {
    row: &'a StatusIndicatorWidget,
    timer: &'a StatusTimer,
}

impl StatusIndicator<'_> {
    // Share width decisions between height measurement and rendering, including
    // wide Unicode characters and elapsed-time text.
    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let row = self.row;
        let now = Instant::now();
        let elapsed_duration = self.timer.display_started_at.map_or_else(
            || self.timer.elapsed_at(now),
            |started_at| now.saturating_duration_since(started_at),
        );
        let pretty_elapsed = fmt_elapsed_compact(elapsed_duration.as_secs());
        let motion_mode = MotionMode::from_animations_enabled(row.animations_enabled);
        let waiting = row.header == "Waiting" || row.header.starts_with("Waiting ");
        let attention_animation_active = (waiting || row.api_error)
            && row.waiting_animation_started_at.is_none_or(|started_at| {
                now.saturating_duration_since(started_at) < row.waiting_animation_duration
            });
        let attention_color = if row.api_error {
            Color::LightRed
        } else {
            Color::Yellow
        };

        let mut spans = Vec::with_capacity(9);
        if let Some(indicator) = activity_indicator(
            Some(self.timer.last_resume_at),
            motion_mode,
            ReducedMotionIndicator::Hidden,
        ) {
            let indicator = Span::styled("●", indicator.style);
            spans.push(if attention_animation_active || row.api_error {
                attention_span(indicator, attention_color)
            } else {
                indicator
            });
            spans.push(" ".into());
        }
        let header_spans = if attention_animation_active {
            attention_header_spans(
                &row.header,
                row.waiting_animation_started_at,
                row.waiting_animation_duration,
                now,
                attention_color,
            )
        } else if row.api_error {
            vec![row.header.clone().light_red()]
        } else {
            shimmer_text(&row.header, motion_mode)
        };
        spans.extend(header_spans);
        if !spans.is_empty() {
            spans.push(" ".into());
        }
        spans.push(format!("({pretty_elapsed}").dim());
        if let Some(status) = row.model_transfer {
            let sent = fmt_bytes(status.sent_bytes);
            let received = fmt_bytes(status.received_bytes);
            spans.push(" • ".dim());
            let sent_transfer = format!("↑ {sent}");
            spans.push(match status.phase {
                ModelTransferPhase::Sending => sent_transfer.bold(),
                ModelTransferPhase::Receiving => sent_transfer.dim(),
            });
            spans.push(" ".dim());
            let received_transfer = format!("↓ {received}");
            spans.push(match status.phase {
                ModelTransferPhase::Sending => received_transfer.dim(),
                ModelTransferPhase::Receiving => received_transfer.bold(),
            });
            spans.push(")".dim());
        } else {
            spans.push(")".dim());
        }
        let mut header = Line::from(spans);
        let mut hook_overflow = None;
        if let Some(message) = &row.hook_status_message {
            if line_width(&header) + display_width(" · ") + display_width(message)
                <= usize::from(width)
            {
                header.spans.extend([" · ".dim(), message.clone().dim()]);
            } else {
                hook_overflow = Some(truncate_line_with_ellipsis_if_overflow(
                    Line::from(vec![DETAILS_PREFIX.dim(), message.clone().dim()]),
                    usize::from(width),
                ));
            }
        }
        let mut lines = Vec::new();
        lines.push(truncate_line_with_ellipsis_if_overflow(
            header,
            usize::from(width),
        ));
        lines.extend(hook_overflow);
        lines.extend(row.wrapped_details_lines(width));
        lines
    }
}

fn attention_span(mut span: Span<'static>, color: Color) -> Span<'static> {
    span.style = span.style.fg(color);
    span
}

fn attention_header_spans(
    header: &str,
    started_at: Option<Instant>,
    duration: Duration,
    now: Instant,
    color: Color,
) -> Vec<Span<'static>> {
    let chars = header.chars().collect::<Vec<_>>();
    let highlighted = started_at.map_or(0, |started_at| {
        let elapsed = now.saturating_duration_since(started_at);
        let progress = (elapsed.as_secs_f64() / duration.as_secs_f64().max(1.0)).min(1.0);
        (progress * chars.len() as f64).ceil() as usize
    });
    chars
        .into_iter()
        .enumerate()
        .map(|(index, character)| {
            let mut span = Span::from(character.to_string()).fg(color);
            if index >= highlighted {
                span = span.dim();
            }
            span
        })
        .collect()
}

impl Renderable for StatusIndicator<'_> {
    fn desired_height(&self, width: u16) -> u16 {
        self.lines(width).len() as u16
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        if self.row.animations_enabled || self.timer.display_started_at.is_some() {
            let interval_ms = if self.row.animations_enabled {
                32
            } else {
                1_000
            };
            self.row
                .frame_requester
                .schedule_frame_in(Duration::from_millis(interval_ms));
        }
        Paragraph::new(Text::from(self.lines(area.width))).render(area, buf);
    }
}

fn fmt_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    let (divisor, unit) = if bytes >= MIB {
        (MIB, "MiB")
    } else if bytes >= KIB {
        (KIB, "KiB")
    } else {
        return format!("{bytes} B");
    };
    let whole = bytes / divisor;
    let hundredths = bytes % divisor * 100 / divisor;
    format!("{whole}.{hundredths:03} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc::unbounded_channel;

    use pretty_assertions::assert_eq;

    #[test]
    fn fmt_elapsed_compact_formats_seconds_minutes_hours() {
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 0), "0s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 1), "1s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 59), "59s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 60), "1m 00s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 61), "1m 01s");
        assert_eq!(fmt_elapsed_compact(3 * 60 + 5), "3m 05s");
        assert_eq!(fmt_elapsed_compact(59 * 60 + 59), "59m 59s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 3600), "1h 00m 00s");
        assert_eq!(fmt_elapsed_compact(3600 + 60 + 1), "1h 01m 01s");
        assert_eq!(fmt_elapsed_compact(25 * 3600 + 2 * 60 + 3), "25h 02m 03s");
    }

    #[test]
    fn renders_with_working_header() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let timer = StatusTimer::default();
        let w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );

        // Render into a fixed-size test terminal and snapshot the backend.
        let mut terminal = Terminal::new(TestBackend::new(80, 2)).expect("terminal");
        terminal
            .draw(|f| w.with_timer(&timer).render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_receiving_transfer_progress() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        widget.update_model_transfer(Some(ModelTransferStatus {
            phase: ModelTransferPhase::Receiving,
            sent_bytes: 2048,
            received_bytes: 1024 * 1024,
        }));
        let mut timer = StatusTimer::default();
        timer.pause_at(timer.last_resume_at);
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                widget
                    .with_timer(&timer)
                    .render(frame.area(), frame.buffer_mut())
            })
            .expect("draw");
        insta::assert_snapshot!(terminal.backend(), @r###"
"Working (0s • ↑ 2.000 KiB ↓ 1.000 MiB)                                          "
"###);
    }

    #[test]
    fn renders_truncated() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let timer = StatusTimer::default();
        let w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );

        // Render into a fixed-size test terminal and snapshot the backend.
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("terminal");
        terminal
            .draw(|f| w.with_timer(&timer).render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_wrapped_details_panama_two_lines() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        w.update_details(
            Some("A man a plan a canal panama".to_string()),
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
        // Freeze time-dependent rendering (elapsed + spinner) to keep the snapshot stable.
        let mut timer = StatusTimer::default();
        timer.pause_at(timer.last_resume_at);

        // Prefix is 4 columns, so a width of 30 yields a content width of 26: one column
        // short of fitting the whole phrase (27 cols), forcing exactly one wrap without ellipsis.
        let mut terminal = Terminal::new(TestBackend::new(30, 3)).expect("terminal");
        terminal
            .draw(|f| w.with_timer(&timer).render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_without_spinner_when_animations_disabled() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        let mut timer = StatusTimer::default();
        timer.pause_at(timer.last_resume_at);

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|f| w.with_timer(&timer).render(f.area(), f.buffer_mut()))
            .expect("draw");
        let line = terminal.backend().buffer().content()[..80]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        assert!(line.starts_with("Working (0s)"));
    }

    #[test]
    fn renders_waiting_header_in_yellow() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        widget.update_header("Waiting".to_string());
        let mut timer = StatusTimer::default();
        timer.pause_at(timer.last_resume_at);

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                widget
                    .with_timer(&timer)
                    .render(frame.area(), frame.buffer_mut())
            })
            .expect("draw");

        insta::assert_snapshot!(terminal.backend());
        let cell = &terminal.backend().buffer()[(0, 0)];
        assert_eq!(cell.symbol(), "W");
        assert_eq!(cell.fg, ratatui::style::Color::Yellow);
    }

    #[test]
    fn renders_api_error_with_waiting_animation_in_light_red() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        let header = "Reconnecting... 1/5";
        widget.update_header(header.to_string());
        widget.reset_api_error_animation(Duration::from_secs(10));
        widget.waiting_animation_started_at = Some(Instant::now() - Duration::from_secs(5));
        let mut timer = StatusTimer::default();
        timer.pause_at(timer.last_resume_at);

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                widget
                    .with_timer(&timer)
                    .render(frame.area(), frame.buffer_mut())
            })
            .expect("draw");

        let styles = (0..header.chars().count())
            .map(|x| {
                let cell = &terminal.backend().buffer()[(x as u16, 0)];
                (cell.symbol().to_string(), cell.fg, cell.modifier)
            })
            .collect::<Vec<_>>();
        assert!(styles.iter().all(|(_, color, _)| *color == Color::LightRed));
        assert!(
            styles
                .iter()
                .all(|(_, _, modifier)| !modifier.contains(ratatui::style::Modifier::BOLD))
        );
        assert!(
            styles
                .last()
                .expect("header style")
                .2
                .contains(ratatui::style::Modifier::DIM)
        );
        insta::assert_debug_snapshot!("api_error_waiting_animation_light_red", styles);

        widget.update_header("Working".to_string());
        assert!(!widget.api_error);
        assert_eq!(widget.waiting_animation_started_at, None);
    }

    #[test]
    fn waiting_animation_expiry_restores_regular_status_rendering() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        widget.update_header("Waiting for terminal".to_string());
        widget.waiting_animation_duration = Duration::from_secs(1);
        widget.waiting_animation_started_at = Some(Instant::now() - Duration::from_secs(2));
        let mut timer = StatusTimer::default();
        timer.pause_at(timer.last_resume_at);

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                widget
                    .with_timer(&timer)
                    .render(frame.area(), frame.buffer_mut())
            })
            .expect("draw");

        insta::assert_snapshot!(terminal.backend(), @r###"
"Waiting for terminal (0s)                                                       "
"###);
        let cell = &terminal.backend().buffer()[(0, 0)];
        assert_eq!(cell.symbol(), "W");
        assert_eq!(cell.fg, ratatui::style::Color::Reset);
    }

    #[test]
    fn hook_status_reflows_without_displacing_status_or_details() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let mut w = StatusIndicatorWidget::new(
            AppEventSender::new(tx),
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        let mut timer = StatusTimer::default();
        timer.pause_at(timer.last_resume_at);
        w.update_hook_status_message(Some("checking 日本語 ｶﾞﾊﾟ policy".to_string()));
        w.update_details(
            Some("existing details".to_string()),
            StatusDetailsCapitalization::Preserve,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );

        let expected = "Working (0s) · checking 日本語 ｶﾞﾊﾟ policy";
        let fit_width = display_width(expected) as u16;
        let mut frames = Vec::new();
        for width in [fit_width, fit_width - 1, 24, fit_width] {
            let height = w.with_timer(&timer).desired_height(width);
            assert_eq!(height, if width >= fit_width { 2 } else { 3 });
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            terminal
                .draw(|f| w.with_timer(&timer).render(f.area(), f.buffer_mut()))
                .expect("draw");
            frames.push(format!("{width} columns:\n{}", terminal.backend()));
        }
        insta::assert_snapshot!(
            "hook_status_reflows_without_background_activity",
            frames.join("\n")
        );
    }

    #[test]
    fn details_overflow_adds_ellipsis() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );
        w.update_details(
            Some("abcd abcd abcd abcd".to_string()),
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );

        let lines = w.wrapped_details_lines(/*width*/ 6);
        assert_eq!(lines.len(), STATUS_DETAILS_DEFAULT_MAX_LINES);
        let last = lines.last().expect("expected last details line");
        assert!(
            last.spans[1].content.as_ref().ends_with("…"),
            "expected ellipsis in last line: {last:?}"
        );
    }

    #[test]
    fn details_args_can_disable_capitalization_and_limit_lines() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );
        w.update_details(
            Some("cargo test -p codex-core and then cargo test -p codex-tui".to_string()),
            StatusDetailsCapitalization::Preserve,
            /*max_lines*/ 1,
        );

        assert_eq!(
            w.details(),
            Some("cargo test -p codex-core and then cargo test -p codex-tui")
        );

        let lines = w.wrapped_details_lines(/*width*/ 24);
        assert_eq!(lines.len(), 1);
        let last = lines.last().expect("expected one details line");
        assert!(
            last.spans
                .last()
                .is_some_and(|span| span.content.as_ref().contains('…')),
            "expected one-line details to be ellipsized, got {last:?}"
        );
    }
}
