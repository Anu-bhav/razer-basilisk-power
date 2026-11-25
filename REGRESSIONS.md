# Regression contract

Things that must keep working. `verify.ps1` automates the parts that can be
checked without a human; the rest is a physical check when touching HID or
charging code.

## Hardware / behaviour

- **Resting LEDs stay purple `#160054`** unless the user explicitly changes
  them. No colour drift on restart, dock, undock, or charging stop.
- **The dock's own red→green breathing is preserved.** The tray never writes a
  static colour when charging *starts*; it only restores the saved colour when
  charging *stops* (and once on startup).
- **Undocking while charging reaches the feed within about 10 s.** The tray
  probes only the charging flag every 5 s and confirms a change on two
  consecutive reads; using the mouse still forces an immediate full read.
- **A single failed charging read is ignored; two consecutive failures clear
  the state.** It must not freeze the previous value, and one busy relay reply
  must not flicker the widget.
- **`source` follows the charging flag, not physical dock presence.** At 100 %
  the dock may stop asserting charging, so `WIRELESS` while sitting on the dock
  is expected. Do not invent a presence bit; the protocol has none.
- **A 0 % dock-relay reading is a link blip**, not an empty battery. Keep the
  last known level.
- **Battery reaches 100 %** when the raw value is `255`; charging stops and the
  LEDs return to static.
- **Mouse input works through the dock relay.** If it does not, replug the dock
  USB before blaming the app. Interface 1 answers `Access is denied`; interface
  0 is the working path.
- **The dock brightness read can answer 0 while the LEDs are lit.** Treat 0 as
  "no reading", never print or apply it as a real value.
- **A mouse that stops answering after a long idle / sleep must recover
  automatically.** The Mouse Dock Pro's `00:bf` / transaction `0x3F` heartbeat
  says whether its RF link is up without touching the relay. A down link is a
  healthy dock with a sleeping mouse: the tray marks the reading stale, does
  **not** spend seconds on RF retries, and re-checks the heartbeat on a timer
  (5 s active / 15 s idle) until the link returns. This retry is **not** gated
  on fresh user input, so a single failed wake attempt cannot leave the widget
  blank until the next mouse movement.
- **A link-up but unresponsive relay heals itself.** After
  `WAKE_RESET_AFTER_FAILURES` consecutive failed reads (or an exhausted wake
  backoff) with the link up, the tray performs a full HID handle reset (close +
  reopen) and forces a fresh read, at most once per `RESET_COOLDOWN`. This
  applies to poll, manual and hover reads, not only wake retries. The user
  should not have to physically replug the dock for the ordinary sleep/wake
  case. The manual *Reconnect devices* menu item still works and is the faster
  path when the user already knows the dock was re-plugged.
- **A dock read must stay bounded.** HID commands use at most 5 tries with
  250 ms gaps (not 10 × 500 ms), so a sleeping mouse cannot block the worker
  long enough to miss a quick unplug/replug. The worker re-enumerates devices
  every 2 s.
- **A dock whose link stays down while the user is active is surfaced.** The
  tray logs one clear warning after about two minutes; `basilisk-power-cli
  --link` reports the same state on demand. If the mouse is demonstrably on and
  the heartbeat still says down, the dock firmware's RF relay is wedged and
  only a USB re-enumeration (replug/power-cycle) clears it; no software command
  is known to reset that relay.

## Feed

- `status.json` is valid JSON at every observable moment (written to
  `status.json.tmp` then renamed).
- Schema keys are **additive only**: `level`, `charging`, `source`,
  `eta_full_min`, `name`, `updated`, `updated_unix`, `poll`. Never rename or
  remove one; the skin and older consumers ignore unknown fields.
- `level` is `-1` or `0..100`; `charging` is `0|1`; `source` is one of
  `dock|wireless|wired|none|unknown`; `poll >= 1`.
- `updated_unix` is the write time; consumers may use it for freshness.
- The file is rewritten on every data change **and at least every 30 s**, so
  `updated_unix` stays well inside the widget's 90 s stale threshold without
  rewriting identical content every loop.
- `poll` is an advisory HID-read hint, **not** the write cadence. The bundled
  skin ignores it and reads at its own fixed 2 s interval.

## Widget

- **Geometry is frozen.** Never change `X=`/`Y=` without an explicit request;
  see `skin/LAYOUT.md`.
- Idle fill and percentage are vivid neon blue; charging fill is vivid magenta.
- Only two text states: percentage and `DOCKED`/`WIRELESS`. ETA appears on its
  own line **only while charging**, never at 100 %.
- No boxed/histogram look, no divider, no extra labels.
- The charging pulse animates the fill, bolt and halo; idle is static.
- **Low battery (≤ 15 %, off the dock) turns the fill and percentage red.**
  Charging takes precedence and stays magenta. The threshold lives in
  `#LowBatteryPercent#`.
- **Do not use `&&` inside a skin Formula.** It evaluated
  `(level >= 0 && level <= 15)` as true at level 50 on Rainmeter 4.5.28.
  Nested ternaries (see `[cState]` / `[cEtaVisible]`) are the supported form.
- The charging look has **one owner**: `[cState]`. `[measureCharging]` no
  longer carries `IfMatch` bangs, because those could re-show magenta meters
  over a stale feed.
- A dead feed may show `--` / `NO SIGNAL`, but **recovery must restore the real
  meters on the next fresh write** (test both directions). `verify.ps1` writes
  an epoch 10 minutes in the past for the stale case, then a current one.
- Freshness is `now - updated_unix` from `freshness.lua`, threshold 90 s
  (`StaleSeconds`). The charging look and ETA come back even when the charging
  flag itself never changed while the feed was dead.
- Any simulated feed must include `updated_unix`; without it the widget is
  correctly treated as stale.
- Rainmeter does not reload an externally replaced `.ini`; restart it (the
  deploy script does) before judging a skin change.

## Build / deploy

- The tray lives in `app/` and is **not** in the root workspace. A root
  `cargo test` alone is not enough; the gate cross-compiles the tray tests and
  runs them on Windows.
- Only deploy through `deploy.ps1`: it builds, copies, verifies SHA-256,
  restarts Rainmeter, checks the feed, and rolls back on failure. The skin is
  staged before replacement and restored on failure; the previous
  `basilisk.ini` is kept as `%LOCALAPPDATA%\BasiliskPower\basilisk.ini.backup`,
  which `maintenance.rs` never age-deletes.
- No feature is done until `verify.ps1` passes (builds + all tests + feed +
  widget idle/charging + LED read-back).
- Keep `Cargo.lock` in both workspaces.
- **Every exe must report the checked-out commit.** `--version` in the tray and
  CLI, the tray menu and the startup log all show the stamp. Build scripts must
  watch the branch ref (`refs/heads/main`) and `packed-refs`, not just
  `.git/HEAD`: commits move the ref, so watching HEAD alone left an old hash.
  A stale hash in a diagnostics bundle makes support harder.
- **Autostart is one `HKCU` Run value** (`Basilisk Power`) managed by
  `--install-autostart` / `--uninstall-autostart` and the tray menu toggle.
  Never write `HKLM`; no admin rights. The value stores an absolute path, so
  re-install after moving the exe. Turning it off must leave no value behind.
- **A diagnostics bundle must not disturb the running app.** It is handled
  before the logger is initialized (which truncates the log), includes the
  version/feed/LED/HID/widget/history state, filters the Rainmeter log to
  BasiliskPower lines only, and its files are aged out after 7 days.
