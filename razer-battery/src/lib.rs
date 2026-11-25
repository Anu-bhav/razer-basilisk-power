//! # razer_battery
//!
//! A cross-platform Rust library for reading battery level and charging status
//! from wireless Razer devices via HID.
//!
//! Based on the protocol reverse-engineered by the OpenRazer project.
//! Works on **Windows**, **Linux**, and **macOS** using the `hidapi` crate —
//! no Razer Synapse or OpenRazer daemon required.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use razer_battery::{scan_all, find_devices, get_battery_info};
//!
//! fn main() -> razer_battery::error::Result<()> {
//!     for (dev, info) in scan_all()? {
//!         println!("{}: {}% (charging: {})", dev.name, info.level, info.charging);
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Platform notes
//!
//! **Windows**: Uses output/input reports (`hid_write`/`hid_read`) instead of
//! feature reports, because many Razer devices don't declare feature reports in
//! their HID descriptor on Windows, causing "parameter is incorrect" errors.
//! Close Razer Synapse before using this library — it claims exclusive HID access.
//!
//! **Linux**: Uses feature reports via hidraw. You may need a udev rule.
//!
//! **macOS**: Uses feature reports via IOKit. You **must** grant Input Monitoring
//! permission to your terminal app (System Settings → Privacy & Security →
//! Input Monitoring → add Terminal / iTerm2 / your IDE). Without this, all HID
//! device opens will fail with "not permitted".

pub mod devices;
pub mod error;
pub mod lighting;
pub mod protocol;

use hidapi::HidApi;
use log::debug;

pub use devices::{KNOWN_DEVICES, RAZER_VID, RazerDevice};
pub use error::RazerError;

/// Battery information.
#[derive(Debug, Clone, Copy)]
pub struct BatteryInfo {
    /// Battery percentage 0–100.
    pub level: u8,
    /// Whether the device is currently charging.
    pub charging: bool,
}

/// Open the correct HID interface for a device.
///
/// Matching priority:
/// 1. VID + PID + interface + usage_page + usage (most specific)
/// 2. VID + PID + interface only (fallback for platforms that don't report usage)
///
/// On macOS, opens in non-exclusive mode to avoid privilege violations on
/// mouse/keyboard HID devices claimed by the OS.
fn open_device(api: &HidApi, dev: &RazerDevice) -> error::Result<hidapi::HidDevice> {
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);
    // Log all matching HID entries for debugging
    for info in api.device_list() {
        if info.vendor_id() == RAZER_VID && info.product_id() == dev.pid {
            debug!(
                "  HID entry: iface={} usage_page={:#06x} usage={:#06x} path={:?}",
                info.interface_number(),
                info.usage_page(),
                info.usage(),
                info.path()
            );
        }
    }

    // First pass: full match
    for info in api.device_list() {
        if info.vendor_id() == RAZER_VID
            && info.product_id() == dev.pid
            && info.interface_number() == dev.interface
            && info.usage_page() == dev.usage_page
            && info.usage() == dev.usage
        {
            debug!(
                "Matched {} by full descriptor (iface={})",
                dev.name, dev.interface
            );
            return Ok(info.open_device(api)?);
        }
    }

    // Second pass / Third pass ordering is platform-dependent:
    //
    // Windows: interface numbers in the HID enumeration don't reliably
    // correspond to USB interface numbers (composite device collections are
    // re-enumerated), so match by usage_page+usage first.
    //
    // Linux/macOS: hidraw interface numbers DO match USB interface numbers, and
    // some devices (e.g. Viper Ultimate with tid=0x3f) need interface 2 even
    // though the Mouse descriptor (usage=0x0002) sits on interface 0. Match by
    // interface number first so the correct hidraw node is chosen.

    #[cfg(target_os = "windows")]
    let (second_is_usage, third_is_usage) = (true, false);
    #[cfg(not(target_os = "windows"))]
    let (second_is_usage, third_is_usage) = (false, true);

    for pass in [second_is_usage, third_is_usage] {
        for info in api.device_list() {
            if info.vendor_id() != RAZER_VID || info.product_id() != dev.pid {
                continue;
            }
            let matched = if pass {
                info.usage_page() == dev.usage_page && info.usage() == dev.usage
            } else {
                info.interface_number() == dev.interface
            };
            if matched {
                if pass {
                    debug!(
                        "Matched {} by VID/PID/usage (iface={} instead of {})",
                        dev.name,
                        info.interface_number(),
                        dev.interface
                    );
                } else {
                    debug!(
                        "Matched {} by VID/PID/interface (iface={})",
                        dev.name, dev.interface
                    );
                }
                return Ok(info.open_device(api)?);
            }
        }
    }

    Err(RazerError::DeviceNotFound {
        vid: RAZER_VID,
        pid: dev.pid,
    })
}

/// Scan USB and return all known Razer wireless devices currently connected.
pub fn find_devices() -> error::Result<Vec<RazerDevice>> {
    let api = HidApi::new()?;
    let mut found = Vec::new();
    for known in KNOWN_DEVICES {
        let seen = api
            .device_list()
            .any(|i| i.vendor_id() == RAZER_VID && i.product_id() == known.pid);
        if seen {
            found.push(known.clone());
        }
    }
    Ok(found)
}

/// Get battery level and charging status from a device.
pub fn get_battery_info(dev: &RazerDevice) -> error::Result<BatteryInfo> {
    let api = HidApi::new()?;
    let hid = open_device(&api, dev)?;

    // Set non-blocking off to ensure reads wait for data
    hid.set_blocking_mode(true)?;

    let level = protocol::get_battery_level(&hid, dev.transaction_id)?;
    let charging = protocol::get_charging_status(&hid, dev.transaction_id)?;
    Ok(BatteryInfo { level, charging })
}

/// Get only the battery percentage.
pub fn get_battery_level(dev: &RazerDevice) -> error::Result<u8> {
    let api = HidApi::new()?;
    let hid = open_device(&api, dev)?;
    hid.set_blocking_mode(true)?;
    protocol::get_battery_level(&hid, dev.transaction_id)
}

/// Get only the charging status.
pub fn get_charging_status(dev: &RazerDevice) -> error::Result<bool> {
    let api = HidApi::new()?;
    let hid = open_device(&api, dev)?;
    hid.set_blocking_mode(true)?;
    protocol::get_charging_status(&hid, dev.transaction_id)
}

/// Persistent handle to one device for efficient repeated polling.
///
/// [`get_battery_info`] creates a fresh `HidApi` (full USB re-enumeration) on
/// every call, which is wasteful when polling on a schedule. A `BatterySession`
/// opens the device once and reuses the handle; call [`BatterySession::reopen`]
/// after the device is unplugged/replugged or if errors persist.
///
/// Added locally for the basilisk-power project.
pub struct BatterySession {
    api: HidApi,
    dev: RazerDevice,
    hid: hidapi::HidDevice,
}

impl BatterySession {
    /// Open the device and prepare it for blocking queries.
    pub fn new(dev: &RazerDevice) -> error::Result<Self> {
        let api = HidApi::new()?;
        let hid = open_device(&api, dev)?;
        hid.set_blocking_mode(true)?;
        Ok(Self {
            api,
            dev: dev.clone(),
            hid,
        })
    }

    /// Re-enumerate USB and open the device again (after reconnect/errors).
    pub fn reopen(&mut self) -> error::Result<()> {
        self.api.refresh_devices()?;
        self.hid = open_device(&self.api, &self.dev)?;
        self.hid.set_blocking_mode(true)?;
        Ok(())
    }

    /// The device this session is bound to.
    pub fn device(&self) -> &RazerDevice {
        &self.dev
    }

    /// Query battery level and charging status.
    pub fn query(&self) -> error::Result<BatteryInfo> {
        let level = protocol::get_battery_level(&self.hid, self.dev.transaction_id)?;
        let charging = protocol::get_charging_status(&self.hid, self.dev.transaction_id)?;
        Ok(BatteryInfo { level, charging })
    }

    /// Query the Mouse Dock Pro's RF link heartbeat.
    pub fn link_state(&self) -> error::Result<bool> {
        protocol::get_dock_link_state(&self.hid)
    }
}

/// Scan all connected devices and return battery info for each.
/// Devices that fail to respond are silently skipped.
pub fn scan_all() -> error::Result<Vec<(RazerDevice, BatteryInfo)>> {
    let devices = find_devices()?;
    let mut results = Vec::new();
    for dev in devices {
        match get_battery_info(&dev) {
            Ok(info) => results.push((dev, info)),
            Err(e) => debug!("Skipping {}: {}", dev.name, e),
        }
    }
    Ok(results)
}
