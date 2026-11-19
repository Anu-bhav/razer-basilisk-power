//! Stamp the git hash into the binary so `--version` can identify exactly
//! which build is installed.

use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=BASILISK_GIT_HASH={hash}");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    // Commits move the branch ref, not HEAD; watch it too or the embedded
    // hash goes stale after the first build.
    println!("cargo:rerun-if-changed=../.git/refs/heads/main");
    println!("cargo:rerun-if-changed=../.git/packed-refs");
}
