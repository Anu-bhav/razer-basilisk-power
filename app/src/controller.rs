use hidapi::{HidApi, HidDevice};
use log::debug;
use std::ffi::CString;
use std::thread;
use std::time::Duration;

use crate::devices::RAZER_DEVICE_LIST;

/// Keep the whole read bounded: the worker also watches for device
/// re-enumeration, and a 10 × 500 ms retry storm used to block it long
/// enough to miss a dock replug. Five tries matches the shared protocol
/// implementation and still rides out a busy RF link.
const MAX_TRIES_SEND: u8 = 5;
const TIME_BETWEEN_SEND: Duration = Duration::from_millis(250);
/// The Mouse Dock Pro relay usually answers within a few tens of ms once the
/// RF link to the mouse is ready, so echo mismatches are retried on a shorter
/// gap than busy/timeout replies.
const TIME_BETWEEN_RETRIES: Duration = Duration::from_millis(120);

pub struct RazerReport {
    pub status: u8,
    pub transaction_id: u8,
    pub remaining_packets: u16,
    pub protocol_type: u8,
    pub data_size: u8,
    pub command_class: u8,
    pub command_id: u8,
    pub arguments: [u8; 80],
    pub crc: u8,
    pub reserved: u8,
}

impl RazerReport {
    pub const STATUS_NEW_COMMAND: u8 = 0x00;
    pub const STATUS_BUSY: u8 = 0x01;
    pub const STATUS_SUCCESSFUL: u8 = 0x02;
    pub const STATUS_FAILURE: u8 = 0x03;
    pub const STATUS_NO_RESPONSE: u8 = 0x04;
    pub const STATUS_NOT_SUPPORTED: u8 = 0x05;

    pub fn new() -> Self {
        RazerReport {
            status: 0,
            transaction_id: 0,
            remaining_packets: 0,
            protocol_type: 0,
            data_size: 0,
            command_class: 0,
            command_id: 0,
            arguments: [0; 80],
            crc: 0,
            reserved: 0,
        }
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() != 90 {
            return Err("Expected 90 bytes of data as razer report");
        }

        let mut report = RazerReport::new();
        report.status = data[0];
        report.transaction_id = data[1];
        report.remaining_packets = u16::from_be_bytes([data[2], data[3]]);
        report.protocol_type = data[4];
        report.data_size = data[5];
        report.command_class = data[6];
        report.command_id = data[7];
        report.arguments.copy_from_slice(&data[8..88]);
        report.crc = data[88];
        report.reserved = data[89];

        Ok(report)
    }

    pub fn pack(&self) -> Vec<u8> {
        let mut data = vec![
            self.status,
            self.transaction_id,
            (self.remaining_packets >> 8) as u8,
            (self.remaining_packets & 0xFF) as u8,
            self.protocol_type,
            self.data_size,
            self.command_class,
            self.command_id,
        ];
        data.extend_from_slice(&self.arguments);
        data.push(self.crc);
        data.push(self.reserved);
        data
    }

    pub fn calculate_crc(&self) -> u8 {
        let data = self.pack();
        data[2..88].iter().fold(0, |crc, &byte| crc ^ byte)
    }

    pub fn is_valid(&self) -> bool {
        self.calculate_crc() == self.crc
    }
}

/// Dock link heartbeat request.
///
/// Transaction `0x3F` addresses the dock firmware itself; `0x1F` is the RF
/// relay channel used for battery/charging commands.
fn dock_link_request() -> RazerReport {
    let mut request = RazerReport::new();
    request.status = RazerReport::STATUS_NEW_COMMAND;
    request.transaction_id = 0x3F;
    request.data_size = 0x1F;
    request.command_class = 0x00;
    request.command_id = 0xBF;
    request
}

/// `arguments[1]` is the dock's availability flag: 1 = mouse link live.
fn link_state_from_report(response: &RazerReport) -> Option<bool> {
    (response.data_size >= 2).then(|| response.arguments[1] == 1)
}

#[derive(Debug)]
pub struct DeviceController {
    pub handle: HidDevice,
    pub name: String,
    pub pid: u16,
    pub report_id: u8,
    pub transaction_id: u8,
    pub swappable_battery: bool,
}

impl DeviceController {
    pub fn new(
        api: &HidApi,
        name: String,
        pid: u16,
        path: String,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let c_path = CString::new(path)?;
        let handle = api.open_path(c_path.as_ref())?;

        let device_info = RAZER_DEVICE_LIST.iter().find(|device| device.pid == pid);

        let transaction_id = device_info.map_or(0x3F, |device| device.transaction_id());
        let swappable_battery = device_info.is_some_and(|device| device.swappable_battery);

        Ok(DeviceController {
            handle,
            name,
            pid,
            report_id: 0x00,
            transaction_id,
            swappable_battery,
        })
    }

    pub fn get_battery_level(&self) -> Result<i32, Box<dyn std::error::Error>> {
        let request = self.create_command(0x07, 0x80, 0x02);
        let response = self.send_payload(request)?;
        let battery_level = (response.arguments[1] as f32 / 255.0) * 100.0;
        Ok(battery_level.round() as i32)
    }

    pub fn get_charging_status(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let request = self.create_command(0x07, 0x84, 0x02);
        let response = self.send_payload(request)?;
        let charging_status = response.arguments[1] != 0;
        Ok(charging_status)
    }

    /// Single-attempt charging read for the fast undock probe.
    ///
    /// The full send path can spend seconds retrying a sleeping relay. The
    /// fast loop runs every few seconds anyway, so one attempt is enough:
    /// a busy relay is simply asked again on the next tick.
    pub fn probe_charging_status(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let request = self.create_command(0x07, 0x84, 0x02);
        let response = self.send_payload_with_tries(request, 1)?;
        let charging_status = response.arguments[1] != 0;
        Ok(charging_status)
    }

    /// Ask the Mouse Dock Pro whether its RF link to the paired mouse is up.
    ///
    /// This is a dock-firmware round-trip (`00:bf`, transaction `0x3F`) that
    /// does not touch the RF relay, so it answers in ~120 ms even while the
    /// mouse sleeps. The tray uses it to avoid battery reads that would spend
    /// seconds timing out on a sleeping mouse.
    pub fn dock_link_state(&self) -> Result<bool, Box<dyn std::error::Error>> {
        // Two tries ride out a single busy reply without turning the fast
        // pre-check into another multi-second stall.
        let response = self.send_payload_with_tries(dock_link_request(), 2)?;
        link_state_from_report(&response)
            .ok_or_else(|| "Dock link heartbeat returned no state".into())
    }

    pub fn send_payload(
        &self,
        request: RazerReport,
    ) -> Result<RazerReport, Box<dyn std::error::Error>> {
        self.send_payload_with_tries(request, MAX_TRIES_SEND)
    }

    fn send_payload_with_tries(
        &self,
        mut request: RazerReport,
        max_tries: u8,
    ) -> Result<RazerReport, Box<dyn std::error::Error>> {
        request.crc = request.calculate_crc();

        for attempt in 0..max_tries {
            self.usb_send(&request)?;
            let response = self.usb_receive()?;

            // The Mouse Dock Pro relays commands to the paired mouse over
            // RF. While the link is busy, the feature read returns a stale
            // or empty report whose echo doesn't match the request. Resend
            // and read again instead of giving up — the OpenRazer kernel
            // driver retries the whole send+read cycle the same way.
            if response.remaining_packets != request.remaining_packets
                || response.command_class != request.command_class
                || response.command_id != request.command_id
            {
                // Routine on the dock relay (stale echo while the RF link is
                // busy). A command that exhausts its tries still returns Err,
                // which the manager logs once per read.
                debug!(
                    "Response doesn't match request (status {:#04x}), retrying",
                    response.status
                );
                if attempt + 1 < max_tries {
                    thread::sleep(TIME_BETWEEN_RETRIES);
                }
                continue;
            }

            match response.status {
                RazerReport::STATUS_SUCCESSFUL => return Ok(response),
                RazerReport::STATUS_BUSY => debug!("Device is busy"),
                RazerReport::STATUS_NO_RESPONSE => debug!("Command timed out"),
                RazerReport::STATUS_NOT_SUPPORTED => return Err("Command not supported".into()),
                RazerReport::STATUS_FAILURE => return Err("Command failed".into()),
                _ => return Err("Error unknown report status".into()),
            }

            if attempt + 1 == max_tries {
                break;
            }
            thread::sleep(TIME_BETWEEN_SEND);
            debug!("Trying to resend command");
        }

        Err(format!("Abort command (tries: {max_tries})").into())
    }

    pub fn create_command(&self, command_class: u8, command_id: u8, data_size: u8) -> RazerReport {
        let mut report = RazerReport::new();
        report.status = RazerReport::STATUS_NEW_COMMAND;
        report.transaction_id = self.transaction_id;
        report.command_class = command_class;
        report.command_id = command_id;
        report.data_size = data_size;
        report
    }

    pub fn usb_send(&self, report: &RazerReport) -> Result<(), Box<dyn std::error::Error>> {
        let mut data = vec![self.report_id];
        data.extend_from_slice(&report.pack());
        self.handle.send_feature_report(&data)?;
        thread::sleep(Duration::from_millis(60));
        Ok(())
    }

    pub fn usb_receive(&self) -> Result<RazerReport, Box<dyn std::error::Error>> {
        let expected_length = 91;
        let mut buf = vec![0u8; expected_length];
        let bytes_read = self.handle.get_feature_report(&mut buf)?;

        if bytes_read != expected_length {
            return Err("Error while getting feature report".into());
        }

        let report = RazerReport::from_bytes(&buf[1..])?;
        if !report.is_valid() {
            return Err("Get report has no valid crc".into());
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_link_request_targets_the_dock_firmware() {
        let mut request = dock_link_request();
        assert_eq!(request.status, RazerReport::STATUS_NEW_COMMAND);
        assert_eq!(request.transaction_id, 0x3F);
        assert_eq!(request.command_class, 0x00);
        assert_eq!(request.command_id, 0xBF);
        assert_eq!(request.data_size, 0x1F);

        request.crc = request.calculate_crc();
        assert!(request.is_valid());
    }

    #[test]
    fn dock_link_state_reads_the_availability_flag() {
        let mut report = RazerReport::new();
        report.data_size = 2;
        report.arguments[1] = 1;
        assert_eq!(link_state_from_report(&report), Some(true));

        report.arguments[1] = 0;
        assert_eq!(link_state_from_report(&report), Some(false));
    }

    #[test]
    fn dock_link_state_rejects_short_responses() {
        let mut report = RazerReport::new();
        report.data_size = 1;
        assert_eq!(link_state_from_report(&report), None);
    }
}
