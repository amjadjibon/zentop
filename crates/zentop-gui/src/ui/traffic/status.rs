//! Bottom-bar state: throughput over the last second and Auto Select.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(1);

/// Byte-counter samples from the capture, for ↑/↓ rates.
#[derive(Default)]
pub(super) struct Throughput {
    samples: VecDeque<(Instant, u64, u64)>,
}

impl Throughput {
    pub(super) fn record(&mut self, now: Instant, sent: u64, received: u64) {
        if self
            .samples
            .back()
            .is_some_and(|&(_, s, r)| sent < s || received < r)
        {
            // A new session restarted the counters.
            self.samples.clear();
        }
        self.samples.push_back((now, sent, received));
        // Keep one sample at or before the window's start as the baseline.
        while self
            .samples
            .get(1)
            .is_some_and(|&(time, _, _)| now.duration_since(time) >= WINDOW)
        {
            self.samples.pop_front();
        }
    }

    pub(super) fn clear(&mut self) {
        self.samples.clear();
    }

    /// Bytes per second sent and received over about the last second.
    pub(super) fn rates(&self) -> (u64, u64) {
        let (Some(&(start, s0, r0)), Some(&(end, s1, r1))) =
            (self.samples.front(), self.samples.back())
        else {
            return (0, 0);
        };
        let elapsed = end.duration_since(start).max(WINDOW).as_secs_f64();
        (
            ((s1 - s0) as f64 / elapsed) as u64,
            ((r1 - r0) as f64 / elapsed) as u64,
        )
    }
}

/// Keeps the newest matching flow selected while enabled. A manual choice
/// stands until a newer flow arrives.
#[derive(Default)]
pub(super) struct AutoSelect {
    pub(super) enabled: bool,
    newest: Option<u64>,
}

impl AutoSelect {
    pub(super) fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.newest = None;
    }

    /// Returns the flow to select when a newer one than last time matches.
    pub(super) fn next(&mut self, newest: Option<u64>) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        let newest = newest?;
        if self.newest.is_some_and(|seen| seen >= newest) {
            return None;
        }
        self.newest = Some(newest);
        Some(newest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_cover_the_last_second_and_reset_with_the_session() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut rate = Throughput::default();
        assert_eq!(rate.rates(), (0, 0));
        rate.record(at(0), 0, 0);
        rate.record(at(500), 1_000, 4_000);
        // Less than a second of samples still reports per second.
        assert_eq!(rate.rates(), (1_000, 4_000));
        rate.record(at(1_000), 2_000, 4_000);
        rate.record(at(1_600), 2_000, 4_000);
        // The baseline is the last sample at or before the window's start.
        assert_eq!(rate.rates(), (909, 0));
        rate.record(at(3_000), 2_000, 4_000);
        assert_eq!(rate.rates(), (0, 0));
        rate.record(at(3_100), 10, 10);
        assert_eq!(rate.rates(), (0, 0));
    }

    #[test]
    fn auto_select_follows_only_newer_flows() {
        let mut auto = AutoSelect::default();
        assert_eq!(auto.next(Some(3)), None);
        auto.toggle();
        assert_eq!(auto.next(None), None);
        assert_eq!(auto.next(Some(3)), Some(3));
        // Nothing newer: a manual selection stands.
        assert_eq!(auto.next(Some(3)), None);
        assert_eq!(auto.next(Some(2)), None);
        assert_eq!(auto.next(Some(7)), Some(7));
        auto.toggle();
        assert_eq!(auto.next(Some(9)), None);
        auto.toggle();
        assert_eq!(auto.next(Some(7)), Some(7));
    }
}
