//! Keep the static LED colour on the mouse and the dock.
//!
//! The colour picker (and `basilisk-power-cli`) write a static colour through
//! the extended-matrix command (`class 0x0F / 0x02`) on the dock's RF relay
//! (`transaction id 0x1F`). That colour sticks — until the dock re-asserts its
//! own charging LED state after a dock or charge transition. The visible
//! result is the LED snapping back to the colour it had while charging.
//!
//! The hardware offers no lever for this on the Basilisk V3 Pro + Mouse Dock
//! Pro: the legacy battery-LED colour command (`0x03 / 0x01`) and the dock
//! charge-type command (`0x03 / 0x10`) are both rejected with status `0x05`,
//! and the extended-matrix read of the battery LED (LED `0x03`) times out.
//! So the only reliable fix is to write the static colour again, a few times,
//! until the device echoes it back.
//!
//! This module is deliberately self-contained (the tray app vendors its own
//! HID controller rather than depending on the `razer-battery` crate) and
//! mirrors `razer_battery::lighting`'s write/read layout.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::controller::DeviceController;

const CMD_CLASS_DEVICE: u8 = 0x00;
const CMD_CLASS_LED: u8 = 0x0f;
const CMD_SET_DEVICE_MODE: u8 = 0x04;
const CMD_SET_EFFECT: u8 = 0x02;
const CMD_GET_EFFECT: u8 = 0x82;

const VARSTORE: u8 = 0x01;
const ZERO_LED: u8 = 0x00;
const EFFECT_STATIC: u8 = 0x01;

/// Transaction id the Mouse Dock Pro relays to the paired mouse.
const TID_MOUSE: u8 = 0x1f;
/// Transaction id that addresses the Mouse Dock Pro itself.
const TID_DOCK: u8 = 0xff;

/// How many times to write the colour before giving up on it sticking.
const MAX_WRITES: u32 = 4;
/// Pause between writes, so the dock has time to re-assert (and then be
/// overwritten by) its charging state.
const WRITE_GAP: Duration = Duration::from_millis(250);

/// A colour the user picked, as stored by the picker / CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Mouse,
    Dock,
}

impl Target {
    pub fn key(self) -> &'static str {
        match self {
            Target::Mouse => "mouse",
            Target::Dock => "dock",
        }
    }
}

pub type Rgb = (u8, u8, u8);

fn config_path() -> Option<PathBuf> {
    crate::system::data_dir().map(|dir| dir.join("led-colors.txt"))
}

/// The last colour successfully written to a target.
pub fn saved_color(target: Target) -> Option<Rgb> {
    let text = fs::read_to_string(config_path()?).ok()?;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == target.key() {
            let hex = value.trim().trim_start_matches('#');
            if hex.len() != 6 {
                return None;
            }
            return Some((
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            ));
        }
    }
    None
}

fn set_static(controller: &DeviceController, tid: u8, rgb: Rgb) -> Result<(), String> {
    let (r, g, b) = rgb;
    let mut request = controller.create_command(CMD_CLASS_LED, CMD_SET_EFFECT, 0x09);
    request.transaction_id = tid;
    // OpenRazer layout: varstore, led, effect, 0, 0, 0x01, R, G, B
    request.arguments[0] = VARSTORE;
    request.arguments[1] = ZERO_LED;
    request.arguments[2] = EFFECT_STATIC;
    request.arguments[3] = 0;
    request.arguments[4] = 0;
    request.arguments[5] = 0x01;
    request.arguments[6] = r;
    request.arguments[7] = g;
    request.arguments[8] = b;
    controller
        .send_payload(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Read the static colour back; `None` when the device reports no static one.
fn get_static(controller: &DeviceController, tid: u8) -> Option<Rgb> {
    let mut request = controller.create_command(CMD_CLASS_LED, CMD_GET_EFFECT, 0x03);
    request.transaction_id = tid;
    request.arguments[0] = VARSTORE;
    request.arguments[1] = ZERO_LED;
    request.arguments[2] = 0x00;
    let response = controller.send_payload(request).ok()?;
    // Static replies mirror the write layout: arguments[0]=varstore,
    // [1]=led, [2]=effect, [3..5]=0, [5]=0x01, [6..8]=RGB.
    if *response.arguments.get(2)? != EFFECT_STATIC {
        return None;
    }
    if *response.arguments.get(5)? != 0x01 {
        return None;
    }
    Some((
        *response.arguments.get(6)?,
        *response.arguments.get(7)?,
        *response.arguments.get(8)?,
    ))
}

/// Put the dock into normal mode. OpenRazer does this before writing the
/// dock's own underglow, and re-issues the colour afterwards.
fn dock_normal_mode(controller: &DeviceController) -> Result<(), String> {
    let mut request = controller.create_command(CMD_CLASS_DEVICE, CMD_SET_DEVICE_MODE, 0x02);
    request.transaction_id = TID_DOCK;
    controller
        .send_payload(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn set_dock(controller: &DeviceController, rgb: Rgb) -> Result<(), String> {
    dock_normal_mode(controller)?;
    set_static(controller, TID_DOCK, rgb)?;

    // The dock needs the "commit" report on the mouse channel after the
    // colour, otherwise the next colour change lands one step late.
    let mut commit = controller.create_command(CMD_CLASS_LED, CMD_SET_EFFECT, 0x06);
    commit.transaction_id = TID_MOUSE;
    commit.arguments[2] = 0x08;
    controller
        .send_payload(commit)
        .map(|_| ())
        .map_err(|e| e.to_string())?;

    set_static(controller, TID_DOCK, rgb)
}

/// Write one colour once, then read it back.
fn write_once(
    controller: &DeviceController,
    target: Target,
    rgb: Rgb,
) -> Result<Option<Rgb>, String> {
    match target {
        Target::Mouse => {
            set_static(controller, TID_MOUSE, rgb)?;
            Ok(get_static(controller, TID_MOUSE))
        }
        Target::Dock => {
            set_dock(controller, rgb)?;
            Ok(get_static(controller, TID_DOCK))
        }
    }
}

/// Re-apply a colour until the device echoes it back, so it survives the dock
/// re-asserting its charging state. Returns the last colour that was read back.
///
/// `controller` must be the tray's already-open handle for the device. A
/// second handle opened while the tray is running does not reach the dock's
/// LED pipeline, so the colour silently stays whatever it was.
pub fn reapply(controller: &DeviceController, target: Target, rgb: Rgb) -> Option<Rgb> {
    let mut last = None;

    for _ in 0..MAX_WRITES {
        match write_once(controller, target, rgb) {
            Ok(color) => {
                last = color;
                if color == Some(rgb) {
                    return last;
                }
            }
            Err(_) => {
                // The device is not answering (asleep / not present): leave
                // whatever colour it had alone.
                return last;
            }
        }
        thread::sleep(WRITE_GAP);
    }

    last
}
