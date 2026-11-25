//! Low-level Razer HID protocol implementation.
//!
//! Razer wireless devices communicate via 90-byte HID reports:
//!
//! | Offset | Field             | Size |
//! |--------|-------------------|------|
//! | 0      | Status            | 1    |
//! | 1      | Transaction ID    | 1    |
//! | 2..3   | Remaining packets | 2    |
//! | 4      | Protocol type     | 1    |
//! | 5      | Data size         | 1    |
//! | 6      | Command class     | 1    |
//! | 7      | Command ID        | 1    |
//! | 8..87  | Arguments         | 80   |
//! | 88     | CRC               | 1    |
//! | 89     | Reserved          | 1    |
//!
//! ## Windows vs Linux/macOS
//!
//! On Windows, Razer devices often don't expose proper HID feature report
//! descriptors, causing `send_feature_report`/`get_feature_report` to fail
//! with error 0x00000057 ("The parameter is incorrect").
//!
//! The workaround is to use **output reports** (`hid_write`) to send commands
//! and **input reports** (`hid_read`) to receive responses. The buffer must
//! be 91 bytes: 1 byte report ID (0x00) + 90 bytes payload.

use crate::error::{RazerError, Result};
use log::debug;
use std::thread;
use std::time::Duration;

/// Razer report payload length (without report ID).
pub const REPORT_LEN: usize = 90;

/// Buffer length for Windows (report ID + payload).
const WIRE_LEN: usize = REPORT_LEN + 1;

/// Command class for power/battery queries.
pub const CMD_CLASS_POWER: u8 = 0x07;

/// Get battery level (response arg[1] = 0..255).
pub const CMD_GET_BATTERY: u8 = 0x80;

/// Get charging status (response arg[1] = 0 or 1).
pub const CMD_IS_CHARGING: u8 = 0x84;

/// Response status: command succeeded.
const STATUS_OK: u8 = 0x02;

/// Response status: device busy (legacy 0x03, current-firmware 0x01).
const STATUS_BUSY: u8 = 0x03;

/// Delay between send and read (milliseconds).
const IO_DELAY_MS: u64 = 100;

/// Maximum retry attempts for busy/timeout responses.
const MAX_RETRIES: u8 = 5;

/// Read timeout for hid_read (milliseconds).
const READ_TIMEOUT_MS: i32 = 1000;

/// Build a 90-byte Razer HID report payload.
pub fn build_report(
    transaction_id: u8,
    cmd_class: u8,
    cmd_id: u8,
    data_size: u8,
) -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = 0x00; // status: new
    buf[1] = transaction_id;
    buf[4] = 0x00; // protocol type
    buf[5] = data_size;
    buf[6] = cmd_class;
    buf[7] = cmd_id;
    buf[88] = compute_crc(&buf);
    buf
}

/// XOR of bytes 2..=87 (the CRC covers most of the report).
pub fn compute_crc(buf: &[u8; REPORT_LEN]) -> u8 {
    buf[2..88].iter().fold(0u8, |acc, &b| acc ^ b)
}

/// Validate the CRC byte in a response.
pub fn validate_crc(buf: &[u8; REPORT_LEN]) -> Result<()> {
    let expected = compute_crc(buf);
    if expected != buf[88] {
        return Err(RazerError::CrcMismatch {
            expected,
            got: buf[88],
        });
    }
    Ok(())
}

/// Send a command and read the response.
///
/// Tries feature reports first on all platforms, falls back to write/read.
pub fn send_command(
    hid: &hidapi::HidDevice,
    report: &[u8; REPORT_LEN],
) -> Result<[u8; REPORT_LEN]> {
    for attempt in 0..MAX_RETRIES {
        debug!(
            "Attempt {}/{}: sending command {:#04x}/{:#04x}",
            attempt + 1,
            MAX_RETRIES,
            report[6],
            report[7]
        );

        // Try to send and receive
        let resp = send_and_receive(hid, report)?;

        match resp[0] {
            STATUS_OK => {
                validate_crc(&resp)?;
                debug!("Got OK response");
                return Ok(resp);
            }
            // Device busy: legacy firmware reports 0x03, current firmware 0x01
            // (per OpenRazer razercommon.h). Wait and retry.
            STATUS_BUSY | 0x01 => {
                debug!(
                    "Device busy (status {:#04x}), waiting before retry...",
                    resp[0]
                );
                thread::sleep(Duration::from_millis(300));
                continue;
            }
            0x00 => {
                // Some devices return 0x00 status initially, retry
                debug!("Got status 0x00, retrying...");
                thread::sleep(Duration::from_millis(200));
                continue;
            }
            status => {
                debug!("Unexpected status: {:#04x}", status);
                // On some retries, the device might not have processed yet
                if attempt < MAX_RETRIES - 1 {
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
                return Err(RazerError::InvalidResponse { status });
            }
        }
    }
    Err(RazerError::Timeout {
        attempts: MAX_RETRIES,
    })
}

/// Send and receive a command, trying feature reports first and falling back
/// to write/read if they fail (works on all platforms).
fn send_and_receive(
    hid: &hidapi::HidDevice,
    report: &[u8; REPORT_LEN],
) -> Result<[u8; REPORT_LEN]> {
    match send_via_feature_report(hid, report) {
        Ok(resp) => Ok(resp),
        Err(e) => {
            debug!("Feature report failed ({e}), falling back to write/read");
            send_via_write_read(hid, report)
        }
    }
}

/// Send via HID feature reports (SET_REPORT / GET_REPORT control transfers).
fn send_via_feature_report(
    hid: &hidapi::HidDevice,
    report: &[u8; REPORT_LEN],
) -> Result<[u8; REPORT_LEN]> {
    // hidapi send_feature_report: first byte is report ID
    let mut send_buf = [0u8; WIRE_LEN];
    send_buf[0] = 0x00; // report ID
    send_buf[1..].copy_from_slice(report);

    hid.send_feature_report(&send_buf)?;
    thread::sleep(Duration::from_millis(IO_DELAY_MS));

    // hidapi get_feature_report: first byte is report ID, filled on return
    let mut recv_buf = [0u8; WIRE_LEN];
    recv_buf[0] = 0x00; // report ID to request
    let n = hid.get_feature_report(&mut recv_buf)?;

    if n < REPORT_LEN {
        debug!("Feature report returned only {} bytes", n);
        return Err(RazerError::NoData);
    }

    let mut resp = [0u8; REPORT_LEN];
    // Response payload starts at byte 1 (after report ID)
    resp.copy_from_slice(&recv_buf[1..WIRE_LEN]);
    Ok(resp)
}

/// Send via output report (hid_write) and receive via input report (hid_read).
/// This is the reliable method on Windows where feature reports often fail.
///
/// The write buffer is 91 bytes: [report_id=0x00, ...90 bytes payload].
/// The read returns up to 90 bytes of payload (hidapi strips report ID on read).
fn send_via_write_read(
    hid: &hidapi::HidDevice,
    report: &[u8; REPORT_LEN],
) -> Result<[u8; REPORT_LEN]> {
    // Build write buffer: report ID 0x00 + 90 bytes payload
    let mut send_buf = [0u8; WIRE_LEN];
    send_buf[0] = 0x00; // report ID
    send_buf[1..].copy_from_slice(report);

    let written = hid.write(&send_buf)?;
    debug!("hid_write sent {} bytes", written);

    thread::sleep(Duration::from_millis(IO_DELAY_MS));

    // Read response — try with WIRE_LEN buffer to handle both cases
    // (some backends include report ID in response, some don't)
    let mut recv_buf = [0u8; WIRE_LEN];
    let n = hid.read_timeout(&mut recv_buf, READ_TIMEOUT_MS)?;

    debug!("hid_read got {} bytes", n);

    if n == 0 {
        return Err(RazerError::NoData);
    }

    let mut resp = [0u8; REPORT_LEN];

    if n >= WIRE_LEN {
        // Got report ID + full payload
        resp.copy_from_slice(&recv_buf[1..WIRE_LEN]);
    } else if n >= REPORT_LEN {
        // Got full payload (report ID was stripped or not present)
        resp.copy_from_slice(&recv_buf[..REPORT_LEN]);
    } else if n > 1 {
        // Partial response — copy what we have
        resp[..n].copy_from_slice(&recv_buf[..n]);
    } else {
        return Err(RazerError::NoData);
    }

    Ok(resp)
}

/// Query battery level, returns 0–100%.
pub fn get_battery_level(hid: &hidapi::HidDevice, tid: u8) -> Result<u8> {
    let report = build_report(tid, CMD_CLASS_POWER, CMD_GET_BATTERY, 0x02);
    let resp = send_command(hid, &report)?;
    let raw = resp[9] as u16;
    Ok(((raw * 100) / 255).min(100) as u8)
}

/// Query charging status.
pub fn get_charging_status(hid: &hidapi::HidDevice, tid: u8) -> Result<bool> {
    let report = build_report(tid, CMD_CLASS_POWER, CMD_IS_CHARGING, 0x02);
    let resp = send_command(hid, &report)?;
    Ok(resp[9] != 0)
}

/// Query the Mouse Dock Pro's RF link heartbeat.
///
/// This is a dock-firmware round-trip (`00:bf`, transaction `0x3F`) that does
/// not touch the RF relay. `arguments[1]` is 1 when a paired mouse is live and
/// 0 when the dock has no link (mouse asleep, off, or out of range). The tray
/// uses it to skip battery reads that would otherwise spend seconds timing out
/// on a sleeping mouse.
pub fn get_dock_link_state(hid: &hidapi::HidDevice) -> Result<bool> {
    let report = build_report(0x3F, 0x00, 0xBF, 0x1F);
    let resp = send_command(hid, &report)?;
    if resp[5] < 2 {
        return Err(RazerError::NoData);
    }
    Ok(resp[9] == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_structure() {
        let r = build_report(0x1f, CMD_CLASS_POWER, CMD_GET_BATTERY, 0x02);
        assert_eq!(r[0], 0x00); // status
        assert_eq!(r[1], 0x1f); // transaction_id
        assert_eq!(r[5], 0x02); // data_size
        assert_eq!(r[6], 0x07); // command_class
        assert_eq!(r[7], 0x80); // command_id
        assert_eq!(r[88], compute_crc(&r));
        assert_eq!(r[89], 0x00); // reserved
    }

    #[test]
    fn crc_consistency() {
        let r = build_report(0x3f, CMD_CLASS_POWER, CMD_IS_CHARGING, 0x02);
        assert_eq!(r[88], compute_crc(&r));
    }

    #[test]
    fn battery_pct_conversion() {
        assert_eq!(((255u16 * 100) / 255).min(100) as u8, 100);
        assert_eq!(((128u16 * 100) / 255).min(100) as u8, 50);
        assert_eq!(((0u16 * 100) / 255).min(100) as u8, 0);
    }
}
