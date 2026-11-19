#![windows_subsystem = "windows"]

mod battery_icon;
mod controller;
mod debounce;
mod devices;
mod diagnostics;
mod feed;
mod led;
mod maintenance;
mod manager;
mod notify;
mod options;
mod reminder;
mod system;
mod tray;
mod wake;

use log::LevelFilter;
use options::Options;
use tray::TrayApp;

/// The version line shown by `--version` and the tray menu.
pub fn version_line() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("BASILISK_GIT_HASH"))
}

fn main() {
    install_panic_hook();

    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(e) => {
            system::show_error(&e);
            std::process::exit(2);
        }
    };

    // Handle --version before opening the log: a version query must never
    // truncate the log of a tray that is already running.
    if options.version {
        system::show_version(&format!("basilisk-power {}", version_line()));
        return;
    }

    // Simulated runs write to their own folder: sharing the live folder
    // would truncate the running tray's log and replace its feed/history.
    if options.uses_simulation_data() {
        if let Err(e) = system::use_simulation_data_dir() {
            system::show_error(&format!(
                "Could not prepare the simulation data folder:\n{e}"
            ));
            std::process::exit(1);
        }
    }

    // Before the logger is initialized: diagnostics reads the *running*
    // instance's log, and init_file_logger truncates it.
    if options.diagnostics {
        match diagnostics::write_bundle() {
            Ok(path) => system::show_version(&format!(
                "Diagnostics written to:\n{}",
                path.display()
            )),
            Err(e) => {
                system::show_error(&format!("Could not write diagnostics:\n{e}"));
                std::process::exit(1);
            }
        }
        return;
    }

    if options.install_autostart {
        match system::set_autostart(true) {
            Ok(()) => system::show_version("Basilisk Power will now start with Windows."),
            Err(e) => {
                log::error!("Failed to enable autostart: {e}");
                system::show_error(&format!("Could not enable autostart:\n{e}"));
                std::process::exit(1);
            }
        }
        return;
    }

    if options.uninstall_autostart {
        match system::set_autostart(false) {
            Ok(()) => system::show_version("Basilisk Power will no longer start with Windows."),
            Err(e) => {
                log::error!("Failed to disable autostart: {e}");
                system::show_error(&format!("Could not disable autostart:\n{e}"));
                std::process::exit(1);
            }
        }
        return;
    }

    // A second instance would draw a second tray icon and, more importantly,
    // truncate the running instance's log when the logger opens below. Take
    // the lock before touching the log; a duplicate launch exits silently.
    // Simulated runs have their own data folder and are allowed alongside the
    // real app.
    if !options.uses_simulation_data() && !system::acquire_single_instance_lock() {
        return;
    }

    // GUI-subsystem exe: stderr goes nowhere, so log to a file we can read
    // when something looks wrong in the tray.
    init_file_logger();
    log::info!("Basilisk Power {} starting", version_line());

    if options != Options::default() {
        log::info!("Options: {options:?}");
    }

    // Keep the data folder from growing without bound: trim the log and the
    // history CSV, and drop stale one-off files.
    maintenance::run_once(&system::data_dir().unwrap_or_default());

    TrayApp::new(options).run();
}

/// Fresh log file per launch at %LOCALAPPDATA%\BasiliskPower\basilisk-power.log;
/// falls back to stderr when the file can't be opened (e.g. tests).
fn init_file_logger() {
    std::env::set_var("RUST_LOG", "info");
    let file = system::data_dir().and_then(|dir| {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dir.join("basilisk-power.log"))
            .ok()
    });
    let mut builder = env_logger::Builder::new();
    builder.filter_level(LevelFilter::Info);
    match file {
        Some(file) => {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
        None => {
            builder.target(env_logger::Target::Stderr);
        }
    }
    builder.init();
}

/// Rust panic messages print to stderr, which a GUI-subsystem exe has
/// nowhere to show — route them into the log file so a silent death is
/// explainable after the fact.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("Panic: {info}");
        default_hook(info);
    }));
}
