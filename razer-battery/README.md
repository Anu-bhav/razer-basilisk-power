# razer_battery

A cross-platform Rust library for reading battery level and charging status from wireless Razer devices via HID.

Based on the protocol reverse-engineered by the [OpenRazer](https://github.com/openrazer/openrazer) project. Works on **Windows**, **Linux**, and **macOS** — no Razer Synapse or OpenRazer daemon required.

## Features

- Query battery percentage (0–100%) and charging status
- 20+ pre-configured wireless Razer mice
- Easy to add custom/unsupported devices
- **Platform-aware I/O**: uses output/input reports on Windows (avoids the "parameter is incorrect" error), feature reports on Linux/macOS
- Retry logic for busy devices with configurable attempts
- CRC validation on all responses
- Detailed debug logging via `log` crate

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
razer_battery = { path = "./razer_battery" }
```

```rust
use razer_battery::{scan_all, find_devices, get_battery_info};

fn main() -> razer_battery::error::Result<()> {
    for (dev, info) in scan_all()? {
        println!("{}: {}% (charging: {})", dev.name, info.level, info.charging);
    }
    Ok(())
}
```

## Supported Devices

| Device | PID | Interface | Transaction ID |
|--------|-----|-----------|---------------|
| DeathAdder V3 Pro | 0x00B6 | 0 | 0x1f |
| DeathAdder V3 HyperSpeed | 0x00B8 | 0 | 0x1f |
| DeathAdder V2 Pro | 0x007C | 2 | 0x3f |
| Viper Ultimate | 0x007A | **2** | 0x3f |
| Viper Ultimate Wireless | 0x007B | **2** | 0x3f |
| Viper V2 Pro | 0x00A5 | 0 | 0x1f |
| Viper V3 Pro | 0x00C2 | 0 | 0x1f |
| Basilisk Ultimate | 0x0088 | **2** | 0x3f |
| Basilisk V3 Pro | 0x00AA | 0 | 0x1f |
| Naga Pro | 0x008F | 2 | 0x1f |
| Naga V2 HyperSpeed | 0x00B3 | 0 | 0x1f |
| Orochi V2 | 0x0094 | 0 | 0x1f |
| Mamba Wireless | 0x0072 | **2** | 0x3f |
| Cobra Pro | 0x00AF | 0 | 0x1f |
| ...and more | | | |

> **Note:** Devices with transaction_id `0x3f` (older wireless receivers) typically need **interface 2**.

## Adding Unsupported Devices

```rust
let my_mouse = razer_battery::devices::custom_device(
    "My Mouse",
    0x00AB,  // PID from Device Manager or lsusb
    2,       // interface — try 0 first, then 2 if it fails
    0x0001,  // usage page
    0x0002,  // usage
    0x3f,    // transaction_id — try 0x1f, 0x3f, or 0xff
);
let info = razer_battery::get_battery_info(&my_mouse)?;
```

### Finding your device info

**PID:** On Windows, open Device Manager → your mouse → Properties → Details → Hardware Ids → look for `VID_1532&PID_XXXX`. On Linux, run `lsusb | grep 1532`.

**Interface & transaction_id:** Check OpenRazer's `razermouse_driver.c`, find the `razer_attr_read_charge_level` function. Common patterns:
- Newer mice (2021+): interface 0, transaction_id `0x1f`
- Older wireless receivers: interface 2, transaction_id `0x3f`
- Fallback: `0xff`

**Debugging:** Run with `RUST_LOG=debug` to see all HID device entries, which interfaces are available, and exactly what's being sent/received.

## Platform Notes

### Windows
- **Close Razer Synapse** before using this library — Synapse claims exclusive HID access to Razer devices.
- This library uses `hid_write`/`hid_read` (output/input reports) on Windows instead of feature reports to avoid the "parameter is incorrect" (0x00000057) error that many Razer devices produce.

### Linux
You may need a udev rule to access HID devices without root:

```
# /etc/udev/rules.d/99-razer.rules
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1532", MODE="0666"
```

Then reload: `sudo udevadm control --reload-rules && sudo udevadm trigger`

### macOS
Grant Input Monitoring permission to your terminal/app in System Settings → Privacy & Security. Also, Razer's DriverKit extensions may claim exclusive HID access — you might need to uninstall them.

## Protocol

The library communicates using 90-byte HID reports with CRC validation (XOR of bytes 2–87). Battery queries use command class `0x07` with command IDs `0x80` (battery level) and `0x84` (charging status). The raw battery value (0–255) is scaled to 0–100%.

On Windows, reports are sent as 91 bytes (report ID `0x00` prefix + 90 bytes payload) via output reports, and responses are read via input reports. On Linux/macOS, feature reports are used via hidraw.

## Troubleshooting

| Error | Likely cause | Fix |
|-------|-------------|-----|
| "Device not found" | Receiver not plugged in, or Synapse has exclusive access | Plug in receiver, close Synapse |
| "parameter is incorrect" (0x57) | Wrong interface number, or feature reports not supported | The library handles this on Windows automatically; if using custom_device, try interface 2 |
| "Invalid response" / timeout | Wrong transaction_id | Try 0x1f, 0x3f, or 0xff |
| Permission denied (Linux) | Missing udev rule | Add the udev rule above |
| 0% battery always | Device asleep or wrong interface | Wake the mouse, try different interface |

## License

MIT

## Acknowledgments

- [OpenRazer](https://github.com/openrazer/openrazer) — protocol reverse-engineering
- [razer-battery-report](https://github.com/xzeldon/razer-battery-report) — Rust reference
- [razer-battery-checker](https://github.com/spozer/razer-battery-checker) — Python reference
- [Mac-Razer-Mouse-Battery-Indicator](https://github.com/hamburger-fries/Mac-Razer-Mouse-Battery-Indicator) — macOS reference
