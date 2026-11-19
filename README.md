# Basilisk Power

A native Windows tray application that shows the battery level of a **Razer
Basilisk V3 Pro** — wired, wireless, or sitting in a **Mouse Dock Pro** —
with low-battery notifications, per-device static LED colour control, and a
matching Rainmeter widget. No Synapse required.

Forked from [razer-battery-tray-indicator](https://github.com/va0z-code/razer-battery-tray-indicator)
(Snake Charger, by va0z-code, based on xzeldon's snake-charger) and extended
with Mouse Dock Pro support and several new features. MIT licensed.

![status](https://img.shields.io/badge/battery%20sources-wired%20%7C%20dongle%20%7C%20dock-44D62C)

## What's in here

| Path | What it is |
| --- | --- |
| [`app/`](app/) | The tray application (its own crate, not a workspace member) |
| [`skin/`](skin/) | The Rainmeter skin, mirrored from `%USERPROFILE%\Documents\Rainmeter\Skins\BasiliskPower` |
| [`razer-battery/`](razer-battery/) | HID library: battery reads, LED control, protocol |
| [`basilisk-power/`](basilisk-power/) | `basilisk-power-cli` — battery + LED command line tool |
| [`led-picker/`](led-picker/) | Compact OpenRGB-style colour-wheel GUI |

## Features

### Battery

- **Three sources, one mouse.** Probes exactly three device IDs:
  - `0x00AA` — Basilisk V3 Pro wired (USB cable)
  - `0x00AB` — Basilisk V3 Pro wireless (HyperSpeed dongle)
  - `0x00A4` — Mouse Dock Pro, relaying the paired mouse's battery
  When dock-paired, the mouse presents *only* under the dock's PID, so it is
  displayed as "Basilisk V3 Pro (Mouse Dock Pro)". Wired mode still shows the
  charging percentage from the cable query; docked charging happens through
  the dock's pogo pins.
- **Charging state from the relayed flag.** The dock relays the mouse's real
  charging flag on transaction `0x1F`; measured on this hardware it is
  reliable and flips within seconds of the mouse being lifted. A failed read
  clears the state rather than freezing the previous value.
- **0% glitch guard.** A "successful" report with 0% on the dock relay is an
  RF link-blip artifact, not a real empty battery — it is ignored and the
  last known value is kept.
- **Echo-mismatch retries.** The dock relay often answers with a stale report
  before the mouse's response arrives; reads are retried (like the OpenRazer
  kernel driver) instead of failing.
- **Adaptive polling.** 2 min ≤ 2%, 5 min ≤ 6%, 10 min ≤ 14%, 30 min ≤ 29%,
  1 h ≤ 49%, 2 h ≤ 79%, 4 h when full, 1-2 min while charging. While the user
  is idle the mouse is polled every minute so a dock drop is noticed fast;
  input going idle (docking the mouse) or returning (picking it up) triggers
  an immediate read.
- **Instant dock reaction.** Any charging-state change forces an immediate
  re-poll and an immediate `status.json` rewrite, so the tray and widget agree
  within a second instead of waiting out the adaptive interval.
- **Hover to refresh.** Hovering the tray icon requests a fresh read
  (throttled to one per 10 s) so the tooltip shows the latest charging data.
- **Time-to-full estimate.** While charging, the tooltip and widget show
  `≈ N min to full` from the recent level slope.
- **Time-left estimate.** Off the dock, the tooltip shows `≈ N h left` from the
  discharge slope (6 h window, samples every 15 min). A typical 8 %/h rate is
  used until the first stable slope exists, so the line is never blank.
- **Status line in the menu**, **Refresh now**, **Reconnect devices** (closes
  every HID handle and rescans, for when a re-plugged dock or mouse keeps a
  stale handle), a **Notifications** on/off toggle, **Open log**, and a
  disabled `v1.0.1 (hash)` line identifying the running build. The exe also
  answers `--version` when launched from a console.
- **Digits mode.** Launch with `--digits` to draw the raw battery number in
  the tray instead of the battery glyph.
- **`status.json` feed + `battery-history.csv`.** Written to
  `%LOCALAPPDATA%\BasiliskPower`, consumed by the bundled Rainmeter skin.
- **Low-battery reminders** (escalating schedule, critical at ≤ 5%), fully
  charged toasts, connection toasts, fullscreen-game awareness, wake-on-input
  retries for a sleeping mouse.
- **Single instance** (named mutex) and easy **autostart** (Run key or
  `shell:startup` shortcut).

### LED colour

- **Per-device colours.** The mouse and the dock's underglow get separate
  colours, because the same RGB renders slightly differently on each.
- **Read-back.** The picker asks both devices for their stored static colour
  on launch and falls back to the last colour we wrote if a device is asleep
  or undocked.
- **Live preview.** Dragging the wheel writes to the selected device as you
  go, so the LEDs follow the picker. Writes are coalesced through one
  background worker — at most one in flight, intermediate positions skipped —
  and only the final resting colour is persisted.
- **Charging breathing effect preserved.** The dock runs its own red→green
  breathing cycle while charging. The tray only re-applies the static colour
  when charging *stops* (or on first sync), never when charging starts, so the
  animation is left alone.
- **Verified writes.** The CLI reads the colour back after writing and retries
  once; the picker already re-applies until the device echoes the colour.
- **Persisted.** Last applied colours live in
  `%LOCALAPPDATA%\BasiliskPower\led-colors.txt` as `mouse=RRGGBB` /
  `dock=RRGGBB`. The last brightness is stored as `brightness=NN` and restored
  when the picker starts.

## Dock quirks (learned the hard way)

These are empirical facts about the Mouse Dock Pro relay channel
(transaction id `0x1F`, interface 0, usage page 1, usage 2):

1. The first read after opening the dock often needs **~9 retries** with a
   ~120 ms gap before the echo matches (stale echoes, status `0x02`).
2. The relayed **charging flag can lag** — measured here it is correct within
   a few seconds of a dock/undock, but it is not instantaneous, and a failed
   read returns an error rather than a wrong value.
3. `0%` SUCCESS responses are **link-blip garbage**; the mouse is fine.
4. When dock-paired, the mouse **disappears from its own PIDs** and presents
   only under `0x00A4`.
5. The dock's **own underglow does not change its reported effect** when
   charging — it keeps reporting the static effect id even during the
   charging animation. So the LED effect cannot be used as a charging signal
   on this hardware; the relayed flag is the only usable one.

The charging switch on the underside of the dock must be ON, and the mouse
must sit correctly aligned on the pogo pins — no software can fix a contact
that isn't touching.

## Building

Cross-compile from WSL (or any Linux with mingw-w64):

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

That builds the whole workspace: `razer-battery`, `basilisk-power-cli` and
`led-picker`. The tray app is a **separate crate** in `app/` and must be
built on its own:

```sh
cd app && cargo build --release --target x86_64-pc-windows-gnu
```

Rust stable, edition 2021/2024, `hidapi` + `tao` + `tray-icon` — the exes
run natively on Windows with no runtime dependencies.

> **Build pitfall:** `cargo build ... -p basilisk-power --bin basilisk-power-cli -p led-picker`
> silently skips `led-picker` because of the `--bin` filter. Build
    `-p led-picker` separately.

The app icon is generated offline by the bundled `gen-icon` tool:

```sh
cd app && cargo run --bin gen-icon   # writes assets/basilisk-power.ico
```

## Deploying

[`deploy.ps1`](deploy.ps1) is the supported way to update an installed copy.
It rebuilds all three executables in WSL, stops the running tray, copies the
fresh binaries, and verifies every copy by SHA-256. It also mirrors the skin,
restarts Rainmeter so the new `.ini` actually loads, and removes the old
`status.json` so the tray regenerates it. Any build or copy failure exits
non-zero instead of leaving a stale exe behind.

PowerShell cannot run a script directly from a UNC path, so copy it out first:

```powershell
Copy-Item <repo>\deploy.ps1 "$env:TEMP\opencode\deploy.ps1" -Force
pwsh -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\opencode\deploy.ps1" -Start
```

`-Start` launches the tray afterwards; `-SkipBuild` copies the existing build
output; `-SkipSkin` leaves Rainmeter alone.

With `-Start`, the script also performs a **health check**: it waits up to 20 s
for a fresh `status.json` naming a real device. If no valid feed appears, it
kills the new tray, restores the previous binaries from `*.backup`, restarts
them, and exits 1. A bad build therefore costs seconds, not a dead tray.

## Verifying a change

[`verify.ps1`](verify.ps1) is the regression gate. It builds both workspaces,
runs all 24 tests (the tray's tests are cross-compiled and executed natively on
Windows), deploys, then checks:

| Check | What it proves |
| --- | --- |
| feed schema/ranges/freshness | tray is alive and writing a valid document |
| widget idle | neon-blue fill, no ETA row, no magenta |
| widget charging (fake feed) | magenta fill, ETA row/text, visible fill pulse |
| tray recovery | real feed returns after the fake-feed tests |
| LED read-back | both devices still answer with the saved colours |

```powershell
Copy-Item <repo>\deploy.ps1,<repo>\verify.ps1 "$env:TEMP\opencode\" -Force
pwsh -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\opencode\verify.ps1"
```

`-SkipWidget`, `-SkipLed`, `-SkipDeploy` shorten the loop while iterating. The
invariants behind these checks are written down in
[`REGRESSIONS.md`](REGRESSIONS.md).

Then either add a `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value
pointing at `basilisk-power.exe`, or drop a shortcut in `shell:startup`.

## `basilisk-power-cli`

| Flag | Effect |
| --- | --- |
| `--watch` | Keep running, poll adaptively (rare when full, often when low) |
| `--interval N` | Force fixed polling every N seconds |
| `--json` | One-shot machine-readable output |
| `--probe` | Try interface/transaction-id combos per connected PID |
| `--list` | Enumerate Razer HID entries (VID `0x1532`) |
| `--set-color RRGGBB` | Static LED colour for both targets |
| `--mouse-color RRGGBB` | Static LED colour for the mouse only |
| `--dock-color RRGGBB` | Static LED colour for the dock only |
| `--target T` | `mouse` \| `dock` \| `both` (with `--set-color` / `--get-color`) |
| `--brightness N` | LED brightness 0-100 (with any colour option) |
| `--get-color` | Read the stored static colour and brightness from each device |
| `--watch-led` | Print both devices' stored colour/effect once a second until stopped |
| `--history [W]` | Summarise `battery-history.csv`: `7d` (default), `24h`, or `all` |
| `--version` | Print the version and git hash, then exit |
| `--digits` | Tray icon shows the raw percentage instead of the glyph |
| `--simulate-battery N` | Report N% for every device (test toasts) |
| `--simulate-game` | Behave as if a fullscreen game is running |
| `--fast-reminders` | Reminder schedule minutes become seconds |
| `--debug` | Verbose HID logging |

Examples:

```sh
basilisk-power-cli --mouse-color 00CCFF --dock-color FF2BFF --brightness 100
basilisk-power-cli --get-color
basilisk-power-cli --history 7d
```

## `led-picker`

A compact colour-wheel GUI in the style of OpenRGB: a hue ring with an inner
saturation/value square. Each device gets its own colour and swatch, there is
a shared brightness slider, and presets along the bottom. Dragging the wheel
previews live on the selected device.

The wheel is an independent egui implementation — no OpenRGB code is used.
OpenRGB is credited as inspiration.

## Rainmeter skin

The skin lives in
`%USERPROFILE%\Documents\Rainmeter\Skins\BasiliskPower\` and is mirrored in
[`skin/`](skin/).

It reads `status.json` every second and shows:

- a transparent battery glyph whose fill tracks the real level
- the percentage
- the source: `DOCKED` while charging, `WIRELESS` otherwise
- a time-to-full estimate while charging
- a cyan lightning bolt with a soft white glow, pulsing while charging
- a charging breath: the magenta fill dims and brightens on the same clock,
  with a faint magenta wash inside the body
- below 15 % off the dock, the fill and percentage turn coral red
  (`#LowBattery#`); a docked mouse keeps the magenta charging look
- a dead feed is visible, not frozen: after 90 s without a write the fill
  disappears and the text changes to `--` / `NO SIGNAL`. The next valid write
  restores the real meters (and the charging look if it is still charging).

Colours come from the Simplic theme: `#NeonBlue#` for the idle fill and the
percentage, `#FontColor2_Neon#` for charging and the source word,
`#FontColor3#` white, MesloLGS NF. The blue is the same vivid neon the CPU/RAM
line graph uses, so the two widgets read as one set.

**Geometry is frozen.** Do not change any `X=`/`Y=` value — see
[`skin/LAYOUT.md`](skin/LAYOUT.md) before touching them.

### Install

Copy `skin/` into `%USERPROFILE%\Documents\Rainmeter\Skins\BasiliskPower\`,
then refresh Rainmeter. `deploy.ps1` does this for you (see
[Deploying](#deploying)), and `verify.ps1` checks the whole chain (see
[Verifying a change](#verifying-a-change)).

### Data schema

`%LOCALAPPDATA%\BasiliskPower\status.json`:

```json
{"level":61,"charging":1,"source":"dock","eta_full_min":53,"name":"Razer Basilisk V3 Pro (Mouse Dock Pro)","updated":"14:32","updated_unix":1732026720,"poll":60}
```

| Field | Meaning |
| --- | --- |
| `level` | Battery percent, `-1` when unknown |
| `charging` | `1` while charging, else `0` |
| `source` | `dock` (charging) \| `wireless` \| `wired` \| `none` |
| `eta_full_min` | Minutes to full while charging, `-1` when unknown |
| `name` | Device name |
| `updated` | Local `HH:MM` of the write |
| `updated_unix` | Unix epoch of the write, for exact freshness checks |
| `poll` | Seconds until the tray next writes |

`battery-history.csv` appends a row per reading (`date,time,level,charging,source`),
trimmed to 2000 rows / 1 MB.

### History insights

`basilisk-power-cli --history` turns that CSV into a short report:

```text
Battery history — last 7d
  window: 2026-11-12 09:00 → 2026-11-19 01:48 (1832 samples)
  level: latest 94%, min 41%, max 100%, average 86.2%
  charging: 74 of 1832 samples (4%), 5 sessions
  drain: 6.4%/h off the dock (12.1 h observed, 15.6 h per 100%)
```

- `--history 24h` or `--history all` change the window; the default is `7d`.
- Charge sessions merge dropouts less than 20 minutes apart, so a dock relay
  that briefly reports "not charging" at 100 % does not inflate the count.
- Drain uses only downward steps between off-dock samples and skips gaps over
  four hours (sleep/shutdown), which would otherwise understate the rate.

### Simulating the feed

For testing the skin without hardware, stop the tray and write `status.json`
yourself:

```powershell
$unix = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
'{"level":42,"charging":1,"source":"dock","eta_full_min":18,"name":"X","updated":"14:32","updated_unix":' + $unix + ',"poll":60}' |
  Set-Content "$env:LOCALAPPDATA\BasiliskPower\status.json" -Encoding ASCII
```

`updated_unix` is required for the freshness check. A hand-written feed
without it is treated as stale and shows `NO SIGNAL`.

### Rainmeter gotchas baked into the skin

These cost real debugging time; they are documented in the skin comments too.

- `#@#` resolves to the skin's *own* `@Resources` folder (not the Skins
  root), so the skin ships its own copy of `Variables.inc`.
- **`UpdateRate` defaults to 600 ms** and multiplies with `Update` and
  `UpdateDivider`. Without an explicit `UpdateRate=1` the skin re-reads
  `status.json` roughly every 50 minutes. This is the single most common
  reason a Rainmeter data widget appears frozen.
- **`UpdateDivider` does not accept variables** — it must be a literal number.
- **`Formula=` on a Calc measure takes a bare measure name**, not
  `[&MeasureName]` (that is bang syntax). The bracketed form silently never
  matches.
- **A bang naming a non-existent meter makes Rainmeter refuse to load the
  whole skin.** When removing a meter, remove every `!ShowMeter`/`!HideMeter`/
  `!SetOption` reference to it or the skin vanishes with no error dialog.
- Bar meters need explicit `MinValue`/`MaxValue` (they default to 0..1).
- The WebParser regex must not use the `(?U)` flag or `\d+` captures a single
  digit.
- A dynamic `!SetOption` on `UpdateDivider` after load is ignored.
- A `status.json` that is replaced while Rainmeter reads it can parse as a
  truncated document. The tray writes `status.json.tmp` and renames it into
  place, so readers only ever see a complete file.
- Freshness is computed by `@Resources\freshness.lua` from `updated_unix`.
  It reads the file itself instead of adding a sixth capture group: Rainmeter
  rejected the longer WebParser expression at runtime even though it was
  valid PCRE. Keep the proven `measureStatus` regex alone.
- **Avoid `&&` in skin expressions.** On this Rainmeter build
  `(level >= 0 && level <= 15)` evaluated true at level 50, and a four-clause
  `IfCondition` showed the ETA during a stale feed. The widget therefore
  encodes freshness, charging and low-battery as nested ternaries on 0/1
  helper measures, and tests a single equality. Do not "simplify" them back
  to `&&`.
- **Rainmeter does not reload a skin when its `.ini` is replaced from outside
  the app.** The old layout keeps running until Rainmeter restarts, which
  makes a correct change look broken. `deploy.ps1` restarts it for this
  reason; when editing by hand, right-click the skin → *Refresh skin*.

## Disk hygiene

`%LOCALAPPDATA%\BasiliskPower` is trimmed automatically on a timer:

| File | Cap |
| --- | --- |
| `basilisk-power.log` | 2 MB (also truncated on each launch) |
| `battery-history.csv` | 1 MB / 2000 rows |
| `*.backup`, `*.bak` | deleted after 7 days |

The two executables, `led-colors.txt` and `status.json` are never touched.

## Logs

`%LOCALAPPDATA%\BasiliskPower\basilisk-power.log` — fresh per launch.

## Credits

- [va0z-code](https://github.com/va0z-code) — Snake Charger fork base
- [xzeldon](https://github.com/xzeldon) — original snake-charger
- The [OpenRazer](https://openrazer.github.io/) project — dock relay driver
  reference, and the Mouse Dock Pro charging protocol
- [OpenRGB](https://openrgb.org/) — the colour wheel is inspired by its
  layout; the implementation here is independent Rust
- Razer green `#44D62C` — obviously

License: MIT (see [LICENSE](LICENSE)).