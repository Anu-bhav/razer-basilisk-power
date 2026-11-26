//! Pure read-scheduling logic for the tray worker.
//!
//! The Mouse Dock Pro's RF relay is a **single shared resource**: a
//! dock-paired mouse sends its input over the same channel the tray reads the
//! battery through. Every read therefore competes with the cursor, so the
//! worker must
//!
//! 1. never read a device more often than [`MIN_READ_INTERVAL`], and
//! 2. always pick the **cheapest** read that can still teach us something.
//!
//! Historically these rules lived as five independent timers (adaptive poll,
//! charge probe, link probe, wake retry, first read) OR-ed together in the
//! worker loop. That worked but was hard to reason about and easy to get
//! wrong. This module is the whole decision, kept pure so it can be tested
//! without hardware.
//!
//! The three read kinds, cheapest first:
//!
//! | Kind | Cost | What it teaches |
//! |------|------|-----------------|
//! | [`ReadKind::Link`] | ~120 ms, **no RF traffic** | Is the dock's RF link to the mouse up? |
//! | [`ReadKind::ChargeProbe`] | one RF command | Is the mouse charging? |
//! | [`ReadKind::Full`] | two RF commands | Battery level + charging + ETA |
//!
//! The scheduler never executes a read; it only decides *which* one (if any)
//! is due. The worker turns a [`Decision`] into the matching manager call.

use std::time::{Duration, Instant};

/// Minimum gap between reads of the same device, whatever asked (poll, hover,
/// manual, charge probe, link probe). A dock-paired mouse shares one RF relay
/// with its input, so coalescing every trigger to one read per device per this
/// window is what keeps the cursor from hitching.
pub const MIN_READ_INTERVAL: Duration = Duration::from_secs(5);

/// Link-heartbeat cadence while the user is active.
pub const LINK_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Link-heartbeat cadence while the user is idle. Returning input forces a read
/// immediately, so this only bounds background wake-up latency.
pub const LINK_PROBE_IDLE_INTERVAL: Duration = Duration::from_secs(15);

/// Charging-only probe cadence while docked.
pub const CHARGE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// The cheapest read that would teach us something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    /// Dock link heartbeat only: no RF traffic, ~120 ms.
    Link,
    /// Charging-flag-only probe: one RF command.
    ChargeProbe,
    /// Full battery + charging read: two RF commands (heartbeat first if the
    /// link state is unknown or the device is stale).
    Full,
}

/// Everything the scheduler needs to know about one device at `now`.
///
/// This is a snapshot: the worker builds one per device per tick from
/// [`crate::tray::MemoryDevice`] plus the global timers.
#[derive(Debug, Clone, Copy)]
pub struct DeviceView {
    /// Last known level; `None` until the first successful read.
    pub battery_level: Option<i32>,
    /// The last read failed (asleep / out of range).
    pub stale: bool,
    /// This device is the Mouse Dock Pro (has a link heartbeat).
    pub is_dock: bool,
    /// When the link heartbeat was last checked.
    pub link_checked: Option<Instant>,
    /// When the device was last read over HID.
    pub last_read: Option<Instant>,
    /// A charging-only probe could still teach us something (mirrors
    /// `MemoryDevice::wants_charge_probe`).
    pub wants_charge_probe: bool,
    /// The global adaptive poll says a full read is due this tick.
    pub poll_due: bool,
    /// The global charge-probe timer says a probe is due this tick.
    pub charge_probe_due: bool,
    /// This device just appeared (or was just re-enumerated).
    pub just_connected: bool,
    /// A stale device's wake retry is due (the user returned from idle).
    pub wake_retry_due: bool,
    /// The user is currently idle.
    pub idle: bool,
}

/// The scheduler's decision for one device: the cheapest read that is due, or
/// `None` if nothing needs doing this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub kind: ReadKind,
    /// Short reason, for the log.
    pub reason: &'static str,
}

impl DeviceView {
    /// True when the relay floor has elapsed since the last read.
    pub fn read_due(&self, now: Instant) -> bool {
        self.last_read
            .is_none_or(|t| now.duration_since(t) >= MIN_READ_INTERVAL)
    }

    /// True when the link heartbeat is due (stale dock only).
    pub fn link_probe_due(&self, now: Instant) -> bool {
        if !self.stale || !self.is_dock {
            return false;
        }
        let interval = if self.idle {
            LINK_PROBE_IDLE_INTERVAL
        } else {
            LINK_PROBE_INTERVAL
        };
        self.link_checked
            .is_none_or(|t| now.duration_since(t) >= interval)
    }

    /// The cheapest read that is due now, or `None`.
    ///
    /// Priority, cheapest and most urgent first:
    ///
    /// 1. **First read** — a new or never-read device must get a full read.
    /// 2. **Link probe** — a stale dock: check the link (no RF) before any RF
    ///    read, so a sleeping mouse costs ~120 ms instead of seconds.
    /// 3. **Wake retry** — a stale device whose backoff says "try again now".
    /// 4. **Poll** — the adaptive full-read cadence.
    /// 5. **Charge probe** — the cheap one-command charging check.
    pub fn next_read(&self, now: Instant) -> Option<Decision> {
        // The relay floor wins over everything: never read early.
        if !self.read_due(now) {
            return None;
        }

        // 1. A new or never-read device must get a full read immediately.
        if self.just_connected || (self.battery_level.is_none() && !self.stale) {
            return Some(Decision {
                kind: ReadKind::Full,
                reason: "first read",
            });
        }

        // 2. A stale dock: check the link first (cheap, no RF).
        if self.link_probe_due(now) {
            return Some(Decision {
                kind: ReadKind::Link,
                reason: "link probe",
            });
        }

        // 3. A stale device whose wake retry is due.
        if self.stale && self.wake_retry_due {
            return Some(Decision {
                kind: ReadKind::Full,
                reason: "wake retry",
            });
        }

        // 4. The adaptive poll is due.
        if self.poll_due {
            return Some(Decision {
                kind: ReadKind::Full,
                reason: "poll",
            });
        }

        // 5. The cheap charging-only probe.
        if self.charge_probe_due && !self.stale && self.wants_charge_probe {
            return Some(Decision {
                kind: ReadKind::ChargeProbe,
                reason: "charge probe",
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// A healthy, docked, recently-read device: nothing due.
    fn healthy(now: Instant) -> DeviceView {
        DeviceView {
            battery_level: Some(80),
            stale: false,
            is_dock: true,
            link_checked: Some(now),
            last_read: Some(now),
            wants_charge_probe: false,
            poll_due: false,
            charge_probe_due: false,
            just_connected: false,
            wake_retry_due: false,
            idle: false,
        }
    }

    #[test]
    fn relay_floor_blocks_everything() {
        let now = Instant::now();
        let mut v = healthy(now);
        v.poll_due = true;
        v.just_connected = true;
        v.charge_probe_due = true;
        v.wants_charge_probe = true;
        // Read 1 s ago: the 5 s floor must suppress every trigger.
        v.last_read = Some(now - secs(1));
        assert_eq!(v.next_read(now), None);
    }

    #[test]
    fn first_read_is_full_and_immediate() {
        let now = Instant::now();
        let v = DeviceView {
            battery_level: None,
            stale: false,
            last_read: None,
            ..healthy(now)
        };
        assert_eq!(
            v.next_read(now),
            Some(Decision {
                kind: ReadKind::Full,
                reason: "first read"
            })
        );
    }

    #[test]
    fn just_connected_forces_a_full_read() {
        let now = Instant::now();
        let v = DeviceView {
            just_connected: true,
            last_read: None,
            ..healthy(now)
        };
        assert_eq!(v.next_read(now).map(|d| d.kind), Some(ReadKind::Full));
    }

    #[test]
    fn stale_dock_probes_the_link_before_any_rf_read() {
        let now = Instant::now();
        let v = DeviceView {
            stale: true,
            link_checked: Some(now - secs(10)),
            last_read: Some(now - secs(10)),
            ..healthy(now)
        };
        assert_eq!(
            v.next_read(now),
            Some(Decision {
                kind: ReadKind::Link,
                reason: "link probe"
            })
        );
    }

    #[test]
    fn active_link_probe_uses_the_short_interval() {
        let now = Instant::now();
        let v = DeviceView {
            stale: true,
            link_checked: Some(now - secs(4)),
            last_read: Some(now - secs(10)),
            idle: false,
            ..healthy(now)
        };
        // 4 s < 5 s active interval: not yet.
        assert_eq!(v.next_read(now), None);
    }

    #[test]
    fn idle_link_probe_waits_longer() {
        let now = Instant::now();
        let v = DeviceView {
            stale: true,
            link_checked: Some(now - secs(10)),
            last_read: Some(now - secs(10)),
            idle: true,
            ..healthy(now)
        };
        // 10 s < 15 s idle interval: not yet.
        assert_eq!(v.next_read(now), None);
    }

    #[test]
    fn stale_device_with_due_wake_retry_gets_a_full_read() {
        let now = Instant::now();
        let v = DeviceView {
            stale: true,
            link_checked: Some(now),
            last_read: Some(now - secs(10)),
            wake_retry_due: true,
            ..healthy(now)
        };
        assert_eq!(
            v.next_read(now),
            Some(Decision {
                kind: ReadKind::Full,
                reason: "wake retry"
            })
        );
    }

    #[test]
    fn poll_due_reads_full_when_healthy() {
        let now = Instant::now();
        let v = DeviceView {
            poll_due: true,
            last_read: Some(now - secs(10)),
            ..healthy(now)
        };
        assert_eq!(
            v.next_read(now),
            Some(Decision {
                kind: ReadKind::Full,
                reason: "poll"
            })
        );
    }

    #[test]
    fn charge_probe_is_the_cheap_read_when_docked() {
        let now = Instant::now();
        let v = DeviceView {
            wants_charge_probe: true,
            charge_probe_due: true,
            last_read: Some(now - secs(10)),
            ..healthy(now)
        };
        assert_eq!(
            v.next_read(now),
            Some(Decision {
                kind: ReadKind::ChargeProbe,
                reason: "charge probe"
            })
        );
    }

    #[test]
    fn charge_probe_is_skipped_while_stale() {
        let now = Instant::now();
        let v = DeviceView {
            stale: true,
            wants_charge_probe: true,
            charge_probe_due: true,
            // Link freshly checked, so no link probe; no wake retry.
            link_checked: Some(now),
            last_read: Some(now - secs(10)),
            ..healthy(now)
        };
        assert_eq!(v.next_read(now), None);
    }

    #[test]
    fn charge_probe_is_skipped_when_not_wanted() {
        let now = Instant::now();
        let v = DeviceView {
            wants_charge_probe: false,
            charge_probe_due: true,
            last_read: Some(now - secs(10)),
            ..healthy(now)
        };
        assert_eq!(v.next_read(now), None);
    }

    #[test]
    fn link_probe_wins_over_poll_for_a_stale_dock() {
        let now = Instant::now();
        let v = DeviceView {
            stale: true,
            link_checked: Some(now - secs(10)),
            last_read: Some(now - secs(10)),
            poll_due: true,
            ..healthy(now)
        };
        // The cheap link check must come before the expensive poll.
        assert_eq!(v.next_read(now).map(|d| d.kind), Some(ReadKind::Link));
    }

    #[test]
    fn non_dock_never_link_probes() {
        let now = Instant::now();
        let v = DeviceView {
            is_dock: false,
            stale: true,
            link_checked: Some(now - secs(100)),
            last_read: Some(now - secs(10)),
            ..healthy(now)
        };
        // Not a dock: no link probe, and no other trigger -> None.
        assert_eq!(v.next_read(now), None);
    }
}
