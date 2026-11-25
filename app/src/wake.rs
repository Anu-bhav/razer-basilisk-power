// Retry schedule for a mouse that stopped answering (asleep, out of range,
// or not yet paired after boot/resume). Pure logic, like reminder.rs.
//
// A sleeping wireless mouse wakes when it is moved, so a retry is only worth
// it after new user input. Failed retries back off (5 s, 10 s, 20 s, ... up
// to RETRY_GAP_MAX) so a mouse that is switched off isn't hammered with HID
// requests while the user keeps typing. Coming back after a long idle resets
// the backoff: that is exactly when the mouse is most likely waking up.

use std::time::{Duration, Instant};

const RETRY_GAP_MIN: Duration = Duration::from_secs(5);
const RETRY_GAP_MAX: Duration = Duration::from_secs(120);
/// Input after at least this much idle time counts as "the user is back".
const RETURN_IDLE: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub struct WakeRetry {
    failures: u32,
    /// Consecutive failures since the last success. Unlike `failures` (the
    /// backoff schedule) this is not cleared by user-input resets, so a
    /// wedged handle is detected as a sustained failure streak rather than
    /// waiting for the backoff clock to run out mid-usage.
    consecutive_failures: u32,
    last_attempt: Option<Instant>,
}

impl WakeRetry {
    /// True when a stale device should be read again now.
    pub fn due(&self, now: Instant, last_input: Option<Instant>) -> bool {
        let Some(last) = self.last_attempt else {
            return true;
        };
        now.duration_since(last) >= self.gap() && last_input.is_some_and(|t| t > last)
    }

    pub fn record(&mut self, now: Instant, success: bool) {
        if success {
            *self = Self::default();
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            self.failures = self.failures.saturating_add(1);
            self.last_attempt = Some(now);
        }
    }

    /// Forget the backoff but keep the attempt time, so a retry still waits
    /// for fresh input. The consecutive-failure streak is deliberately left
    /// alone: a user-input reset means "the user is back", not "the handle is
    /// healthy", so the escalation counter keeps counting.
    pub fn reset_backoff(&mut self) {
        self.failures = 0;
    }

    /// Clear the failure streak without touching the attempt time.
    ///
    /// Used when a read was skipped for a reason that is not an RF failure,
    /// such as the dock reporting that its link is down. The dock answered,
    /// so the handle is healthy; a later real failure must start a fresh
    /// streak instead of inheriting old link-down probes.
    pub fn clear_failures(&mut self) {
        self.failures = 0;
        self.consecutive_failures = 0;
    }

    /// Consecutive failures since the last success; 0 while healthy.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// True once the failure backoff has grown to its maximum (5 s → 10 s →
    /// 20 s → 40 s → 80 s → 120 s). Callers use this to decide that a device
    /// which keeps failing through its backoff window may have a wedged HID
    /// handle and deserves a full reset rather than further retries.
    pub fn is_backoff_exhausted(&self) -> bool {
        self.gap() >= RETRY_GAP_MAX
    }

    fn gap(&self) -> Duration {
        let shift = self.failures.saturating_sub(1).min(16);
        (RETRY_GAP_MIN * (1 << shift)).min(RETRY_GAP_MAX)
    }
}

/// Turns periodic idle-time samples into input events.
#[derive(Debug, Default)]
pub struct Activity {
    last_idle: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputSample {
    /// When the last keyboard/mouse input happened.
    pub last_input: Option<Instant>,
    /// New input arrived after a long idle period.
    pub returned: bool,
}

impl Activity {
    pub fn sample(&mut self, now: Instant, idle: Option<Duration>) -> InputSample {
        let returned = match (self.last_idle, idle) {
            (Some(prev), Some(cur)) => prev >= RETURN_IDLE && cur < prev,
            _ => false,
        };
        if idle.is_some() {
            self.last_idle = idle;
        }
        InputSample {
            last_input: idle.and_then(|idle| now.checked_sub(idle)),
            returned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn backoff_is_not_exhausted_before_any_failure() {
        let t = Instant::now();
        let r = WakeRetry::default();
        assert!(!r.is_backoff_exhausted());
    }

    #[test]
    fn consecutive_failures_accumulate_and_clear_on_success() {
        let t = Instant::now();
        let mut r = WakeRetry::default();
        assert_eq!(r.consecutive_failures(), 0);
        r.record(t, false);
        r.record(t, false);
        assert_eq!(r.consecutive_failures(), 2);
        r.record(t, true);
        assert_eq!(r.consecutive_failures(), 0);
    }

    #[test]
    fn user_input_backoff_reset_keeps_the_failure_streak() {
        let t = Instant::now();
        let mut r = WakeRetry::default();
        r.record(t, false);
        r.record(t, false);
        assert_eq!(r.consecutive_failures(), 2);
        // A user-input reset clears the backoff clock but not the streak.
        r.reset_backoff();
        assert_eq!(r.consecutive_failures(), 2);
    }

    #[test]
    fn clear_failures_forgets_a_link_down_streak() {
        let t = Instant::now();
        let mut r = WakeRetry::default();
        for _ in 0..6 {
            r.record(t, false);
        }
        assert!(r.is_backoff_exhausted());
        r.clear_failures();
        assert_eq!(r.consecutive_failures(), 0);
        assert!(!r.is_backoff_exhausted());
        // The last attempt is kept, so a retry still waits for fresh input.
        assert!(!r.due(t + secs(5), Some(t - secs(1))));
    }

    #[test]
    fn backoff_becomes_exhausted_only_after_repeated_failures() {
        let t = Instant::now();
        let mut r = WakeRetry::default();
        assert!(!r.is_backoff_exhausted());
        r.record(t, false);
        assert!(!r.is_backoff_exhausted());
        r.record(t, false);
        r.record(t, false);
        r.record(t, false);
        r.record(t, false);
        assert!(!r.is_backoff_exhausted());
        // Sixth failure: the backoff reaches its 120 s cap and is exhausted.
        r.record(t, false);
        assert!(r.is_backoff_exhausted());
        r.reset_backoff();
        assert!(!r.is_backoff_exhausted());
    }

    #[test]
    fn retries_only_after_input() {
        let t = Instant::now();
        let mut r = WakeRetry::default();
        r.record(t, false);
        assert!(!r.due(t + secs(30), Some(t - secs(1))));
        assert!(r.due(t + secs(30), Some(t + secs(29))));
    }

    #[test]
    fn backs_off_and_caps() {
        let t = Instant::now();
        let mut r = WakeRetry::default();
        let mut gaps = Vec::new();
        for _ in 0..8 {
            r.record(t, false);
            gaps.push(r.gap().as_secs());
        }
        assert_eq!(gaps, vec![5, 10, 20, 40, 80, 120, 120, 120]);
        assert!(!r.due(t + secs(119), Some(t + secs(100))));
        assert!(r.due(t + secs(120), Some(t + secs(100))));
    }

    #[test]
    fn success_and_return_reset() {
        let t = Instant::now();
        let mut r = WakeRetry::default();
        for _ in 0..6 {
            r.record(t, false);
        }
        r.reset_backoff();
        assert!(r.due(t + secs(5), Some(t + secs(4))));
        r.record(t, true);
        assert!(r.due(t, None));
    }

    #[test]
    fn detects_return_from_idle() {
        let t = Instant::now();
        let mut a = Activity::default();
        assert!(!a.sample(t, Some(secs(10))).returned);
        assert!(!a.sample(t, Some(secs(3))).returned);
        assert!(!a.sample(t, Some(secs(90))).returned);
        let s = a.sample(t + secs(5), Some(secs(1)));
        assert!(s.returned);
        assert_eq!(s.last_input, Some(t + secs(4)));
    }
}
