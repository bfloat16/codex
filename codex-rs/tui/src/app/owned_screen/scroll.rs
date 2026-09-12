//! Mouse-wheel acceleration for the application-owned transcript.

use std::time::Duration;
use std::time::Instant;

use crate::tui::MouseScrollDirection;

const BASE_SCROLL_ROWS: usize = 3;
const SCROLL_ACCELERATION_HALF_LIFE_MS: usize = 150;
const SCROLL_ACCELERATION_RESET_AFTER: Duration = Duration::from_millis(250);
const SCROLL_ACCELERATION_BOOST_PER_MILLE: usize = 400;
const MAX_SCROLL_MULTIPLIER_PER_MILLE: usize = 5_000;
pub(super) const PER_MILLE: usize = 1_000;

/// Decaying scroll multiplier modeled after native desktop wheel acceleration.
#[derive(Debug)]
pub(super) struct ScrollAcceleration {
    direction: Option<MouseScrollDirection>,
    last_at: Option<Instant>,
    pub(super) multiplier_per_mille: usize,
}

impl Default for ScrollAcceleration {
    fn default() -> Self {
        Self {
            direction: None,
            last_at: None,
            multiplier_per_mille: PER_MILLE,
        }
    }
}

impl ScrollAcceleration {
    pub(super) fn rows(&mut self, direction: MouseScrollDirection) -> usize {
        self.rows_at(direction, Instant::now())
    }

    pub(super) fn rows_at(&mut self, direction: MouseScrollDirection, now: Instant) -> usize {
        let elapsed = self
            .last_at
            .and_then(|last_at| now.checked_duration_since(last_at));
        if self.direction == Some(direction)
            && elapsed.is_some_and(|elapsed| elapsed <= SCROLL_ACCELERATION_RESET_AFTER)
        {
            let elapsed_ms = elapsed
                .and_then(|elapsed| usize::try_from(elapsed.as_millis()).ok())
                .unwrap_or(usize::MAX);
            let decay_denominator = SCROLL_ACCELERATION_HALF_LIFE_MS
                .saturating_add(elapsed_ms)
                .max(1);
            // This rational decay reaches one half at the configured half-life without putting
            // floating-point work in the input hot path.
            let retained = self
                .multiplier_per_mille
                .saturating_sub(PER_MILLE)
                .saturating_mul(SCROLL_ACCELERATION_HALF_LIFE_MS)
                / decay_denominator;
            let boost = SCROLL_ACCELERATION_BOOST_PER_MILLE
                .saturating_mul(SCROLL_ACCELERATION_HALF_LIFE_MS)
                / decay_denominator;
            self.multiplier_per_mille = PER_MILLE
                .saturating_add(retained)
                .saturating_add(boost)
                .min(MAX_SCROLL_MULTIPLIER_PER_MILLE);
        } else {
            self.multiplier_per_mille = PER_MILLE;
        }
        self.direction = Some(direction);
        self.last_at = Some(now);

        BASE_SCROLL_ROWS
            .saturating_mul(self.multiplier_per_mille)
            .saturating_add(PER_MILLE / 2)
            / PER_MILLE
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }
}
