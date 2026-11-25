//! basilisk-power-cli — battery + LED tool for the Razer Basilisk V3 Pro.
//!
//! Modes:
//!   basilisk-power-cli                  one-shot scan, print battery status
//!   basilisk-power-cli --watch          keep running, poll adaptively (see common.rs)
//!   basilisk-power-cli --interval N     force fixed polling every N seconds
//!   basilisk-power-cli --json           one-shot machine-readable output
//!   basilisk-power-cli --probe          try interface / transaction-id combos per PID
//!   basilisk-power-cli --list           enumerate Razer (VID 0x1532) HID entries
//!   basilisk-power-cli --link           query the Mouse Dock Pro RF link heartbeat
//!   basilisk-power-cli --set-color ...  set static LED colour(s)
//!   basilisk-power-cli --get-color      read back the stored static colour(s)
//!   basilisk-power-cli --debug          verbose HID logging
//!
//! For the background tray icon, run `basilisk-power-tray` instead.

mod common;

use std::io::Write as _;
use std::time::{Duration, Instant};

use chrono::Local;
use razer_battery::devices::custom_device;
use razer_battery::lighting;
use razer_battery::{BatterySession, RazerError};

mod history;

use common::{Row, label, measure_all, next_wait};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_HASH: &str = env!("BASILISK_GIT_HASH");

enum Mode {
    Once,
    Watch,
    Probe,
    List,
    Link,
    SetColor,
    GetColor,
    WatchLed,
    History(history::Window),
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn bar(level: u8, width: usize) -> String {
    let filled = (level as usize * width) / 100;
    format!("[{}{}]", "#".repeat(filled), "-".repeat(width - filled))
}

fn render_line(row: &Row) -> String {
    let ts = Local::now().format("%H:%M:%S");
    match &row.reading {
        common::Reading::Info { level, charging } => format!(
            "{ts}  {:<30} {:>3}%  {}  {}",
            label(row),
            level,
            bar(*level, 20),
            if *charging {
                "charging (docked)"
            } else {
                "on battery"
            },
        ),
        common::Reading::NotPresent => format!("{ts}  {:<30}  --   not present", label(row)),
        common::Reading::Failed(e) => format!("{ts}  {:<30}  !!   {e}", label(row)),
    }
}

fn json_line(row: &Row) -> String {
    match &row.reading {
        common::Reading::Info { level, charging } => format!(
            r#"{{"name":"{}","pid":"{:#06x}","level":{},"charging":{},"ok":true}}"#,
            row.name, row.pid, level, *charging
        ),
        common::Reading::NotPresent => format!(
            r#"{{"name":"{}","pid":"{:#06x}","ok":false,"error":"not_present"}}"#,
            row.name, row.pid
        ),
        common::Reading::Failed(e) => format!(
            r#"{{"name":"{}","pid":"{:#06x}","ok":false,"error":"{}"}}"#,
            row.name,
            row.pid,
            e.replace('"', "'")
        ),
    }
}

// ── Modes ────────────────────────────────────────────────────────────────────

fn once(json: bool) {
    let rows = measure_all();
    if json {
        let items: Vec<String> = rows.iter().map(json_line).collect();
        println!(r#"{{"devices":[{}]}}"#, items.join(","));
        return;
    }
    println!(
        "Razer battery scan — {}",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    println!();
    for row in &rows {
        println!("{}", render_line(row));
    }
    if rows
        .iter()
        .all(|r| matches!(r.reading, common::Reading::NotPresent))
    {
        println!();
        println!("No devices responded. Tips:");
        println!("  - Move the mouse / click a button to wake it from sleep");
        println!("  - Make sure the Mouse Dock Pro (or HyperSpeed dongle) is plugged in");
        println!("  - If Razer Synapse holds exclusive HID access, close it and retry");
        println!("  - Run with --debug for per-interface HID logs, or --probe to test combos");
    }
}

fn watch(fixed: Option<Duration>, json: bool) {
    loop {
        let rows = measure_all();
        for row in &rows {
            let line = if json {
                json_line(row)
            } else {
                render_line(row)
            };
            println!("{line}");
        }
        let _ = std::io::stdout().flush();
        std::thread::sleep(next_wait(&rows, fixed));
    }
}

fn probe() {
    let api = hidapi::HidApi::new().expect("failed to initialize hidapi");
    let present: Vec<u16> = [0x00AAu16, 0x00AB, 0x00A4]
        .into_iter()
        .filter(|pid| {
            api.device_list()
                .any(|i| i.vendor_id() == 0x1532 && i.product_id() == *pid)
        })
        .collect();

    println!("Probing combo: interface x transaction_id (this can take a while)");
    println!(
        "Present PIDs: {:?}",
        present
            .iter()
            .map(|p| format!("{p:#06x}"))
            .collect::<Vec<_>>()
    );
    println!();

    for pid in present {
        for iface in [0i32, 1, 2] {
            for tid in [0x1fu8, 0x0f, 0x3f, 0xff] {
                let dev = custom_device(
                    "probe",
                    pid,
                    iface,
                    common::USAGE_PAGE,
                    common::USAGE_MOUSE,
                    tid,
                );
                print!("  {pid:#06x} iface {iface} tid {tid:#04x}  ...  ");
                let _ = std::io::stdout().flush();
                match BatterySession::new(&dev) {
                    Ok(sess) => match sess.query() {
                        Ok(info) => println!("OK   {}% charging={}", info.level, info.charging),
                        Err(e) => println!("query failed: {e}"),
                    },
                    Err(RazerError::DeviceNotFound { .. }) => {
                        println!("no matching HID interface");
                    }
                    Err(e) => println!("open failed: {e}"),
                }
            }
        }
    }
    println!();
    println!("Combos marked OK are the ones the app should use.");
}

fn link() {
    let dev = custom_device(
        "Mouse Dock Pro",
        0x00A4,
        0,
        common::USAGE_PAGE,
        common::USAGE_MOUSE,
        0x1f,
    );
    let started = Instant::now();
    match BatterySession::new(&dev) {
        Ok(session) => match session.link_state() {
            Ok(true) => println!("dock RF link: UP ({} ms)", started.elapsed().as_millis()),
            Ok(false) => println!(
                "dock RF link: DOWN - mouse asleep, off, or out of range ({} ms)",
                started.elapsed().as_millis()
            ),
            Err(e) => println!(
                "dock RF link query failed: {e} ({} ms)",
                started.elapsed().as_millis()
            ),
        },
        Err(e) => println!("open failed: {e}"),
    }
}

fn list() {
    let api = hidapi::HidApi::new().expect("failed to initialize hidapi");
    println!("HID entries for VID 0x1532 (Razer):");
    for info in api.device_list() {
        if info.vendor_id() != 0x1532 {
            continue;
        }
        println!(
            "  PID {:#06x}  iface {:>2}  usage_page {:#06x}  usage {:#06x}  serial {:?}  path {:?}",
            info.product_id(),
            info.interface_number(),
            info.usage_page(),
            info.usage(),
            info.serial_number().map(|s| s.to_string()),
            String::from_utf8_lossy(info.path().to_bytes()),
        );
    }
}

fn apply_requests(requests: &[lighting::ColorRequest], brightness: Option<u8>) {
    let summary = requests
        .iter()
        .map(|request| {
            format!(
                "{} #{:02X}{:02X}{:02X}",
                request.target.key(),
                request.rgb.0,
                request.rgb.1,
                request.rgb.2
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    println!("Static colour: {summary}");

    let hw_brightness = brightness.map(lighting::percent_to_brightness);
    for outcome in lighting::apply(requests, hw_brightness) {
        if let Some(error) = &outcome.error {
            eprintln!("{}: {error}", outcome.device);
            continue;
        }
        println!("{}:", outcome.device);
        for step in &outcome.steps {
            match &step.result {
                Ok(()) => println!("  {}: OK", step.label),
                Err(e) => eprintln!("  {}: FAILED - {e}", step.label),
            }
        }
    }

    // Confirm the write landed. A stale handle or a dock/charge transition can
    // accept the command without retaining the colour; one retry is enough.
    for request in requests {
        let mut landed = lighting::read_color(request.target) == Some(request.rgb);
        if !landed {
            let _ = lighting::apply(std::slice::from_ref(request), hw_brightness);
            landed = lighting::read_color(request.target) == Some(request.rgb);
        }
        if landed {
            println!("{}: verified", request.target.label());
        } else {
            eprintln!(
                "{}: warning - read-back did not match",
                request.target.label()
            );
        }
    }

    if let Some(percent) = brightness {
        lighting::save_brightness(percent);
    }
}

fn print_colors(target: &str) {
    let mut targets = Vec::new();
    if target == "mouse" || target == "both" {
        targets.push(lighting::Target::Mouse);
    }
    if target == "dock" || target == "both" {
        targets.push(lighting::Target::Dock);
    }

    for reading in lighting::read_colors(&targets) {
        let hex = |rgb: lighting::Rgb| format!("#{:02X}{:02X}{:02X}", rgb.0, rgb.1, rgb.2);
        match reading.color {
            Some(rgb) => {
                // This firmware can answer a brightness read with 0 even while
                // the LEDs are visibly on. Treat 0 as "no reading" rather than
                // printing a misleading 0% beside a lit device.
                let bright = lighting::read_brightness(reading.target)
                    .filter(|v| *v > 0)
                    .map(|v| format!(", brightness {}%", (v as u16 * 100 / 255)));
                match bright {
                    Some(b) => println!(
                        "{} ({}) -> {} [static{}]",
                        reading.target.label(),
                        reading.device,
                        hex(rgb),
                        b
                    ),
                    None => println!(
                        "{} ({}) -> {} [static]",
                        reading.target.label(),
                        reading.device,
                        hex(rgb)
                    ),
                }
            }
            None => {
                let effect = reading
                    .effect
                    .map(|id| format!("effect {id:#04x}"))
                    .unwrap_or_else(|| "no answer".to_string());
                match lighting::saved_color(reading.target) {
                    Some(rgb) => println!(
                        "{} ({}) -> {} [last applied; device reports {effect}]",
                        reading.target.label(),
                        reading.device,
                        hex(rgb)
                    ),
                    None => match &reading.error {
                        Some(error) => {
                            eprintln!("{} ({}): {error}", reading.target.label(), reading.device)
                        }
                        None => println!(
                            "{} ({}) -> no stored static colour ({effect})",
                            reading.target.label(),
                            reading.device
                        ),
                    },
                }
            }
        }
    }
}

/// Poll the stored LED colours and effect ids until interrupted.
///
/// Useful for confirming that a colour was written and that the device kept
/// it -- for example while checking that the charging animation comes back.
fn watch_led() {
    println!("Watching LED state (Ctrl-C to stop)");
    loop {
        let ts = chrono::Local::now().format("%H:%M:%S");
        let mut line = format!("{ts}");
        for target in [lighting::Target::Mouse, lighting::Target::Dock] {
            let state = lighting::read_colors(&[target])
                .into_iter()
                .next()
                .map(|r| match r.color {
                    Some(rgb) => format!(
                        "#{:02X}{:02X}{:02X} effect={:#04x}",
                        rgb.0,
                        rgb.1,
                        rgb.2,
                        r.effect.unwrap_or(0)
                    ),
                    None => r.error.unwrap_or_else(|| "no static colour".to_string()),
                })
                .unwrap_or_else(|| "no answer".to_string());
            line.push_str(&format!("  |  {}: {state}", target.key()));
        }
        println!("{line}");
        let _ = std::io::stdout().flush();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn help() {
    println!(
        "basilisk-power-cli — Razer Basilisk V3 Pro battery + LED tool\n\
         \n\
         usage: basilisk-power-cli [options]\n\
         \n\
         options:\n\
         \x20 --watch        keep running, poll adaptively (rare when full, often when low)\n\
         \x20 --interval N   force fixed polling every N seconds (overrides adaptive)\n\
         \x20 --json         one-shot machine-readable output\n\
         \x20 --probe        try interface/transaction-id combos per connected PID\n\
         \x20 --list         enumerate Razer HID entries (VID 0x1532)\n\
         \x20 --link         query the Mouse Dock Pro RF link heartbeat\n\
         \x20 --set-color RRGGBB     static LED colour for both targets\n\
         \x20 --mouse-color RRGGBB   static LED colour for the mouse only\n\
         \x20 --dock-color RRGGBB    static LED colour for the dock only\n\
         \x20 --target T     mouse | dock | both (with --set-color / --get-color)\n\
         \x20 --brightness N  LED brightness 0-100 (with any colour option)\n\
         \x20 --get-color    read the stored static colour and brightness\n\
         \x20 --watch-led    poll the stored LED colours and effects once a second\n\
         \x20 --history [W]  summarise battery-history.csv (7d default, 24h, all)\n\
         \x20 --debug        verbose HID logging\n\
         \x20 --version      print the version and exit\n\
         \x20 -h, --help     this help\n\
         \n\
         background tray icon: run basilisk-power-tray"
    );
}

fn main() {
    let mut mode = Mode::Once;
    let mut interval: Option<u64> = None;
    let mut json = false;
    let mut debug = false;
    let mut color: Option<lighting::Rgb> = None;
    let mut mouse_color: Option<lighting::Rgb> = None;
    let mut dock_color: Option<lighting::Rgb> = None;
    let mut target = String::from("both");
    let mut brightness: Option<u8> = None;

    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("basilisk-power-cli {VERSION} ({GIT_HASH})");
                return;
            }
            "--watch" => mode = Mode::Watch,
            "--interval" => match it.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(n) if n >= 1 => interval = Some(n),
                _ => {
                    eprintln!("--interval needs a number of seconds >= 1");
                    std::process::exit(2);
                }
            },
            "--json" => json = true,
            "--probe" => mode = Mode::Probe,
            "--list" => mode = Mode::List,
            "--link" => mode = Mode::Link,
            "--set-color" => match it.next().as_deref().and_then(lighting::parse_hex_color) {
                Some(rgb) => {
                    color = Some(rgb);
                    mode = Mode::SetColor;
                }
                None => {
                    eprintln!("--set-color needs a hex colour such as 00CCFF");
                    std::process::exit(2);
                }
            },
            "--mouse-color" => match it.next().as_deref().and_then(lighting::parse_hex_color) {
                Some(rgb) => {
                    mouse_color = Some(rgb);
                    mode = Mode::SetColor;
                }
                None => {
                    eprintln!("--mouse-color needs a hex colour such as 00CCFF");
                    std::process::exit(2);
                }
            },
            "--dock-color" => match it.next().as_deref().and_then(lighting::parse_hex_color) {
                Some(rgb) => {
                    dock_color = Some(rgb);
                    mode = Mode::SetColor;
                }
                None => {
                    eprintln!("--dock-color needs a hex colour such as FF2BFF");
                    std::process::exit(2);
                }
            },
            "--get-color" => mode = Mode::GetColor,
            "--watch-led" => mode = Mode::WatchLed,
            "--history" => {
                // The window is optional; only consume the next argument when
                // it is a value rather than another flag.
                let window = match it.peek() {
                    Some(next) if !next.starts_with('-') => {
                        history::Window::parse(&it.next().unwrap())
                    }
                    _ => history::Window::parse("7d"),
                };
                match window {
                    Ok(window) => mode = Mode::History(window),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(2);
                    }
                }
            }
            "--target" => match it.next().as_deref() {
                Some(value @ ("mouse" | "dock" | "both")) => target = value.to_string(),
                _ => {
                    eprintln!("--target must be mouse, dock or both");
                    std::process::exit(2);
                }
            },
            "--brightness" => match it.next().and_then(|v| v.parse::<u8>().ok()) {
                Some(value) if value <= 100 => brightness = Some(value),
                _ => {
                    eprintln!("--brightness needs a number from 0 to 100");
                    std::process::exit(2);
                }
            },
            "--debug" => debug = true,
            "-h" | "--help" => {
                help();
                return;
            }
            other => {
                eprintln!("unknown argument: {other}\n");
                help();
                std::process::exit(2);
            }
        }
    }

    if debug {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    }

    match mode {
        Mode::Once => once(json),
        Mode::Watch => watch(interval.map(Duration::from_secs), json),
        Mode::Probe => probe(),
        Mode::List => list(),
        Mode::Link => link(),
        Mode::GetColor => print_colors(&target),
        Mode::WatchLed => watch_led(),
        Mode::History(window) => {
            let Some(path) = history::default_path() else {
                eprintln!("LOCALAPPDATA is not set, so the history file cannot be found");
                std::process::exit(2);
            };
            match history::load(&path, window) {
                Ok((_summary, text)) => print!("{text}"),
                Err(e) => {
                    eprintln!("cannot read {}: {e}", path.display());
                    eprintln!("The tray writes this file while it is running.");
                    std::process::exit(1);
                }
            }
        }
        Mode::SetColor => {
            let mut requests = Vec::new();

            if let Some(rgb) = color {
                if target == "mouse" || target == "both" {
                    requests.push(lighting::ColorRequest {
                        target: lighting::Target::Mouse,
                        rgb,
                    });
                }
                if target == "dock" || target == "both" {
                    requests.push(lighting::ColorRequest {
                        target: lighting::Target::Dock,
                        rgb,
                    });
                }
            }

            if let Some(rgb) = mouse_color {
                requests.retain(|request| request.target != lighting::Target::Mouse);
                requests.push(lighting::ColorRequest {
                    target: lighting::Target::Mouse,
                    rgb,
                });
            }

            if let Some(rgb) = dock_color {
                requests.retain(|request| request.target != lighting::Target::Dock);
                requests.push(lighting::ColorRequest {
                    target: lighting::Target::Dock,
                    rgb,
                });
            }

            if requests.is_empty() {
                eprintln!("--set-color, --mouse-color or --dock-color is required");
                std::process::exit(2);
            }

            apply_requests(&requests, brightness);
        }
    }
}
