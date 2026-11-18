use hidapi::HidApi;
use log::warn;
use std::collections::{HashMap, HashSet};

use crate::controller::DeviceController;
use crate::devices::{DeviceInfo, RAZER_DEVICE_LIST};

pub struct DeviceManager {
    api: Option<HidApi>,
    device_controllers: Vec<DeviceController>,
    /// Controllers whose last command failed; reopened on the next fetch
    /// (the device may have been re-plugged between polls).
    stale: HashSet<u16>,
}

impl DeviceManager {
    pub fn new() -> Self {
        Self {
            api: None,
            device_controllers: Vec::new(),
            stale: HashSet::new(),
        }
    }

    /// The open controller for a PID, if the device is present.
    ///
    /// The tray already holds a handle per device for battery polling. LED
    /// writes must reuse it: opening a second handle to the same HID device
    /// while the tray is running makes the writes land on a handle the dock
    /// does not act on.
    pub fn controller(&self, pid: u16) -> Option<&DeviceController> {
        self.device_controllers
            .iter()
            .find(|controller| controller.pid == pid)
    }

    /// The controller to talk to for an LED target: the mouse's own PID when
    /// it is plugged in, otherwise the dock that relays to it.
    pub fn led_controller(&self, target: crate::led::Target) -> Option<&DeviceController> {
        use crate::led::Target;
        match target {
            Target::Dock => self.controller(crate::devices::RAZER_MOUSE_DOCK_PRO.pid),
            Target::Mouse => [0x00AAu16, 0x00AB]
                .into_iter()
                .find_map(|pid| self.controller(pid))
                .or_else(|| self.controller(crate::devices::RAZER_MOUSE_DOCK_PRO.pid)),
        }
    }

    pub fn fetch_devices(&mut self) -> (Vec<u32>, Vec<u32>) {
        if self.api.is_none() {
            self.api = HidApi::new()
                .inspect_err(|e| warn!("Failed to initialize HID API: {e}"))
                .ok();
        }
        let Some(api) = self.api.as_mut() else {
            return (vec![], vec![]);
        };

        if let Err(e) = api.refresh_devices() {
            warn!("Failed to refresh device list: {:?}", e);
        }

        let old_ids: HashSet<u32> = self
            .device_controllers
            .iter()
            .map(|c| c.pid as u32)
            .collect();

        // Reuse already-open handles; only open devices that are new or stale.
        let mut existing: HashMap<u16, DeviceController> = self
            .device_controllers
            .drain(..)
            .filter(|c| !self.stale.contains(&c.pid))
            .map(|c| (c.pid, c))
            .collect();
        self.stale.clear();

        let mut new_controllers: Vec<DeviceController> = Vec::new();
        for (device, path) in Self::matching_devices(api) {
            if new_controllers.iter().any(|c| c.pid == device.pid) {
                continue;
            }
            if let Some(controller) = existing.remove(&device.pid) {
                new_controllers.push(controller);
                continue;
            }
            match DeviceController::new(api, device.name.to_owned(), device.pid, path) {
                Ok(controller) => new_controllers.push(controller),
                Err(err) => warn!("Failed to create device controller: {:?}", err),
            }
        }

        let new_ids: HashSet<u32> = new_controllers.iter().map(|c| c.pid as u32).collect();

        let removed_devices: Vec<u32> = old_ids.difference(&new_ids).cloned().collect();
        let connected_devices: Vec<u32> = new_ids.difference(&old_ids).cloned().collect();

        self.device_controllers = new_controllers;

        (removed_devices, connected_devices)
    }

    pub fn get_device_name(&self, id: u32) -> Option<String> {
        self.device_controllers
            .iter()
            .find(|c| c.pid as u32 == id)
            .map(|c| c.name.clone())
    }

    pub fn get_device_battery_level(&mut self, id: u32) -> Option<i32> {
        let controller = self
            .device_controllers
            .iter()
            .find(|c| c.pid as u32 == id)?;

        match controller.get_battery_level() {
            Ok(level) => Some(level),
            Err(err) => {
                warn!("Failed to get battery level: {:?}", err);
                self.stale.insert(controller.pid);
                None
            }
        }
    }

    pub fn is_device_charging(&mut self, id: u32) -> Option<bool> {
        let controller = self
            .device_controllers
            .iter()
            .find(|c| c.pid as u32 == id)?;

        if controller.swappable_battery {
            return Some(false);
        }

        match controller.get_charging_status() {
            Ok(status) => Some(status),
            Err(err) => {
                warn!("Failed to get charging status: {:?}", err);
                self.stale.insert(controller.pid);
                None
            }
        }
    }

    fn matching_devices(api: &HidApi) -> Vec<(&'static DeviceInfo, String)> {
        let razer_devices: HashMap<(u16, u16), &'static DeviceInfo> = RAZER_DEVICE_LIST
            .iter()
            .map(|d| ((d.vid, d.pid), d))
            .collect();

        api.device_list()
            .filter_map(|hid_device| {
                let device =
                    razer_devices.get(&(hid_device.vendor_id(), hid_device.product_id()))?;
                if hid_device.interface_number() != device.interface.into() {
                    return None;
                }
                if cfg!(target_os = "windows")
                    && (hid_device.usage_page() != device.usage_page
                        || hid_device.usage() != device.usage)
                {
                    return None;
                }
                Some((*device, hid_device.path().to_string_lossy().into_owned()))
            })
            .collect()
    }
}
