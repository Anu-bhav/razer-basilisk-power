// Publishes the battery state for other consumers:
//   - status.json: tiny hand-rolled JSON feed read by the Rainmeter skin
//   - battery-history.csv: rolling log of readings for future graphs
// Both live in %LOCALAPPDATA%\BasiliskPower. Writes are self-throttled so
// the polling loop can call them every tick.

use std::{path::PathBuf, time::Duration, time::Instant};

use crate::system;

/// Cadence published when no device is live. Well under the skin's stale
/// threshold, so a healthy widget never blanks itself.
const IDLE_POLL_SECONDS: u64 = 30;

/// One device's state, borrowed from the worker's device map.
#[derive(Debug, Clone, Copy)]
pub struct DeviceSnapshot<'a> {
    pub name: &'a str,
    /// `None` when the last read failed (asleep / out of range).
    pub level: Option<i32>,
    pub charging: bool,
    /// Minutes-to-full estimate while charging, `None` when unknown.
    pub eta_full_min: Option<i32>,
    /// "wired" | "wireless" | "dock" | "unknown"
    pub source: &'static str,
    /// Seconds until the next status write. The skin uses this as its read
    /// cadence, so it polls quickly while things change and slowly when idle.
    pub poll_seconds: u64,
}

/// Maps a display name to the feed's short source tag.
///
/// The dock is the transport whenever the mouse is dock-paired, but it only
/// counts as "docked" while the mouse is actually on it charging. Lift the
/// mouse off and it is running on battery: still relayed through the dock,
/// but wireless as far as the user is concerned.
pub fn source_of(name: &str, charging: bool) -> &'static str {
    if name.contains("(Wired)") {
        "wired"
    } else if name.contains("(Mouse Dock Pro)") {
        if charging { "dock" } else { "wireless" }
    } else if name.contains("(Wireless)") {
        "wireless"
    } else {
        "unknown"
    }
}

pub struct Feed {
    dir: Option<PathBuf>,
    last_status: Option<String>,
    last_history: Option<(Instant, i32, bool)>,
}

impl Feed {
    pub fn new() -> Self {
        Self {
            dir: system::data_dir(),
            last_status: None,
            last_history: None,
        }
    }

    /// Rewrites status.json when its content changed (the embedded
    /// timestamp changes per minute, so consumers never see stale data).
    pub fn write_status(&mut self, device: Option<&DeviceSnapshot>, _now: Instant) {
        let Some(dir) = &self.dir else { return };
        let updated = system::local_time_parts().0;
        let json = match device {
            Some(d) => format!(
                concat!(
                    r#"{{"level":{},"charging":{},"source":"{}","#,
                    r#""eta_full_min":{},"name":"{}","updated":"{}","poll":{}}}"#
                ),
                d.level.unwrap_or(-1),
                if d.charging { 1 } else { 0 },
                d.source,
                d.eta_full_min.unwrap_or(-1),
                d.name,
                updated,
                d.poll_seconds,
            ),
            None => format!(
                r#"{{"level":-1,"charging":0,"source":"none","eta_full_min":-1,"name":"","updated":"{}","poll":{}}}"#,
                updated,
                IDLE_POLL_SECONDS,
            ),
        };
        // Skip the write only when the content is unchanged AND the file is
        // still there. Without the existence check, a status.json that gets
        // deleted while the tray runs is never recreated: the cached string
        // still matches, so every later call returns early and the skin is
        // left reading a file that no longer exists.
        let path = dir.join("status.json");
        let unchanged = self.last_status.as_deref() == Some(json.as_str());
        let exists = path.exists();
        if unchanged && exists {
            return;
        }
        if std::fs::write(&path, json.as_bytes()).is_ok() {
            self.last_status = Some(json);
        }
    }

    /// Appends a row when the level or charging state changed, or at most
    /// every 15 minutes while unchanged. Trims the file past 1 MB.
    pub fn append_history(&mut self, device: Option<&DeviceSnapshot>, now: Instant) {
        let Some(dir) = &self.dir else { return };
        let Some(d) = device else { return };
        let Some(level) = d.level else { return };
        let due = match self.last_history {
            None => true,
            Some((t, l, c)) => {
                level != l || c != d.charging || now.duration_since(t) >= Duration::from_secs(15 * 60)
            }
        };
        if !due {
            return;
        }
        let (.., hhmmss, ymd) = system::local_time_parts();
        let row = format!(
            "{ymd},{hhmmss},{},{},{}\n",
            level,
            if d.charging { 1 } else { 0 },
            d.source
        );
        let path = dir.join("battery-history.csv");
        if !path.exists() {
            let _ = std::fs::write(&path, "date,time,level,charging,source\n");
        }
        let appended = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, row.as_bytes()))
            .is_ok();
        if appended {
            self.last_history = Some((now, level, d.charging));
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > 1_000_000 {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let lines: Vec<&str> = text
                        .lines()
                        .filter(|l| !l.starts_with("date,"))
                        .collect();
                    let skip = lines.len().saturating_sub(2000);
                    let mut out = String::from("date,time,level,charging,source\n");
                    for line in &lines[skip..] {
                        out.push_str(line);
                        out.push('\n');
                    }
                    let _ = std::fs::write(&path, out);
                }
            }
        }
    }
}
