/// Razer USB Vendor ID.
pub const RAZER_VID: u16 = 0x1532;

/// Describes a known Razer wireless device.
#[derive(Debug, Clone)]
pub struct RazerDevice {
    /// Human-readable device name
    pub name: &'static str,
    /// USB Product ID (wireless receiver PID)
    pub pid: u16,
    /// HID interface number to open
    pub interface: i32,
    /// HID usage page
    pub usage_page: u16,
    /// HID usage
    pub usage: u16,
    /// Firmware transaction ID for this device
    pub transaction_id: u8,
}

/// Create a custom RazerDevice for hardware not in KNOWN_DEVICES.
///
/// Find the PID from Device Manager (Windows) or `lsusb` (Linux).
/// Look up transaction_id in OpenRazer's razermouse_driver.c.
/// Common values: 0x1f (newer), 0x3f (older receivers), 0xff (fallback).
///
/// **Interface number matters!** Most devices use interface 0, but some
/// (e.g. Viper Ultimate, Basilisk Ultimate) need interface 2.
/// If you get "parameter is incorrect" errors, try different interfaces.
pub fn custom_device(
    name: &'static str,
    pid: u16,
    interface: i32,
    usage_page: u16,
    usage: u16,
    transaction_id: u8,
) -> RazerDevice {
    RazerDevice { name, pid, interface, usage_page, usage, transaction_id }
}

// ── Known device database ───────────────────────────────────────────────
// Sources: OpenRazer razermouse_driver.h/.c, community battery tools.
//
// IMPORTANT: The interface number is critical. Devices that use an older
// protocol (transaction_id 0x3f) typically need interface 2, while newer
// devices (transaction_id 0x1f) typically use interface 0.
//
// transaction_id values:
//   0x1f  – most newer mice (2021+)
//   0x3f  – older wireless receivers (Mamba, Basilisk Ultimate, Viper Ultimate)
//   0xff  – fallback / default

macro_rules! dev {
    ($name:expr, $pid:expr, $iface:expr, $tid:expr) => {
        RazerDevice {
            name: $name,
            pid: $pid,
            interface: $iface,
            usage_page: 0x0001,
            usage: 0x0002,
            transaction_id: $tid,
        }
    };
    ($name:expr, $pid:expr, $iface:expr, $up:expr, $u:expr, $tid:expr) => {
        RazerDevice {
            name: $name,
            pid: $pid,
            interface: $iface,
            usage_page: $up,
            usage: $u,
            transaction_id: $tid,
        }
    };
}

pub const KNOWN_DEVICES: &[RazerDevice] = &[
    // ── DeathAdder ──────────────────────────────────────────────────
    dev!("Razer DeathAdder V3 Pro (Wireless)",       0x00B6, 0, 0x1f),
    dev!("Razer DeathAdder V3 Pro (Wired/Wireless)", 0x00B7, 0, 0x1f),
    dev!("Razer DeathAdder V3 HyperSpeed",           0x00B8, 0, 0x1f),
    dev!("Razer DeathAdder V2 Pro (Wireless)",       0x007C, 2, 0x3f),
    dev!("Razer DeathAdder V2 X HyperSpeed",         0x009C, 0, 0x1f),

    // ── Viper ───────────────────────────────────────────────────────
    // NOTE: Viper Ultimate uses interface 2 and transaction_id 0x3f
    dev!("Razer Viper Ultimate (Wired)",             0x007A, 2, 0x3f),
    dev!("Razer Viper Ultimate (Wireless)",          0x007B, 2, 0x3f),
    dev!("Razer Viper V2 Pro (Wireless)",            0x00A5, 0, 0x1f),
    dev!("Razer Viper V3 Pro (Wireless)",            0x00C2, 0, 0x1f),
    dev!("Razer Viper V3 HyperSpeed",                0x00C3, 0, 0x1f),
    dev!("Razer Viper Mini Signature Ed. (Wireless)", 0x00BA, 0, 0x1f),

    // ── Basilisk ────────────────────────────────────────────────────
    // NOTE: Basilisk Ultimate uses interface 2 and transaction_id 0x3f
    dev!("Razer Basilisk Ultimate (Receiver)",       0x0088, 2, 0x3f),
    dev!("Razer Basilisk V3 Pro (Wireless)",         0x00AA, 0, 0x1f),
    dev!("Razer Basilisk V3 X HyperSpeed",           0x00B9, 0, 0x1f),

    // ── Naga ────────────────────────────────────────────────────────
    dev!("Razer Naga Pro (Wireless)",                0x008F, 2, 0x1f),
    dev!("Razer Naga V2 HyperSpeed",                 0x00B3, 0, 0x1f),

    // ── Orochi ──────────────────────────────────────────────────────
    dev!("Razer Orochi V2",                          0x0094, 0, 0x1f),

    // ── Mamba ───────────────────────────────────────────────────────
    dev!("Razer Mamba Wireless (Receiver)",          0x0072, 2, 0x3f),

    // ── Cobra ───────────────────────────────────────────────────────
    dev!("Razer Cobra Pro (Wireless)",               0x00AF, 0, 0x1f),

    // ── HyperPolling ────────────────────────────────────────────────
    dev!("Razer HyperPolling Wireless Dongle",       0x00C0, 0, 0x1f),
];
