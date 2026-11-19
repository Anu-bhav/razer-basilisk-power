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

/// Even when nothing changed, rewrite status.json at least this often so the
/// `updated_unix` freshness stamp never gets close to the skin's 90 s cutoff.
const STATUS_HEARTBEAT: Duration = Duration::from_secs(30);

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
    /// Advisory seconds until the next scheduled HID battery read (the skin
    /// itself uses its own fixed cadence). 1 means this tick just read.
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
    /// Content that does not vary between writes (everything except the
    /// timestamps), used to tell a real change from a heartbeat.
    last_status: Option<String>,
    last_status_write: Option<Instant>,
    last_history: Option<(Instant, i32, bool)>,
}

impl Feed {
    pub fn new() -> Self {
        Self {
            dir: system::data_dir(),
            last_status: None,
            last_status_write: None,
            last_history: None,
        }
    }

    /// Rewrites status.json when its data changed, or once per heartbeat when
    /// nothing did. The timestamp is stamped at write time rather than every
    /// tick, so an idle tray stops rewriting an identical feed every 5 s.
    pub fn write_status(&mut self, device: Option<&DeviceSnapshot>, now: Instant) {
        let Some(dir) = &self.dir else { return };
        let content = match device {
            Some(d) => format!(
                concat!(
                    r#""level":{},"charging":{},"source":"{}","#,
                    r#""eta_full_min":{},"name":"{}","poll":{}"#
                ),
                d.level.unwrap_or(-1),
                if d.charging { 1 } else { 0 },
                d.source,
                d.eta_full_min.unwrap_or(-1),
                d.name,
                d.poll_seconds,
            ),
            None => format!(
                r#""level":-1,"charging":0,"source":"none","eta_full_min":-1,"name":"","poll":{}"#,
                IDLE_POLL_SECONDS,
            ),
        };
        // Skip the write only when the data is unchanged, the heartbeat is not
        // due AND the file is still there. Without the existence check, a
        // status.json deleted while the tray runs is never recreated: the
        // cached content still matches, so every later call returns early.
        let path = dir.join("status.json");
        if !status_due(
            self.last_status.as_deref(),
            self.last_status_write,
            now,
            path.exists(),
            &content,
        ) {
            return;
        }

        let updated = system::local_time_parts().0;
        let updated_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let json =
            format!("{{{content},\"updated\":\"{updated}\",\"updated_unix\":{updated_unix}}}");
        let tmp = dir.join("status.json.tmp");
        let write = std::fs::write(&tmp, json.as_bytes())
            .and_then(|_| std::fs::rename(&tmp, &path));
        if write.is_ok() {
            self.last_status = Some(content);
            self.last_status_write = Some(now);
        } else {
            let _ = std::fs::remove_file(&tmp);
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

/// Decide whether status.json needs writing now.
///
/// A real change is published immediately; unchanged content is rewritten on
/// the heartbeat so `updated_unix` stays fresh. A deleted file always wins.
fn status_due(
    last: Option<&str>,
    last_write: Option<Instant>,
    now: Instant,
    exists: bool,
    content: &str,
) -> bool {
    if !exists || last != Some(content) {
        return true;
    }
    last_write.is_none_or(|t| now.saturating_duration_since(t) >= STATUS_HEARTBEAT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_status_waits_for_the_heartbeat() {
        let t0 = Instant::now();
        let content = r#""level":50"#;
        assert!(status_due(None, None, t0, true, content));
        assert!(!status_due(Some(content), Some(t0), t0, true, content));
        assert!(!status_due(
            Some(content),
            Some(t0),
            t0 + Duration::from_secs(29),
            true,
            content
        ));
        assert!(status_due(
            Some(content),
            Some(t0),
            t0 + STATUS_HEARTBEAT,
            true,
            content
        ));
    }

    #[test]
    fn changed_or_missing_status_is_written_immediately() {
        let t0 = Instant::now();
        let content = r#""level":50"#;
        assert!(status_due(
            Some(content),
            Some(t0),
            t0 + Duration::from_secs(1),
            true,
            r#""level":49"#
        ));
        assert!(status_due(
            Some(content),
            Some(t0),
            t0 + Duration::from_secs(1),
            false,
            content
        ));
    }
}
