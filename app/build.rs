//! Stamp the tray binary with its version and git hash, and embed the icon.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/basilisk-power.ico");
    println!("cargo:rerun-if-changed=../.git/HEAD");

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

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/basilisk-power.ico")
            .set("ProductName", "Basilisk Power")
            .set("FileDescription", "Basilisk Power")
            .set("CompanyName", "Anu-bhav")
            .set("FileVersion", &version)
            .set("ProductVersion", &version);
        res.compile().expect("failed to embed Windows resources");
    }
}
