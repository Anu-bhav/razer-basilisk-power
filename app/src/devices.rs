pub struct DeviceInfo {
    pub name: &'static str,
    pub pid: u16,
    pub interface: u8,
    pub usage_page: u16,
    pub usage: u16,
    pub vid: u16,
    pub swappable_battery: bool,
}

impl DeviceInfo {
    pub const fn new(
        name: &'static str,
        pid: u16,
        interface: u8,
        usage_page: u16,
        usage: u16,
        swappable_battery: bool,
    ) -> Self {
        DeviceInfo {
            name,
            pid,
            interface,
            usage_page,
            usage,
            vid: 0x1532,
            swappable_battery,
        }
    }

    pub const fn transaction_id(&self) -> u8 {
        // Every device in this build speaks the 0x1F channel: the Basilisk V3
        // Pro wireless family uses 0x1F natively, and the Mouse Dock Pro
        // relays mouse commands to the paired mouse on 0x1F (the RF relay
        // channel — same as the OpenRazer Mouse Dock Pro kernel driver).
        0x1F
    }
}

pub const RAZER_BASILISK_V3_PRO_WIRED: DeviceInfo =
    DeviceInfo::new("Razer Basilisk V3 Pro (Wired)", 0x00AA, 0, 1, 2, false);
pub const RAZER_BASILISK_V3_PRO_WIRELESS: DeviceInfo =
    DeviceInfo::new("Razer Basilisk V3 Pro (Wireless)", 0x00AB, 0, 1, 2, false);

/// The Mouse Dock Pro presents the paired mouse's battery through its own
/// HID interface. The mouse shows up entirely under this PID when it is
/// dock-paired, so it is displayed as the mouse it carries, not as a dock
/// with a battery of its own. Charging while docked happens through the
/// dock's pogo pins.
pub const RAZER_MOUSE_DOCK_PRO: DeviceInfo = DeviceInfo::new(
    "Razer Basilisk V3 Pro (Mouse Dock Pro)",
    0x00A4,
    0,
    1,
    2,
    false,
);

/// Personal build: only this mouse and its dock are probed for.
pub const RAZER_DEVICE_LIST: [DeviceInfo; 3] = [
    RAZER_BASILISK_V3_PRO_WIRED,
    RAZER_BASILISK_V3_PRO_WIRELESS,
    RAZER_MOUSE_DOCK_PRO,
];
