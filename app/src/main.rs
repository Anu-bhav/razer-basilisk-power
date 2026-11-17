#![windows_subsystem = "windows"]

mod battery_icon;
mod controller;
mod debounce;
mod devices;
mod feed;
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

fn main() {
    // GUI-subsystem exe: stderr goes nowhere, so log to a file we can read
    // when something looks wrong in the tray.
    init_file_logger();
    log::info!("Basilisk Power starting");

    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(e) => {
            log::error!("{e}");
            std::process::exit(2);
        }
    };
    if options != Options::default() {
        log::info!("Options: {options:?}");
    }

    // A second instance would draw a second tray icon; block it. Simulated
    // runs (testing flags) are allowed alongside the real app.
    if options.simulate_battery.is_none() && !system::acquire_single_instance_lock() {
        log::info!("Another instance is already running, exiting");
        return;
    }

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
