//! Disk hygiene for %LOCALAPPDATA%\BasiliskPower.
//!
//! Everything the app writes lives in one folder and is meant to stay small:
//!
//! | File                  | Growth              | Cap            |
//! |-----------------------|---------------------|----------------|
//! | `basilisk-power.log`  | per launch (truncate) | 2 MB         |
//! | `battery-history.csv` | 1 row per reading   | 1 MB / 2000 rows |
//! | `led-colors.txt`      | tiny, rewritten     | 4 lines        |
//! | `basilisk.ini.backup` | one per skin deploy | kept forever (rollback) |
//! | other `*.backup`      | one per deploy      | deleted after 7 days |
//! | `diagnostics-*.txt`   | one per export      | deleted after 7 days |
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

/// The deployed skin's rollback copy. It is hand-tuned by the user and cannot
/// be recreated from source, so age must never delete it.
const PRESERVED_BACKUP: &str = "basilisk.ini.backup";

/// Trim the files this app owns. Cheap, idempotent, safe to call on a timer.
///
/// Never touches the two executables, `led-colors.txt` or `status.json`: those
/// are small and load-bearing.
pub fn run_once(dir: &Path) {
    trim_to_size(&dir.join("basilisk-power.log"), MAX_LOG_BYTES, false);
    trim_history(&dir.join("battery-history.csv"));
    remove_stale_artifacts(dir);
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

/// Delete one-off setup and support artefacts once they are old enough to be
/// irrelevant.
fn remove_stale_artifacts(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let cutoff = SystemTime::now().checked_sub(MAX_BACKUP_AGE);

    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if is_preserved(&name) {
            continue;
        }

        let is_backup = name.ends_with(".backup") || name.ends_with(".bak");
        let is_diagnostics = name.starts_with("diagnostics-") && name.ends_with(".txt");
        if !is_backup && !is_diagnostics {
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

/// True for artefacts that must survive every cleanup pass.
fn is_preserved(name: &str) -> bool {
    name.eq_ignore_ascii_case(PRESERVED_BACKUP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skin_backup_is_never_treated_as_stale() {
        assert!(is_preserved("basilisk.ini.backup"));
        assert!(is_preserved("BASILISK.INI.BACKUP"));
    }

    #[test]
    fn other_artifacts_are_still_age_cleaned() {
        assert!(!is_preserved("basilisk-power.exe.backup"));
        assert!(!is_preserved("diagnostics-2026-11-18.txt"));
        assert!(!is_preserved("led-colors.txt"));
    }

    #[test]
    fn cleanup_keeps_the_skin_backup_and_removes_old_artifacts() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bp-maint-{}-{unique}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let backup = dir.join(PRESERVED_BACKUP);
        let old_diag = dir.join("diagnostics-old.txt");
        let old_bak = dir.join("led-colors.txt.bak");
        for path in [&backup, &old_diag, &old_bak] {
            fs::write(path, "x").unwrap();
        }

        // Age everything past the 7-day cutoff.
        let old = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
        for path in [&backup, &old_diag, &old_bak] {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        run_once(&dir);

        assert!(backup.exists(), "the hand-tuned skin backup was deleted");
        assert!(!old_diag.exists(), "an old diagnostics bundle survived");
        assert!(!old_bak.exists(), "an old .bak file survived");

        let _ = fs::remove_dir_all(&dir);
    }
}