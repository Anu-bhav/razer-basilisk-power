//! Support bundle: one text file with the state needed to diagnose a problem
//! without asking the user to hunt through folders.
//!
//! Deliberately excludes anything unrelated to Basilisk Power. The Rainmeter
//! log, for example, is filtered to lines that mention this skin; the whole
//! file also logs other skins the user may have installed. Paths are included
//! because they are the first thing support needs, so remind users to skim the
//! file before posting it publicly.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Only the end of the log matters; keeps the bundle small.
const LOG_TAIL_BYTES: u64 = 128 * 1024;
/// Last history rows: enough to see a trend without megabytes of CSV.
const HISTORY_TAIL_LINES: usize = 200;
/// Rainmeter log lines mentioning this skin.
const RAINMETER_TAIL_LINES: usize = 100;

pub fn write_bundle() -> Result<PathBuf, String> {
    let dir = crate::system::data_dir().ok_or("LOCALAPPDATA is not set")?;
    let (seconds, date) = {
        let (_, hms, date) = crate::system::local_time_parts();
        (hms.replace(':', ""), date)
    };
    let report = build_report(&dir);
    let path = dir.join(format!("diagnostics-{date}-{seconds}.txt"));
    std::fs::write(&path, report)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn build_report(dir: &Path) -> String {
    let mut out = String::new();
    let (clock, _, date) = crate::system::local_time_parts();

    let _ = writeln!(out, "Basilisk Power diagnostics");
    let _ = writeln!(out, "========================");
    let _ = writeln!(out, "version:   {}", crate::version_line());
    let _ = writeln!(out, "generated: {date} {clock} (local)");
    let _ = writeln!(
        out,
        "exe:       {}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("(unknown: {e})"))
    );
    let _ = writeln!(out, "platform:  {} {}", std::env::consts::OS, std::env::consts::ARCH);
    if let Some(ver) = windows_version() {
        let _ = writeln!(out, "windows:   {ver}");
    }
    let autostart = match crate::system::autostart_state() {
        crate::system::AutostartState::Enabled => "enabled".to_string(),
        crate::system::AutostartState::Disabled => "disabled".to_string(),
        crate::system::AutostartState::Stale => format!(
            "stale (points at {})",
            crate::system::autostart_target()
                .unwrap_or_else(|| "another path".to_string())
        ),
    };
    let _ = writeln!(out, "autostart: {autostart}");
    let _ = writeln!(out, "data dir:  {}", dir.display());
    let _ = writeln!(out);

    section(&mut out, "status.json", &read_text(&dir.join("status.json")));
    section(&mut out, "led-colors.txt", &read_text(&dir.join("led-colors.txt")));

    let _ = writeln!(out, "--- files ---");
    for name in [
        "status.json",
        "led-colors.txt",
        "battery-history.csv",
        "basilisk-power.log",
    ] {
        let path = dir.join(name);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .map(|s| format!("mtime_unix={s}"))
                    .unwrap_or_default();
                let _ = writeln!(out, "{name}: {} bytes {modified}", meta.len());
            }
            Err(_) => {
                let _ = writeln!(out, "{name}: missing");
            }
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "--- HID devices (Razer VID 0x1532) ---");
    let _ = writeln!(out, "{}", hid_devices());
    let _ = writeln!(out);

    let _ = writeln!(out, "--- widget (Rainmeter) ---");
    let _ = writeln!(out, "process: {}", rainmeter_state());
    let skin_dir = rainmeter_skin_dir();
    match skin_dir {
        Some(skin) => {
            for relative in [
                "basilisk.ini",
                "@Resources/Variables.inc",
                "@Resources/freshness.lua",
            ] {
                let path = skin.join(relative);
                match std::fs::metadata(&path) {
                    Ok(meta) => {
                        let _ = writeln!(out, "{relative}: {} bytes", meta.len());
                    }
                    Err(_) => {
                        let _ = writeln!(out, "{relative}: missing");
                    }
                }
            }
            if let Ok(ini) = std::fs::read_to_string(skin.join("basilisk.ini")) {
                for key in ["StaleSeconds", "LowBatteryPercent"] {
                    let value = ini
                        .lines()
                        .find_map(|line| line.trim().strip_prefix(&format!("{key}=")))
                        .unwrap_or("(not set)");
                    let _ = writeln!(out, "{key}: {value}");
                }
            }
        }
        None => {
            let _ = writeln!(out, "skin folder: unknown (USERPROFILE is not set)");
        }
    }
    if let Some(log) = rainmeter_log_tail() {
        let _ = writeln!(out, "Rainmeter log (BasiliskPower lines only):");
        let _ = writeln!(out, "{log}");
    }
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "--- basilisk-power.log (last {}) ---",
        human_bytes(LOG_TAIL_BYTES)
    );
    match read_tail(&dir.join("basilisk-power.log"), LOG_TAIL_BYTES) {
        Some(log) => {
            let _ = writeln!(out, "{log}");
        }
        None => {
            let _ = writeln!(out, "(missing or unreadable)");
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "--- battery-history.csv (last {HISTORY_TAIL_LINES} lines) ---"
    );
    match std::fs::read_to_string(dir.join("battery-history.csv")) {
        Ok(csv) => {
            let tail = tail_lines(&csv, HISTORY_TAIL_LINES);
            let _ = writeln!(out, "{tail}");
        }
        Err(_) => {
            let _ = writeln!(out, "(missing or unreadable)");
        }
    }

    out
}

fn section(out: &mut String, title: &str, body: &str) {
    let _ = writeln!(out, "--- {title} ---");
    let _ = writeln!(out, "{}", body.trim_end());
    let _ = writeln!(out);
}

fn read_text(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => format!("(unreadable: {e})"),
    }
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} bytes")
    }
}

/// Last `max_bytes` of a file as text, dropping a partial first line when the
/// cut landed mid-file.
fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.take(max_bytes).read_to_end(&mut buf).ok()?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    Some(text)
}

/// Last `n` lines, capped so a one-line file does not gain a blank line.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Rainmeter log lines that mention this skin, and nothing from other skins.
fn filter_lines(text: &str, needle: &str, n: usize) -> String {
    let matches: Vec<&str> = text
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains(needle))
        .collect();
    let start = matches.len().saturating_sub(n);
    matches[start..].join("\n")
}

fn rainmeter_skin_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|home| {
        PathBuf::from(home)
            .join("Documents")
            .join("Rainmeter")
            .join("Skins")
            .join("BasiliskPower")
    })
}

fn rainmeter_log_tail() -> Option<String> {
    let log = std::env::var_os("APPDATA").map(|appdata| {
        PathBuf::from(appdata)
            .join("Rainmeter")
            .join("Rainmeter.log")
    })?;
    let text = std::fs::read_to_string(log).ok()?;
    let filtered = filter_lines(&text, "basiliskpower", RAINMETER_TAIL_LINES);
    if filtered.is_empty() {
        Some("(no BasiliskPower lines)".to_string())
    } else {
        Some(filtered)
    }
}

/// True only when Rainmeter is actually running; log presence alone is not
/// enough (a closed Rainmeter keeps its log).
fn rainmeter_state() -> &'static str {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        match std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq Rainmeter.exe", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            Ok(out) if String::from_utf8_lossy(&out.stdout).contains("Rainmeter.exe") => {
                "running"
            }
            Ok(_) => "not running",
            Err(_) => "unknown (tasklist failed)",
        }
    }
    #[cfg(not(windows))]
    {
        "unknown (not Windows)"
    }
}

fn windows_version() -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn hid_devices() -> String {
    let mut api = match hidapi::HidApi::new() {
        Ok(api) => api,
        Err(e) => return format!("(HID enumeration failed: {e})"),
    };
    if let Err(e) = api.refresh_devices() {
        return format!("(HID refresh failed: {e})");
    }
    let mut lines = Vec::new();
    for device in api.device_list().filter(|d| d.vendor_id() == 0x1532) {
        lines.push(format!(
            "pid=0x{:04X} interface={} usage_page=0x{:04X} usage=0x{:04X} product={}",
            device.product_id(),
            device.interface_number(),
            device.usage_page(),
            device.usage(),
            device.product_string().unwrap_or("?")
        ));
    }
    if lines.is_empty() {
        "(no Razer HID devices found)".to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_keeps_the_end() {
        assert_eq!(tail_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(tail_lines("only", 5), "only");
        assert_eq!(tail_lines("", 5), "");
    }

    #[test]
    fn filter_keeps_only_matching_lines() {
        let log = "NOTE other.ini: hi\nNOTE BasiliskPower\\basilisk.ini: one\nNOTE clock: tick\nNOTE basilisk.ini: two";
        assert_eq!(
            filter_lines(log, "basiliskpower", 10),
            "NOTE BasiliskPower\\basilisk.ini: one"
        );
        assert_eq!(
            filter_lines(log, "basilisk.ini", 1),
            "NOTE basilisk.ini: two"
        );
    }

    #[test]
    fn read_tail_drops_the_partial_first_line() {
        let path = std::env::temp_dir().join(format!(
            "basilisk-diag-test-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
        let tail = read_tail(&path, 24).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!tail.contains("first line"), "tail was {tail:?}");
        assert!(tail.ends_with("third line\n"));
        assert!(tail.contains("second line"));
    }

    #[test]
    fn bundle_reports_core_state() {
        let dir = std::env::temp_dir().join(format!(
            "basilisk-diag-bundle-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("status.json"), r#"{"level":42}"#).unwrap();
        std::fs::write(dir.join("led-colors.txt"), "mouse=160054\n").unwrap();
        let report = build_report(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.contains("Basilisk Power diagnostics"));
        assert!(report.contains(r#"{"level":42}"#));
        assert!(report.contains("mouse=160054"));
        assert!(report.contains("battery-history.csv: missing"));
    }
}
