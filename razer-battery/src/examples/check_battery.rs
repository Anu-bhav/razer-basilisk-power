//! Example: scan for connected Razer devices and print battery info.
//!
//! Run with:
//!     cargo run --example check_battery
//!
//! Enable debug logging to troubleshoot connection issues:
//!     RUST_LOG=debug cargo run --example check_battery

use razer_battery::{devices::custom_device, find_devices, get_battery_info};

fn main() {
    env_logger::init();

    println!("=== Razer Battery Checker ===");
    println!();
    println!("Scanning for Razer wireless devices...");
    println!();

    match find_devices() {
        Ok(devices) if devices.is_empty() => {
            println!("No known Razer wireless devices found.");
            println!();
            println!("Tips:");
            println!("  - Make sure the wireless receiver/dongle is plugged in");
            println!("  - On Windows: close Razer Synapse (it claims exclusive HID access)");
            println!("  - On Linux: check udev rules (see README)");
            println!();
            println!("For unlisted devices, create a custom entry:");
            println!("  let dev = custom_device(\"My Mouse\", 0x00XX, 0, 0x0001, 0x0002, 0x1f);");
        }
        Ok(devices) => {
            println!("Found {} device(s):", devices.len());
            println!();
            for dev in &devices {
                print!(
                    "  {} (PID {:#06x}, iface {}) ... ",
                    dev.name, dev.pid, dev.interface
                );

                match get_battery_info(dev) {
                    Ok(info) => {
                        let bar = battery_bar(info.level);
                        let status = if info.charging { " [CHARGING]" } else { "" };
                        println!("{}% {} {}", info.level, bar, status);
                    }
                    Err(e) => {
                        println!("ERROR: {}", e);
                        if cfg!(target_os = "macos") && format!("{}", e).contains("not permitted") {
                            println!(
                                "         macOS: Grant Input Monitoring permission to your terminal app"
                            );
                            println!(
                                "         (System Settings > Privacy & Security > Input Monitoring)"
                            );
                        } else {
                            println!("         Try running with RUST_LOG=debug for more info");
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("Failed to initialize HID: {}", e),
    }

    println!();

    // ─── Uncomment below to test a custom device ───
    // let my_mouse = custom_device("My Mouse", 0x00AB, 2, 0x0001, 0x0002, 0x3f);
    // match get_battery_info(&my_mouse) {
    //     Ok(info) => println!("Custom: {}% charging={}", info.level, info.charging),
    //     Err(e) => eprintln!("Custom device error: {}", e),
    // }
}

fn battery_bar(pct: u8) -> String {
    let filled = (pct as usize) / 5;
    let empty = 20 - filled;
    format!(
        "[{}{}]",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(empty)
    )
}
