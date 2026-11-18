//! Disk hygiene for %LOCALAPPDATA%\BasiliskPower.
//!
//! Everything the app writes lives in one folder and is meant to stay small:
//!
//! | File                  | Growth              | Cap            |
//! |-----------------------|---------------------|----------------|
//! | `basilisk-power.log`  | per launch (truncate) | 2 MB         |
//! | `battery-history.csv` | 1 row per reading   | 1 MB / 2000 rows |
//! | `led-colors.txt`      | tiny, rewritten     | 4 lines        |
//! | `*.backup`            | one-off from setup  | deleted after 7 days |
//!
//! The log is already truncated on every launch, so the caps here are a
//! safety net: if a bug ever makes the app log in a tight loop, or a reading
//! lands every tick, the file is trimmed instead of growing without bound.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Hard ceiling for the log. A GUI tray app writing at info level stays in the
/// low hundreds of KB; 2 MB means something is looping.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// Hard ceiling for the history CSV. Rows are ~25 bytes, so this is roughly
/// 40k readings -- months of data at normal rates.
const MAX_HISTORY_BYTES: u64 = 1024 * 1024;

/// Rows kept when the history file is trimmed.
const KEEP_HISTORY_ROWS: usize = 2000;

/// One-off files (`.backup` and friends) older than this are removed.
const MAX_BACKUP_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Trim the files this app owns. Cheap, idempotent, safe to call on a timer.
///
/// Never touches the two executables, `led-colors.txt` or `status.json`: those
/// are small and load-bearing.
pub fn run_once(dir: &Path) {
    trim_to_size(&dir.join("basilisk-power.log"), MAX_LOG_BYTES, false);
    trim_history(&dir.join("battery-history.csv"));
    remove_stale_backups(dir);
}

/// Keep the newest `keep_lines` of a text file, dropping the oldest.
fn trim_to_size(path: &Path, max_bytes: u64, keep_header: bool) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() <= max_bytes {
        return;
    }

    let Ok(text) = fs::read_to_string(path) else {
        return;
    };

    let keep = if keep_header {
        KEEP_HISTORY_ROWS
    } else {
        2000
    };

    let mut lines: Vec<&str> = text.lines().collect();
    if lines.len() > keep {
        let drain = lines.len() - keep;
        lines.drain(0..drain);
    }

    let mut out = String::with_capacity(lines.len() * 32);
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    let _ = fs::write(path, out);
}

/// Trim the history CSV, preserving its header row.
fn trim_history(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_HISTORY_BYTES {
        return;
    }

    let Ok(text) = fs::read_to_string(path) else {
        return;
    };

    let mut lines: Vec<&str> = text.lines().filter(|l| !l.starts_with("date,")).collect();
    if lines.len() > KEEP_HISTORY_ROWS {
        let drain = lines.len() - KEEP_HISTORY_ROWS;
        lines.drain(0..drain);
    }

    let mut out = String::from("date,time,level,charging,source\n");
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    let _ = fs::write(path, out);
}

/// Delete one-off setup artefacts once they are old enough to be irrelevant.
fn remove_stale_backups(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let cutoff = SystemTime::now().checked_sub(MAX_BACKUP_AGE);

    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        let is_backup = name.ends_with(".backup") || name.ends_with(".bak");
        if !is_backup {
            continue;
        }
        let Some(cutoff) = cutoff else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < cutoff {
            let _ = fs::remove_file(&path);
        }
    }
}