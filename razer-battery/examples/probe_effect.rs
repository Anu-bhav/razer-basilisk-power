//! Probe whether the Mouse Dock Pro answers a GET_EFFECT / colour query.
//!
//! OpenRazer only defines write attributes for effects, but the extended
//! Chroma protocol has a GET_EFFECT command (0x0F/0x82). This example prints
//! the raw response for both transaction IDs so we can see what the hardware
//! actually supports.

use razer_battery::lighting;
use razer_battery::protocol::{build_report, compute_crc, send_command};

fn main() {
    let api = hidapi::HidApi::new().expect("HID init");
    let pid = if lighting::is_present(&api, lighting::PID_DOCK_PRO) {
        lighting::PID_DOCK_PRO
    } else if lighting::is_present(&api, lighting::PID_MOUSE_WIRED) {
        lighting::PID_MOUSE_WIRED
    } else {
        lighting::PID_MOUSE_WIRELESS
    };

    let hid = lighting::open(&api, pid).expect("open device");
    for tid in [lighting::TID_MOUSE, lighting::TID_DOCK] {
        for size in [0x03u8, 0x02u8] {
            let mut report = build_report(tid, 0x0f, 0x82, size);
            report[8] = 0x01; // varstore
            report[9] = 0x00; // led
            report[10] = 0x00;
            report[88] = compute_crc(&report);
            println!("tid {tid:#04x} size {size:#04x}: sending...");
            match send_command(&hid, &report) {
                Ok(resp) => {
                    println!("  OK  status={:#04x} args={:02x?}", resp[0], &resp[8..24]);
                }
                Err(e) => println!("  ERR {e}"),
            }
        }
    }
}
