# Troubleshooting

Start with the diagnostics bundle: tray menu → **Export diagnostics…**, or run
`basilisk-power.exe --diagnostics`. It writes
`%LOCALAPPDATA%\BasiliskPower\diagnostics-<date>-<time>.txt` and opens it. It
contains the version, OS, autostart state, `status.json`, LED colours, Razer HID
interfaces, widget file checks, and the relevant log tails. Skim it before
posting publicly: it includes local paths.

The live log is `%LOCALAPPDATA%\BasiliskPower\basilisk-power.log` (fresh per
launch, trimmed at 2 MB). Tray menu → **Open log** opens it directly.

## The widget says `NO SIGNAL` / `--`

The widget only trusts `status.json` when `updated_unix` is at most
`StaleSeconds` old (90 s). A stale feed deliberately hides the fill, percentage
and charging ETA rather than showing an old number.

1. Is the tray running? Check Task Manager, or launch
   `%LOCALAPPDATA%\BasiliskPower\basilisk-power.exe`.
2. Read `%LOCALAPPDATA%\BasiliskPower\status.json`. It should contain
   `updated_unix` and a recent `updated` time. A feed without `updated_unix` is
   treated as stale by design.
3. Look at the log tail: `no battery reading (asleep?)` is normal while the
   mouse sleeps and wakes on input; `Command timed out` usually means the dock
   relay is busy. The tray retries and recovers on its own.
4. Tray menu → **Refresh now** forces a read; **Reconnect devices** closes and
   reopens HID handles after a replug or re-pair.
5. If only the widget is stale, restart Rainmeter. It does **not** reload a
   skin whose `.ini` was replaced from outside the app; `deploy.ps1` restarts it
   for this reason. Right-click the skin → *Refresh skin* is the manual fix.

Freshness must always come from `updated_unix`. Do not reintroduce a `HHMM`
comparison; it breaks across midnight and time zones.

## The tray icon is missing or shows `NO DEVICE`

- Launch the exe from `%LOCALAPPDATA%\BasiliskPower`, not a stale copy in the
  repo build folder.
- `Another instance is already running, exiting` in the log means a second
  launch was blocked; use the existing icon.
- Check that the dock or mouse appears in the diagnostics under
  `HID devices (Razer VID 0x1532)`. No 0x00A4 (dock) / 0x00AA / 0x00AB (mouse)
  entries means Windows does not see the device — replug it.
- **Reconnect devices** is safe to use while the tray owns the handles. Avoid
  running the CLI colour tools at the same moment as a battery poll.

## The percentage looks wrong

- A successful 0 % report from the dock relay is an RF link blip, not an empty
  battery. The tray keeps the last known value instead of showing 0 %.
- While docked, the battery is reported through the Mouse Dock Pro
  (`0x00A4`); while wireless it comes from the mouse (`0x00AB`). The widget's
  source word (`DOCKED` / `WIRELESS`) reflects the charging flag, not physical
  dock presence.
- At 100 % the dock can stop asserting charging while the mouse is still on it,
  so `WIRELESS` at 100 % is expected. The protocol has no separate
  "on the dock" signal, and guessing from battery level would mislabel a
  just-lifted mouse.
- Charging is read from the dock relay's `0x1F` flag. A single failed read is
  ignored; the tray clears charging only after two consecutive failures, so
  one busy relay reply does not freeze or flicker the state.
- Hover the icon to force a throttled refresh (at most once per 10 s), or use
  **Refresh now**.

## LED colours are wrong or do not change

- Saved state lives in `%LOCALAPPDATA%\BasiliskPower\led-colors.txt`:
  `mouse=RRGGBB`, `dock=RRGGBB`, `brightness=NN`. The defaults are `160054`
  (purple) for both.
- Re-apply and read back with:
  `basilisk-power-cli --mouse-color 160054 --dock-color 160054 --brightness 100`
  then `basilisk-power-cli --get-color`.
- The picker restores the saved brightness; a dock brightness read may return
  `0` even when the LEDs are on. That is a dock quirk, not a failure.
- Dock interface 0 is the usable control path on this hardware; interface 1
  returns `Access is denied`. Do not "fix" that by switching interfaces.
- Closing Synapse/Razer software is required — it can reclaim the HID devices.
- The mouse's own receiver and the dock relay behave differently: when the
  mouse is docked, colour writes go through the dock (`0x00A4`).

## The mouse does not come back after going idle / sleeping

Symptom: after a long idle period (or after the mouse slept), using the mouse
no longer shows up in the tray or widget. The only reliable recovery is to
unplug and replug the dock — even switching the mouse off and on does not help.

Two independent faults were addressed:

1. **Windows USB selective suspend** (was enabled on this machine) puts idle
   USB HID interfaces — including the Mouse Dock Pro relay — into a low-power
   state. The relay then stops answering feature reports until a physical
   replug. Disable it:

   ```powershell
   # via script in the repo:
   pwsh -File scripts\disable-usb-selective-suspend.ps1
   # or directly:
   powercfg /SETACVALUEINDEX SCHEME_CURRENT 2a737441-1930-4402-8d77-b2bebba308a3 48e6b7a6-50f5-4782-a5d4-53bb8f07e226 0
   powercfg /SETDCVALUEINDEX SCHEME_CURRENT 2a737441-1930-4402-8d77-b2bebba308a3 48e6b7a6-50f5-4782-a5d4-53bb8f07e226 0
   powercfg /SETACTIVE SCHEME_CURRENT
   ```

2. **The tray's wake retry waited for fresh user input and its HID retries
   blocked the worker for seconds.** A failed read used to spend up to
   10 × 500 ms on the RF relay, then the next retry required input newer than
   that attempt — so moving the mouse once during the failure could leave the
   widget blank indefinitely. The tray now:

   - asks the dock's own heartbeat (`00:bf`, transaction `0x3F`) first; the
     dock answers in about 120 ms even while the mouse sleeps and tells us the
     RF link is down without any RF traffic;
   - retries that heartbeat on a timer (5 s active, 15 s idle) regardless of
     new input, and reads the battery the moment the link returns;
   - bounds RF commands to 5 tries × 250 ms and re-enumerates every 2 s;
   - resets and reopens the HID handles after 3 consecutive failed reads while
     the link is up, at most once a minute.

   Check the heartbeat yourself with:

   ```powershell
   basilisk-power-cli.exe --link
   ```

   `UP` means the dock sees the mouse; `DOWN` means the mouse is asleep, off or
   out of range. `DOWN` while the mouse is demonstrably on and moving means the
   dock firmware's RF relay is wedged — that state has no known software reset
   and needs the dock USB re-enumerated (replug or power-cycle). The tray logs
   one warning after about two minutes in that state.

If the symptom persists, use *Reconnect devices* from the tray menu — that
performs the same handle reset on demand.

## The time estimates look odd

- **Time to full** appears only while charging and only for a positive estimate.
  It starts from a learned level slope, then falls back to roughly 1 %/min.
- **Time left** appears off the dock. It learns from up to 6 h of samples
  (recorded at most every 15 min) and falls back to 8 %/h until a stable slope
  exists. A short or rising history is ignored rather than reported as a wild
  number.
- ETA values are clamped to sane bounds (1 min … 48 h for time left, 1 … 600 min
  for time to full).

## Autostart does not work

- Check `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value
  `Basilisk Power`. The tray menu's **Start with Windows** item mirrors it.
- The stored value is an absolute path to the exe. If you moved or rebuilt the
  app, run `basilisk-power.exe --install-autostart` again.
- Disable with the menu item or `--uninstall-autostart`; it removes the value.
- Windows Task Manager → Startup apps can also show whether the entry is
  disabled at the OS level.

## Building, deploying and verifying

- The tray is a separate workspace in `app/` and needs GTK when built on Linux;
  cross-compile it for Windows and run the test exe there.
- Deploy only through `deploy.ps1`. It builds, copies, verifies SHA-256,
  restarts Rainmeter, health-checks the feed, and rolls back on failure.
- `verify.ps1` is the regression gate. It must pass before a feature is
  considered done. If it fails after a skin change, confirm Rainmeter actually
  restarted and is running — the gate starts it.
- PowerShell cannot execute scripts from a UNC path. Copy `deploy.ps1` and
  `verify.ps1` to a local folder first.
- In skin formulas, use nested ternaries for ranges. On the tested Rainmeter
  build `(level >= 0 && level <= 15)` evaluated true at level 50. This is
  documented in `REGRESSIONS.md`.

## Clean recovery

1. Exit the tray (menu → **Exit**) and stop Rainmeter's BasiliskPower skin.
2. Keep `led-colors.txt` if you want your colours; back up
   `battery-history.csv` if you want the history.
3. Run `deploy.ps1 -Start`; it replaces both exes and the skin, and removes a
   stale `status.json`.
4. If the widget stays blank, start Rainmeter manually and refresh the skin.
5. Re-run `verify.ps1` to confirm the feed, widget states, autostart and LED
   read-back all work.
