//! Battery-history insights for `--history`.
//!
//! `battery-history.csv` is written by the tray (date,time,level,charging,
//! source). This module parses it, filters to a window, and produces a small
//! summary. It is deliberately independent of HID and of the filesystem so it
//! can be unit-tested with synthetic rows.

use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime};

/// One usable history row.
#[derive(Debug, Clone)]
pub struct Sample {
    pub when: NaiveDateTime,
    pub level: i32,
    pub charging: bool,
}

/// A parsed time window: `None` means "all recorded time".
#[derive(Debug, Clone, Copy)]
pub struct Window(Option<Duration>);

impl Window {
    /// Accepts `7d`, `24h`, or `all` (case-insensitive).
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim().to_ascii_lowercase();
        if text == "all" {
            return Ok(Self(None));
        }
        if let Some(days) = text.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
            if days > 0 {
                return Ok(Self(Some(Duration::days(days))));
            }
        }
        if let Some(hours) = text.strip_suffix('h').and_then(|n| n.parse::<i64>().ok()) {
            if hours > 0 {
                return Ok(Self(Some(Duration::hours(hours))));
            }
        }
        Err("--history expects a window such as 7d, 24h or all".to_string())
    }

    pub fn label(&self) -> String {
        match self.0 {
            Some(d) if d.num_days() > 0 && d.num_hours() % 24 == 0 => {
                format!("last {}d", d.num_days())
            }
            Some(d) => format!("last {}h", d.num_hours()),
            None => "all recorded time".to_string(),
        }
    }

    fn cutoff(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        self.0.map(|d| now - d)
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub samples: usize,
    pub first: Option<NaiveDateTime>,
    pub last: Option<NaiveDateTime>,
    pub latest: Option<i32>,
    pub min_level: Option<i32>,
    pub max_level: Option<i32>,
    pub avg_level: f64,
    pub charging_samples: usize,
    pub charge_sessions: usize,
    /// Percentage points per hour while off the dock.
    pub drain_per_hour: Option<f64>,
    /// Hours of discharge observations behind `drain_per_hour`.
    pub drain_hours: f64,
}

impl Summary {
    pub fn charging_fraction(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.charging_samples as f64 / self.samples as f64
        }
    }
}

fn parse_sample(line: &str) -> Option<Sample> {
    let mut fields = line.split(',');
    let date = fields.next()?.trim();
    let time = fields.next()?.trim();
    let level = fields.next()?.trim().parse::<i32>().ok()?;
    let charging = match fields.next()?.trim() {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    // The source column is informational here; tolerate it missing.
    let _source = fields.next();

    if level < 0 || level > 100 {
        return None;
    }
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let time = NaiveTime::parse_from_str(time, "%H:%M:%S").ok()?;
    Some(Sample {
        when: NaiveDateTime::new(date, time),
        level,
        charging,
    })
}

/// Parse every usable row; malformed lines and the header are skipped.
pub fn parse(text: &str) -> Vec<Sample> {
    text.lines().filter_map(parse_sample).collect()
}

/// Summarise samples inside the window ending at `now`.
pub fn summarize(samples: &[Sample], window: Window, now: NaiveDateTime) -> Summary {
    let cutoff = window.cutoff(now);
    let rows: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.when <= now && cutoff.is_none_or(|c| s.when >= c))
        .collect();

    let mut summary = Summary::default();
    if rows.is_empty() {
        return summary;
    }

    summary.samples = rows.len();
    summary.first = Some(rows[0].when);
    summary.last = Some(rows[rows.len() - 1].when);
    summary.latest = Some(rows[rows.len() - 1].level);
    summary.min_level = rows.iter().map(|s| s.level).min();
    summary.max_level = rows.iter().map(|s| s.level).max();
    summary.avg_level = rows.iter().map(|s| s.level as f64).sum::<f64>() / rows.len() as f64;
    summary.charging_samples = rows.iter().filter(|s| s.charging).count();

    // A charge session is a run of charging samples. Short dropouts (the dock
    // relay sometimes reports charging for a few seconds at a time) and single
    // missed heartbeats should not each count as a new session, so runs whose
    // samples are less than 20 minutes apart are merged.
    let mut previous_charge: Option<NaiveDateTime> = None;
    for sample in rows.iter().filter(|s| s.charging) {
        let starts_new = previous_charge
            .is_none_or(|previous| (sample.when - previous) > Duration::minutes(20));
        if starts_new {
            summary.charge_sessions += 1;
        }
        previous_charge = Some(sample.when);
    }

    // Drain: only count downward steps between consecutive off-dock samples.
    // Gaps over four hours are skipped -- they usually mean the machine was
    // asleep, and folding them in would understate the real drain rate.
    let max_gap = Duration::hours(4).num_seconds();
    let mut drop = 0i64;
    let mut seconds = 0i64;
    for pair in rows.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a.charging || b.charging {
            continue;
        }
        let elapsed = (b.when - a.when).num_seconds();
        if elapsed <= 0 || elapsed > max_gap {
            continue;
        }
        if let Some(delta) = a.level.checked_sub(b.level) {
            if delta > 0 {
                drop += i64::from(delta);
                seconds += elapsed;
            }
        }
    }
    if seconds > 0 {
        let hours = seconds as f64 / 3600.0;
        summary.drain_hours = hours;
        summary.drain_per_hour = Some(drop as f64 / hours);
    }

    summary
}

/// Format the human-readable report printed by `--history`.
pub fn render(summary: &Summary, window: Window) -> String {
    let mut out = format!("Battery history — {}\n", window.label());
    if summary.samples == 0 {
        out.push_str("  no readings in this window yet (is the tray running?)\n");
        return out;
    }

    if let (Some(first), Some(last)) = (summary.first, summary.last) {
        out.push_str(&format!(
            "  window: {} → {} ({} samples)\n",
            first.format("%Y-%m-%d %H:%M"),
            last.format("%Y-%m-%d %H:%M"),
            summary.samples
        ));
    }

    let latest = summary.latest.unwrap_or(-1);
    let min = summary.min_level.unwrap_or(-1);
    let max = summary.max_level.unwrap_or(-1);
    out.push_str(&format!(
        "  level: latest {latest}%, min {min}%, max {max}%, average {:.1}%\n",
        summary.avg_level
    ));

    out.push_str(&format!(
        "  charging: {} of {} samples ({:.0}%), {} session{}\n",
        summary.charging_samples,
        summary.samples,
        summary.charging_fraction() * 100.0,
        summary.charge_sessions,
        if summary.charge_sessions == 1 { "" } else { "s" }
    ));

    match summary.drain_per_hour {
        Some(rate) => out.push_str(&format!(
            "  drain: {:.1}%/h off the dock ({:.1} h observed, {:.1} h per 100%)\n",
            rate,
            summary.drain_hours,
            100.0 / rate
        )),
        None => out.push_str("  drain: not enough off-dock data yet\n"),
    }

    out
}

/// Default history path (`%LOCALAPPDATA%\BasiliskPower\battery-history.csv`).
pub fn default_path() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|base| std::path::PathBuf::from(base).join("BasiliskPower").join("battery-history.csv"))
}

/// Read and summarise a history file. Used by the CLI.
pub fn load(path: &std::path::Path, window: Window) -> std::io::Result<(Summary, String)> {
    let text = std::fs::read_to_string(path)?;
    let samples = parse(&text);
    let summary = summarize(&samples, window, Local::now().naive_local());
    let rendered = render(&summary, window);
    Ok((summary, rendered))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDateTime::new(
            NaiveDate::from_ymd_opt(y, m, d).unwrap(),
            NaiveTime::from_hms_opt(h, min, 0).unwrap(),
        )
    }

    const CSV: &str = "\
date,time,level,charging,source
2026-11-18,08:00:00,80,0,wireless
2026-11-18,09:00:00,76,0,wireless
2026-11-18,10:00:00,72,0,wireless
2026-11-18,11:00:00,80,0,wireless
2026-11-18,11:30:00,80,1,dock
2026-11-18,11:45:00,81,1,dock
2026-11-18,12:00:00,90,1,dock
2026-11-18,12:30:00,100,0,dock
2026-11-19,08:00:00,98,0,wireless
not a row
2026-11-19,09:00:00,-5,0,wireless
";

    #[test]
    fn parses_window_forms() {
        assert!(Window::parse("7d").is_ok());
        assert!(Window::parse("24H").is_ok());
        assert!(Window::parse("all").is_ok());
        assert!(Window::parse("7").is_err());
        assert!(Window::parse("0d").is_err());
        assert!(Window::parse("week").is_err());
    }

    #[test]
    fn skips_header_and_bad_rows() {
        let samples = parse(CSV);
        assert_eq!(samples.len(), 9);
        assert_eq!(samples[0].level, 80);
        assert_eq!(samples[6].level, 90);
    }

    #[test]
    fn summarises_levels_and_sessions() {
        let now = at(2026, 11, 19, 12, 0);
        let summary = summarize(&parse(CSV), Window::parse("7d").unwrap(), now);
        assert_eq!(summary.samples, 9);
        assert_eq!(summary.latest, Some(98));
        assert_eq!(summary.min_level, Some(72));
        assert_eq!(summary.max_level, Some(100));
        assert_eq!(summary.charging_samples, 3);
        assert_eq!(summary.charge_sessions, 1);
        assert!((summary.avg_level - 84.111).abs() < 0.01);
    }

    #[test]
    fn computes_drain_and_ignores_charging_and_gaps() {
        let now = at(2026, 11, 19, 12, 0);
        let summary = summarize(&parse(CSV), Window::parse("7d").unwrap(), now);
        // 80 -> 76 -> 72 over two hours = 4%/h. The 11:00 -> 11:30 rise is
        // ignored, as are charging rows and the 19.5 h overnight gap.
        let rate = summary.drain_per_hour.expect("drain");
        assert!((rate - 4.0).abs() < 0.001, "rate was {rate}");
        assert!((summary.drain_hours - 2.0).abs() < 0.001);
    }

    #[test]
    fn window_filters_old_rows() {
        let now = at(2026, 11, 19, 12, 0);
        let summary = summarize(&parse(CSV), Window::parse("24h").unwrap(), now);
        assert_eq!(summary.samples, 3);
        assert_eq!(summary.min_level, Some(90));
        assert_eq!(summary.charge_sessions, 1);
    }

    #[test]
    fn empty_window_renders_without_panicking() {
        let now = at(2026, 11, 19, 12, 0);
        let summary = summarize(&[], Window::parse("7d").unwrap(), now);
        let text = render(&summary, Window::parse("7d").unwrap());
        assert!(text.contains("no readings"));
    }
}
