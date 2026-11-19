//! Shared device table, measurements and adaptive polling logic
//! used by both the CLI (`basilisk-power`) and the tray app (`basilisk-power-tray`).

use std::time::Duration;

use razer_battery::devices::{RazerDevice, custom_device};
use razer_battery::{BatterySession, RazerError};

pub const USAGE_PAGE: u16 = 0x0001; // generic desktop
pub const USAGE_MOUSE: u16 = 0x0002; // mouse

// ── Device table ─────────────────────────────────────────────────────────────
// The Basilisk V3 Pro enumerates under different PIDs depending on connection
// mode (cable → 0x00AA, standalone HyperSpeed dongle → 0x00AB). When paired to
// the Mouse Dock Pro, everything appears under the dock's PID (0x00A4) and the
// dock relays battery/charging queries to the mouse over RF.
pub fn candidates() -> Vec<RazerDevice> {
    vec![
        custom_device("Basilisk V3 Pro", 0x00AA, 0, USAGE_PAGE, USAGE_MOUSE, 0x1f),
        custom_device("Basilisk V3 Pro", 0x00AB, 0, USAGE_PAGE, USAGE_MOUSE, 0x1f),
        custom_device("Mouse Dock Pro", 0x00A4, 0, USAGE_PAGE, USAGE_MOUSE, 0x1f),
    ]
}

#[derive(Debug, Clone)]
pub enum Reading {
    Info { level: u8, charging: bool },
    NotPresent,
    Failed(String),
}

impl Reading {
    /// (level, charging) when the device answered.
    pub fn info(&self) -> Option<(u8, bool)> {
        match *self {
            Reading::Info { level, charging } => Some((level, charging)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub pid: u16,
    pub reading: Reading,
}

pub fn label(row: &Row) -> String {
    format!("{} [{:#06x}]", row.name, row.pid)
}

/// Query one device. Errors are folded into the Reading, never propagated.
pub fn measure(dev: &RazerDevice) -> Row {
    let reading = match BatterySession::new(dev) {
        Ok(sess) => match sess.query() {
            Ok(info) => Reading::Info {
                level: info.level,
                charging: info.charging,
            },
            Err(e) => Reading::Failed(e.to_string()),
        },
        Err(RazerError::DeviceNotFound { .. }) => Reading::NotPresent,
        Err(e) => Reading::Failed(e.to_string()),
    };
    Row {
        name: dev.name.to_string(),
        pid: dev.pid,
        reading,
    }
}

/// Scan all candidate devices.
pub fn measure_all() -> Vec<Row> {
    candidates().iter().map(measure).collect()
}

// ── Adaptive polling ─────────────────────────────────────────────────────────
// Rare when the battery is healthy, frequent when low. While charging, poll
// occasionally to catch "full" / unplug events.

pub fn adaptive_interval(level: u8, charging: bool) -> Duration {
    let secs: u64 = if charging {
        5 * 60
    } else {
        match level {
            0..=2 => 2 * 60,
            3..=6 => 5 * 60,
            7..=14 => 10 * 60,
            15..=29 => 30 * 60,
            30..=49 => 60 * 60,
            50..=79 => 2 * 60 * 60,
            _ => 4 * 60 * 60, // 80..=100
        }
    };
    Duration::from_secs(secs)
}

/// How long to sleep after a scan round. Fixed interval wins when given;
/// otherwise the shortest adaptive need across devices; 1-2 min fallback
/// when nothing is present or something is failing.
pub fn next_wait(rows: &[Row], fixed: Option<Duration>) -> Duration {
    let mut next = fixed;
    for row in rows {
        if let Some((level, charging)) = row.reading.info() {
            let adaptive = adaptive_interval(level, charging);
            next = Some(match next {
                Some(n) if n <= adaptive => n,
                _ => adaptive,
            });
        }
    }
    next.unwrap_or(Duration::from_secs(
        if rows.iter().any(|r| matches!(r.reading, Reading::Failed(_))) {
            120
        } else {
            60
        },
    ))
}

/// Pick the row to display: a charging device wins, else the first device
/// that answered.
// Shared with the tray bin (via #[path]), which doesn't use this — silence the
// dead-code warning for that build only.
#[allow(dead_code)]
pub fn display_row(rows: &[Row]) -> Option<&Row> {
    let infos: Vec<&Row> = rows.iter().filter(|r| r.reading.info().is_some()).collect();
    if infos.is_empty() {
        return None;
    }
    let charging = infos
        .iter()
        .find(|r| matches!(r.reading, Reading::Info { charging: true, .. }));
    Some(charging.copied().unwrap_or(infos[0]))
}
