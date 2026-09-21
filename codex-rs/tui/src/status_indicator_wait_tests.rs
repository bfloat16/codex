use super::*;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn terminal_wait_counter_and_animation_share_two_minute_deadline() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut widget = StatusIndicatorWidget::new(
        AppEventSender::new(tx),
        crate::tui::FrameRequester::test_dummy(),
        /*animations_enabled*/ false,
    );
    widget.update_header("Waiting for terminal".into());
    widget.reset_waiting_animation(Duration::from_secs(/*secs*/ 120));
    widget.waiting_animation_started_at = Some(Instant::now() - Duration::from_secs(/*secs*/ 60));
    let timer = StatusTimer::default();
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 80, /*height*/ 1)).expect("terminal");
    terminal
        .draw(|frame| {
            widget
                .with_timer(&timer)
                .render(frame.area(), frame.buffer_mut())
        })
        .expect("draw");
    assert_eq!(terminal.backend().buffer()[(0, 0)].fg, Color::Yellow);
    insta::assert_snapshot!(terminal.backend());
}
