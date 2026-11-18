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
- **Dock/undock and charge changes reach the feed within a second**, not at the
  next adaptive poll.
- **A failed charging read clears the state**; it must not freeze the previous
  value.
- **A 0 % dock-relay reading is a link blip**, not an empty battery. Keep the
  last known level.
- **Battery reaches 100 %** when the raw value is `255`; charging stops and the
  LEDs return to static.
- **Mouse input works through the dock relay.** If it does not, replug the dock
  USB before blaming the app. Interface 1 answers `Access is denied`; interface
  0 is the working path.
- **The dock brightness read can answer 0 while the LEDs are lit.** Treat 0 as
  "no reading", never print or apply it as a real value.

## Feed

- `status.json` is valid JSON at every observable moment (written to
  `status.json.tmp` then renamed).
- Schema keys are **additive only**: `level`, `charging`, `source`,
  `eta_full_min`, `name`, `updated`, `updated_unix`, `poll`. Never rename or
  remove one; the skin and older consumers ignore unknown fields.
- `level` is `-1` or `0..100`; `charging` is `0|1`; `source` is one of
  `dock|wireless|wired|none|unknown`; `poll >= 1`.
- `updated_unix` is the write time; consumers may use it for freshness.

## Widget

- **Geometry is frozen.** Never change `X=`/`Y=` without an explicit request;
  see `skin/LAYOUT.md`.
- Idle fill and percentage are vivid neon blue; charging fill is vivid magenta.
- Only two text states: percentage and `DOCKED`/`WIRELESS`. ETA appears on its
  own line **only while charging**, never at 100 %.
- No boxed/histogram look, no divider, no extra labels.
- The charging pulse animates the fill, bolt and halo; idle is static.
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
  restarts Rainmeter, checks the feed, and rolls back on failure.
- No feature is done until `verify.ps1` passes (builds + all tests + feed +
  widget idle/charging + LED read-back).
- Keep `Cargo.lock` in both workspaces.
