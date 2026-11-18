//! Static LED colour control for Razer mice and accessories.
//!
//! Ported from OpenRazer's USB protocol:
//! - `razerchromacommon.c`: `razer_chroma_extended_matrix_effect_static`,
//!   `razer_chroma_extended_matrix_brightness`
//! - `razeraccessory_driver.c`: the Mouse Dock Pro's mode-switch dance
//! - OpenRazer PR #2817: mouse LED commands relayed through the Mouse Dock Pro
//!
//! The same report layout is used by every function here:
//!
//! | Offset | Field             |
//! |--------|-------------------|
//! | 0      | Status            |
//! | 1      | Transaction ID    |
//! | 4      | Protocol type     |
//! | 5      | Data size         |
//! | 6      | Command class     |
//! | 7      | Command ID        |
//! | 8..    | Arguments         |
//! | 88     | CRC               |
//!
//! The dock listens on two transaction IDs: commands on `0x1F` are relayed
//! to the paired mouse, commands on `0xFF` configure the dock's own underglow.

use std::env;
use std::fs;
use std::path::PathBuf;

use hidapi::{HidApi, HidDevice};

use crate::error::{RazerError, Result};
use crate::protocol::{build_report, compute_crc, send_command};

pub const RAZER_VID: u16 = 0x1532;

pub const PID_MOUSE_WIRED: u16 = 0x00aa;
pub const PID_MOUSE_WIRELESS: u16 = 0x00ab;
pub const PID_DOCK_PRO: u16 = 0x00a4;

const CMD_CLASS_DEVICE: u8 = 0x00;
const CMD_CLASS_LED: u8 = 0x0f;

const CMD_SET_DEVICE_MODE: u8 = 0x04;
const CMD_SET_EFFECT: u8 = 0x02;
const CMD_GET_EFFECT: u8 = 0x82;
const CMD_SET_BRIGHTNESS: u8 = 0x04;
const CMD_GET_BRIGHTNESS: u8 = 0x84;

pub const VARSTORE: u8 = 0x01;
const ZERO_LED: u8 = 0x00;
const EFFECT_STATIC: u8 = 0x01;

/// Which device a colour should be written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Mouse,
    Dock,
}

impl Target {
    /// Short identifier used by CLI options and the saved-colour file.
    pub fn key(self) -> &'static str {
        match self {
            Target::Mouse => "mouse",
            Target::Dock => "dock",
        }
    }

    /// Human-readable name.
    pub fn label(self) -> &'static str {
        match self {
            Target::Mouse => "Mouse",
            Target::Dock => "Mouse Dock Pro",
        }
    }
}

/// A colour write for one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorRequest {
    pub target: Target,
    pub rgb: Rgb,
}

/// The stored static colour read back from a device.
#[derive(Debug)]
pub struct ColorReading {
    pub target: Target,
    /// Device the command was sent through.
    pub device: &'static str,
    /// `None` when the device is running a non-static effect.
    pub color: Option<Rgb>,
    /// The effect id reported by the device (`0x01` = static).
    pub effect: Option<u8>,
    /// Set when the device was absent, asleep or did not answer.
    pub error: Option<String>,
}

/// One command in an apply sequence.
#[derive(Debug)]
pub struct Step {
    pub label: &'static str,
    pub result: Result<()>,
}

/// What happened for one target.
#[derive(Debug)]
pub struct DeviceOutcome {
    pub target: Target,
    /// Human-readable device actually used, or why none was available.
    pub device: &'static str,
    /// Set when the device could not be opened / was not present.
    pub error: Option<String>,
    pub steps: Vec<Step>,
}

impl DeviceOutcome {
    pub fn ok(&self) -> bool {
        self.error.is_none() && self.steps.iter().all(|step| step.result.is_ok())
    }
}

/// Map a 0-100 percentage to the device's 0-255 brightness scale.
pub fn percent_to_brightness(percent: u8) -> u8 {
    ((percent.min(100) as u16 * 255) / 100) as u8
}

/// Transaction ID the Mouse Dock Pro relays to the paired mouse.
pub const TID_MOUSE: u8 = 0x1f;
/// Transaction ID that addresses the Mouse Dock Pro itself.
pub const TID_DOCK: u8 = 0xff;

/// The RGB colour written to a device.
pub type Rgb = (u8, u8, u8);

/// True when a device with this PID is currently present.
pub fn is_present(api: &HidApi, pid: u16) -> bool {
    api.device_list()
        .any(|d| d.vendor_id() == RAZER_VID && d.product_id() == pid)
}

/// Longest time to wait for a static colour to read back after a write.
const REAPPLY_VERIFY_ATTEMPTS: u32 = 4;

/// Rewrite a target's static colour until the device reports it back.
///
/// Re-applying is what makes the colour "stick" on this hardware: the mouse's
/// LED sits behind the Mouse Dock Pro while docked, and the dock re-asserts
/// its charging LED state on the link after a dock or charge transition, which
/// is visible as the LED snapping back to the colour it had while charging.
/// Neither the extended-matrix path (`0x0F / 0x82` on LED 0x03) nor the legacy
/// battery-LED commands (`0x03 / 0x01`, `0x03 / 0x10`) get an answer on this
/// dock (status `0x05` / timeout), so the only reliable fix is to write the
/// static colour again a few times until the read-back agrees.
///
/// Returns the colour that ended up on the device and the number of writes.
pub fn reapply(target: Target, rgb: Rgb, brightness: Option<u8>) -> (Option<Rgb>, u32) {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(_) => return (None, 0),
    };
    let dock_present = is_present(&api, PID_DOCK_PRO);
    let (device, pid) = match target {
        Target::Mouse => mouse_device(&api),
        Target::Dock => (
            "Mouse Dock Pro",
            if dock_present { PID_DOCK_PRO } else { 0 },
        ),
    };
    let _ = device;

    if pid == 0 {
        return (None, 0);
    }
    let Ok(hid) = open(&api, pid) else {
        return (None, 0);
    };

    let mut writes = 0;
    let mut last = None;
    for attempt in 0..REAPPLY_VERIFY_ATTEMPTS {
        let result = match target {
            Target::Mouse => set_mouse_color(&hid, rgb),
            Target::Dock => set_dock_color(&hid, rgb),
        };
        if result.is_err() {
            return (last, writes);
        }
        writes += 1;

        let read = match target {
            Target::Mouse => get_mouse_color(&hid),
            Target::Dock => get_dock_color(&hid),
        };
        match read {
            Ok((EFFECT_STATIC, Some(color))) => {
                last = Some(color);
                if color == rgb {
                    break;
                }
            }
            _ => {
                if attempt == 0 && brightness.is_some() {
                    // Brightness does not change the colour, so a failed
                    // read-back here is not worth retrying on its own.
                    break;
                }
            }
        }
    }

    // Brightness is separate from the colour; send it once the colour settles.
    if let Some(value) = brightness {
        let result = match target {
            Target::Mouse => set_mouse_brightness(&hid, value),
            Target::Dock => set_dock_brightness(&hid, value),
        };
        if result.is_err() {
            return (last, writes);
        }
        writes += 1;
    }

    if last.is_some() {
        save_color(target, rgb);
    }
    (last, writes)
}

/// Pick the device the mouse should be addressed through.
///
/// A cable or the mouse's own dongle is preferred; when the mouse is docked it
/// disappears from its own PIDs and the Mouse Dock Pro relays the commands.
pub fn mouse_device(api: &HidApi) -> (&'static str, u16) {
    if is_present(api, PID_MOUSE_WIRED) {
        ("Basilisk V3 Pro (wired)", PID_MOUSE_WIRED)
    } else if is_present(api, PID_MOUSE_WIRELESS) {
        ("Basilisk V3 Pro (wireless dongle)", PID_MOUSE_WIRELESS)
    } else if is_present(api, PID_DOCK_PRO) {
        ("Basilisk V3 Pro (through Mouse Dock Pro)", PID_DOCK_PRO)
    } else {
        ("Basilisk V3 Pro (not present)", 0)
    }
}

/// Open the Razer control interface for a PID.
///
/// Mirrors the interface selection used by the tray app: interface 0 with
/// the mouse usage page/usage, falling back to interface 0 of any usage.
pub fn open(api: &HidApi, pid: u16) -> Result<HidDevice> {
    let mut interface_zero = None;
    let mut usage_match = None;

    for info in api.device_list() {
        if info.vendor_id() != RAZER_VID || info.product_id() != pid {
            continue;
        }
        if info.usage_page() == 0x0001 && info.usage() == 0x0002 {
            if info.interface_number() == 0 {
                return Ok(info.open_device(api)?);
            }
            usage_match.get_or_insert(info);
        }
        if info.interface_number() == 0 {
            interface_zero.get_or_insert(info);
        }
    }

    if let Some(info) = usage_match.or(interface_zero) {
        return Ok(info.open_device(api)?);
    }

    Err(RazerError::DeviceNotFound { vid: RAZER_VID, pid })
}

/// Recompute the CRC after the argument bytes have been written.
fn finish(report: &mut [u8; crate::protocol::REPORT_LEN]) {
    report[88] = compute_crc(report);
}

fn static_effect(hid: &HidDevice, tid: u8, rgb: Rgb) -> Result<()> {
    let (r, g, b) = rgb;
    let mut report = build_report(tid, CMD_CLASS_LED, CMD_SET_EFFECT, 0x09);
    // OpenRazer layout: varstore, led, effect, 0, 0, 0x01, R, G, B
    report[8..17].copy_from_slice(&[
        VARSTORE,
        ZERO_LED,
        EFFECT_STATIC,
        0,
        0,
        0x01,
        r,
        g,
        b,
    ]);
    finish(&mut report);
    send_command(hid, &report)?;
    Ok(())
}

fn brightness_report(hid: &HidDevice, tid: u8, value: u8) -> Result<()> {
    let mut report = build_report(tid, CMD_CLASS_LED, CMD_SET_BRIGHTNESS, 0x03);
    report[8..11].copy_from_slice(&[VARSTORE, ZERO_LED, value]);
    finish(&mut report);
    send_command(hid, &report)?;
    Ok(())
}

/// Set a static colour on a Basilisk V3 Pro (wired or wireless dongle).
pub fn set_mouse_color(hid: &HidDevice, rgb: Rgb) -> Result<()> {
    static_effect(hid, TID_MOUSE, rgb)
}

/// Set the mouse's LED brightness (0-255).
pub fn set_mouse_brightness(hid: &HidDevice, value: u8) -> Result<()> {
    brightness_report(hid, TID_MOUSE, value)
}

/// Set a static colour on the Mouse Dock Pro's own underglow.
///
/// The dock needs the same sequence as OpenRazer: enter normal mode, write
/// the colour, send the "commit" report, then write the colour again. Without
/// the commit step every later colour change is applied one step late.
pub fn set_dock_color(hid: &HidDevice, rgb: Rgb) -> Result<()> {
    let mut mode = build_report(TID_DOCK, CMD_CLASS_DEVICE, CMD_SET_DEVICE_MODE, 0x02);
    finish(&mut mode);
    send_command(hid, &mode)?;

    static_effect(hid, TID_DOCK, rgb)?;

    let mut commit = build_report(TID_MOUSE, CMD_CLASS_LED, CMD_SET_EFFECT, 0x06);
    commit[10] = 0x08;
    finish(&mut commit);
    send_command(hid, &commit)?;

    static_effect(hid, TID_DOCK, rgb)
}

/// Set the dock's underglow brightness (0-255).
pub fn set_dock_brightness(hid: &HidDevice, value: u8) -> Result<()> {
    brightness_report(hid, TID_DOCK, value)
}

/// Read the mouse's current LED brightness (0-255).
pub fn get_mouse_brightness(hid: &HidDevice) -> Result<u8> {
    get_brightness(hid, TID_MOUSE)
}

/// Read the dock's current underglow brightness (0-255).
pub fn get_dock_brightness(hid: &HidDevice) -> Result<u8> {
    get_brightness(hid, TID_DOCK)
}

fn get_brightness(hid: &HidDevice, tid: u8) -> Result<u8> {
    let mut report = build_report(tid, CMD_CLASS_LED, CMD_GET_BRIGHTNESS, 0x03);
    report[8..10].copy_from_slice(&[VARSTORE, ZERO_LED]);
    finish(&mut report);
    let response = send_command(hid, &report)?;
    Ok(response[10])
}

/// Read the current brightness (0-255) of a target.
///
/// Uses the same device selection as `apply`: the mouse is addressed directly
/// when plugged in or on its own dongle, and through the dock's relay when
/// dock-paired. The dock's own underglow uses its own transaction id.
pub fn read_brightness(target: Target) -> Option<u8> {
    let api = HidApi::new().ok()?;
    let dock_present = is_present(&api, PID_DOCK_PRO);
    let (device, pid) = match target {
        Target::Mouse => mouse_device(&api),
        Target::Dock => (
            "Mouse Dock Pro",
            if dock_present { PID_DOCK_PRO } else { 0 },
        ),
    };
    let _ = device;

    if pid == 0 {
        return None;
    }
    let hid = open(&api, pid).ok()?;
    match target {
        Target::Mouse => get_mouse_brightness(&hid).ok(),
        Target::Dock => get_dock_brightness(&hid).ok(),
    }
}

/// Parse a `RRGGBB` or `#RRGGBB` string.
pub fn parse_hex_color(value: &str) -> Option<Rgb> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    Some((
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ))
}

/// Read the effect id and, for a static effect, its colour.
fn get_effect(hid: &HidDevice, tid: u8) -> Result<(u8, Option<Rgb>)> {
    let mut report = build_report(tid, CMD_CLASS_LED, CMD_GET_EFFECT, 0x03);
    report[8..11].copy_from_slice(&[VARSTORE, ZERO_LED, 0x00]);
    finish(&mut report);
    let response = send_command(hid, &report)?;

    let effect = response[10];
    // Static replies mirror the write layout: varstore, led, effect, 0, 0,
    // 0x01, R, G, B. Other effects (the built-in charging wave, for example)
    // carry no static colour.
    let color = if effect == EFFECT_STATIC && response[13] == 0x01 {
        Some((response[14], response[15], response[16]))
    } else {
        None
    };
    Ok((effect, color))
}

/// Read the mouse's effect id and stored static colour.
pub fn get_mouse_color(hid: &HidDevice) -> Result<(u8, Option<Rgb>)> {
    get_effect(hid, TID_MOUSE)
}

/// Read the dock's effect id and stored static colour.
pub fn get_dock_color(hid: &HidDevice) -> Result<(u8, Option<Rgb>)> {
    get_effect(hid, TID_DOCK)
}

/// Read the stored static colour of every requested target.
///
/// Devices answer this even while a transient charging effect is running, so
/// the value the user last picked is normally returned. A device that is
/// asleep or absent reports an error instead.
pub fn read_colors(targets: &[Target]) -> Vec<ColorReading> {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            return targets
                .iter()
                .map(|&target| ColorReading {
                    target,
                    device: "HID",
                    color: None,
                    effect: None,
                    error: Some(e.to_string()),
                })
                .collect();
        }
    };

    let dock_present = is_present(&api, PID_DOCK_PRO);
    let mut readings = Vec::new();

    for &target in targets {
        let (device, pid) = match target {
            Target::Mouse => mouse_device(&api),
            Target::Dock => (
                "Mouse Dock Pro",
                if dock_present { PID_DOCK_PRO } else { 0 },
            ),
        };

        let mut reading = ColorReading {
            target,
            device,
            color: None,
            effect: None,
            error: None,
        };

        if pid == 0 {
            reading.error = Some("not present".to_string());
            readings.push(reading);
            continue;
        }

        match open(&api, pid) {
            Ok(hid) => {
                let result = match target {
                    Target::Mouse => get_mouse_color(&hid),
                    Target::Dock => get_dock_color(&hid),
                };
                match result {
                    Ok((effect, color)) => {
                        reading.effect = Some(effect);
                        reading.color = color;
                    }
                    Err(e) => reading.error = Some(e.to_string()),
                }
            }
            Err(e) => reading.error = Some(e.to_string()),
        }

        readings.push(reading);
    }

    readings
}

fn config_path() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA").map(|base| {
        PathBuf::from(base)
            .join("BasiliskPower")
            .join("led-colors.txt")
    })
}

/// The last colour successfully written to a target.
///
/// This is the fallback when a device is asleep, undocked or running a
/// transient effect, so the picker still opens on the colour the user chose.
pub fn saved_color(target: Target) -> Option<Rgb> {
    let text = fs::read_to_string(config_path()?).ok()?;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == target.key() {
            return parse_hex_color(value);
        }
    }
    None
}

/// Remember a colour after it has been written successfully.
pub fn save_color(target: Target, rgb: Rgb) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }

    let mut lines: Vec<String> = fs::read_to_string(&path)
        .ok()
        .map(|text| {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let prefix = format!("{}=", target.key());
    let value = format!("{prefix}{:02X}{:02X}{:02X}", rgb.0, rgb.1, rgb.2);
    if let Some(line) = lines.iter_mut().find(|line| line.starts_with(&prefix)) {
        *line = value;
    } else {
        lines.push(value);
    }

    let mut text = lines.join("\n");
    text.push('\n');
    let _ = fs::write(path, text);
}

/// Pick the colour to show for a reading: live value first, then the last
/// colour we saved. Returns the colour and where it came from.
pub fn effective_color(reading: &ColorReading) -> Option<(Rgb, &'static str)> {
    if let Some(rgb) = reading.color {
        return Some((rgb, "device"));
    }
    saved_color(reading.target).map(|rgb| (rgb, "saved"))
}

/// Apply a static colour to each requested target and optionally set
/// brightness (0-255) on every device written.
///
/// The mouse is addressed directly when it is plugged in or its own wireless
/// dongle is present; otherwise the Mouse Dock Pro relays the same commands
/// to the docked mouse. Dock colours always go to the dock's own underglow.
/// Colours that were written successfully are remembered for the next launch.
pub fn apply(requests: &[ColorRequest], brightness: Option<u8>) -> Vec<DeviceOutcome> {
    apply_inner(requests, brightness, true)
}

/// Write one static colour without updating the saved-colour file.
///
/// This is the live-preview path used while the colour wheel is dragged.
/// Persisting every intermediate colour would rewrite `led-colors.txt` dozens
/// of times per second, so only the final resting colour is saved by the
/// caller once the drag settles.
pub fn preview(request: ColorRequest, brightness: Option<u8>) -> DeviceOutcome {
    apply_inner(&[request], brightness, false)
        .into_iter()
        .next()
        .unwrap_or(DeviceOutcome {
            target: request.target,
            device: "HID",
            error: Some("no device outcome".to_string()),
            steps: vec![],
        })
}

fn apply_inner(
    requests: &[ColorRequest],
    brightness: Option<u8>,
    remember: bool,
) -> Vec<DeviceOutcome> {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            return requests
                .iter()
                .map(|request| DeviceOutcome {
                    target: request.target,
                    device: "HID",
                    error: Some(e.to_string()),
                    steps: vec![],
                })
                .collect();
        }
    };

    let dock_present = is_present(&api, PID_DOCK_PRO);
    let mut outcomes = Vec::new();

    for request in requests {
        let (device, pid) = match request.target {
            Target::Mouse => mouse_device(&api),
            Target::Dock => (
                "Mouse Dock Pro",
                if dock_present { PID_DOCK_PRO } else { 0 },
            ),
        };

        let mut outcome = DeviceOutcome {
            target: request.target,
            device,
            error: None,
            steps: vec![],
        };

        if pid == 0 {
            outcome.error = Some("not present".to_string());
            outcomes.push(outcome);
            continue;
        }

        match open(&api, pid) {
            Ok(hid) => {
                let (label, result) = match request.target {
                    Target::Mouse => (
                        "static colour",
                        set_mouse_color(&hid, request.rgb),
                    ),
                    Target::Dock => (
                        "static colour + mode switch",
                        set_dock_color(&hid, request.rgb),
                    ),
                };
                outcome.steps.push(Step { label, result });

                if let Some(value) = brightness {
                    let result = match request.target {
                        Target::Mouse => set_mouse_brightness(&hid, value),
                        Target::Dock => set_dock_brightness(&hid, value),
                    };
                    outcome.steps.push(Step {
                        label: "brightness",
                        result,
                    });
                }

                if outcome.ok() && remember {
                    save_color(request.target, request.rgb);
                }
            }
            Err(e) => outcome.error = Some(e.to_string()),
        }

        outcomes.push(outcome);
    }

    outcomes
}
