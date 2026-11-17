# Basilisk Power

A native Windows tray application that shows the battery level of a **Razer
Basilisk V3 Pro** — wired, wireless, or sitting in a **Mouse Dock Pro** —
with low-battery notifications. No Synapse required.

Forked from [razer-battery-tray-indicator](https://github.com/va0z-code/razer-battery-tray-indicator)
(Snake Charger, by va0z-code, based on xzeldon's snake-charger) and extended
with Mouse Dock Pro support and several new features. MIT licensed.

![status](https://img.shields.io/badge/battery%20sources-wired%20%7C%20dongle%20%7C%20dock-44D62C)

## Features

- **Three sources, one mouse.** Probes exactly three device IDs:
  - `0x00AA` — Basilisk V3 Pro wired (USB cable)
  - `0x00AB` — Basilisk V3 Pro wireless (HyperSpeed dongle)
  - `0x00A4` — Mouse Dock Pro, relaying the paired mouse's battery
  When dock-paired, the mouse presents *only* under the dock's PID, so it is
  displayed as "Basilisk V3 Pro (Mouse Dock Pro)". Wired mode still shows the
  charging percentage from the cable query; docked charging happens through
  the dock's pogo pins.
- **Docked-charging inference.** The dock's relayed charging flag lies (it
  reads `false` while the mouse physically charges). While docked, charging
  is also inferred from the level climbing between reads, so the green bolt
  shows reliably.
- **0% glitch guard.** A "successful" report with 0% on the dock relay is an
  RF link-blip artifact, not a real empty battery — it is ignored and the
  last known value is kept.
- **Echo-mismatch retries.** The dock relay often answers with a stale report
  before the mouse's response arrives; reads are retried (like the OpenRazer
  kernel driver) instead of failing.
- **Adaptive polling.** 2 min ≤ 2%, 5 min ≤ 6%, 10 min ≤ 14%, 30 min ≤ 29%,
  1 h ≤ 49%, 2 h ≤ 79%, 4 h when full, 5 min while charging. A full battery
  isn't checked every five minutes.
- **Hover to refresh.** Hovering the tray icon requests a fresh read
  (throttled to one per 10 s) so the tooltip shows the latest charging data.
- **Time-to-full estimate.** While charging, the tooltip shows
  `≈ N min to full` from the recent level slope.
- **Status line in the menu**, a **Notifications** on/off toggle,
  and an **Open log** item.
- **Digits mode.** Launch with `--digits` to draw the raw battery number in
  the tray instead of the battery glyph.
- **status.json feed + battery-history.csv.** Written to
  `%LOCALAPPDATA%\BasiliskPower`, consumed by the bundled Rainmeter skin.
- **Low-battery reminders** (escalating schedule, critical at ≤ 5%), fully
  charged toasts, connection toasts, fullscreen-game awareness, wake-on-input
  retries for a sleeping mouse.
- **Single instance** (named mutex) and easy **autostart** (Run key or
  `shell:startup` shortcut).

## Dock quirks (learned the hard way)

These are empirical facts about the Mouse Dock Pro relay channel
(transaction id `0x1F`, interface 0, usage page 1, usage 2):

1. The first read after opening the dock often needs **~9 retries** with a
   ~120 ms gap before the echo matches (stale echoes, status `0x02`).
2. The relayed **charging flag is unreliable** — it can read `false` while
   the mouse physically charges through the pogo pins.
3. `0%` SUCCESS responses are **link-blip garbage**; the mouse is fine.
4. When dock-paired, the mouse **disappears from its own PIDs** and presents
   only under `0x00A4`.

The charging switch on the underside of the dock must be ON, and the mouse
must sit correctly aligned on the pogo pins — no software can fix a contact
that isn't touching.

## Building

Cross-compile from WSL (or any Linux with mingw-w64):

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

Rust 1.100 (stable), edition 2021, `hidapi` + `tao` + `tray-icon` — the exe
runs natively on Windows with no runtime dependencies. The app icon is
generated offline by the bundled `gen-icon` tool:

```sh
cargo run --bin gen-icon   # writes assets/basilisk-power.ico
```

## Installing

1. Copy `target/x86_64-pc-windows-gnu/release/basilisk-power.exe` to
   `%LOCALAPPDATA%\BasiliskPower\`.
2. Autostart: either a `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
   value pointing at the exe, or a shortcut in `shell:startup`.

### Command line options

| Flag | Effect |
| --- | --- |
| `--digits` | Tray icon shows the raw percentage instead of the glyph |
| `--simulate-battery <N>` | Report N% for every device (test toasts) |
| `--simulate-game` | Behave as if a fullscreen game is running |
| `--fast-reminders` | Reminder schedule minutes become seconds |

## Rainmeter skin

A skin in the `BasiliskPower` skin folder reads `status.json` (30 s refresh)
and shows the percentage, a bar, the source (WIRED / WIRELESS / DOCKED) and a
CHARGING flag with the time-to-full estimate — styled after the Simplic /
Mond skins (MesloLGS NF, cyan/magenta/white at low alpha, hover transparency).

## Logs

`%LOCALAPPDATA%\BasiliskPower\basilisk-power.log` — fresh per launch.

## Credits

- [va0z-code](https://github.com/va0z-code) — Snake Charger fork base
- [xzeldon](https://github.com/xzeldon) — original snake-charger
- The OpenRazer project — dock relay driver reference
- Razer green `#44D62C` — obviously

License: MIT (see [LICENSE](LICENSE)).
