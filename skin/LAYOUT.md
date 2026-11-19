# Basilisk Power — Rainmeter skin layout (FROZEN)

The X/Y values below were tuned by hand by the user and visually approved.
**Do not change any `X=` or `Y=` value in `basilisk.ini` unless the user
explicitly asks for it.** They are recorded here so they can be restored if the
file is ever overwritten or a future edit goes wrong.

Skin path (live):
`%USERPROFILE%\Documents\Rainmeter\Skins\BasiliskPower\basilisk.ini`

Deployed copy in this repository: `skin/basilisk.ini`.

Backups:
- `%LOCALAPPDATA%\BasiliskPower\basilisk.ini.backup` is the previous live skin,
  written by `deploy.ps1` before it replaces the file. `maintenance.rs` never
  age-deletes it.
- Git history keeps every earlier layout as well.

Geometry is the frozen part; comments and cadence settings are not. Any
comment-only edit changes the hash, so use the table below as the source of
truth rather than a single SHA.

Current deployed-copy SHA-256 (16-meter layout, 2 s read cadence):
`84C0EFFD821845362F2BE651B48BBB31066C056316257E027D96A406C2A47D38`

## Meter geometry (as approved)

| Meter             | Type   | X   | Y   | W  | H  | Notes                                      |
|-------------------|--------|-----|-----|----|----|--------------------------------------------|
| MeterFillCyan     | Bar    | 34  | 24  | 68 | 40 | fill inside the battery body, discharging  |
| MeterFillMagenta  | Bar    | 34  | 24  | 68 | 40 | same rect, shown while charging            |
| MeterFillLow      | Bar    | 34  | 24  | 68 | 40 | same rect, red at or below the low cutoff  |
| MeterFillPulse    | Shape  | —   | —   | —  | —  | rect `34,24,68,40`; dims the charging fill |
| MeterBodyPulse    | Shape  | —   | —   | —  | —  | rect `34,24,68,40`; magenta breathing wash |
| MeterBattery      | IMAGE  | 30  | 20  | 92 | 48 | battery.png outline (transparent centre)   |
| MeterBoltGlowWide | IMAGE  | 48  | 17  | 40 | 54 | widest, faintest bolt halo                 |
| MeterBoltGlowMid  | IMAGE  | 52  | 22  | 32 | 44 | middle halo                                |
| MeterBoltGlowNear | IMAGE  | 55  | 26  | 26 | 36 | near halo                                  |
| MeterBolt         | IMAGE  | 57  | 29  | 22 | 30 | bolt.png, cyan, pulsing while charging      |
| MeterLevel        | String | 200 | 20  | —  | —  | right-aligned, 16 pt cyan `%1%`             |
| MeterLevelStale   | String | 200 | 20  | —  | —  | same box, grey `--%` for a dead feed        |
| MeterLevelLow     | String | 200 | 20  | —  | —  | same box, red low-battery percentage        |
| MeterSource       | String | 200 | 47  | —  | —  | right-aligned, 8 pt magenta `DOCKED` etc.   |
| MeterSourceStale  | String | 200 | 47  | —  | —  | same box, grey `NO SIGNAL`                  |
| MeterEta          | String | 200 | 58  | —  | —  | right-aligned, 8 pt white `%1 MIN TO FULL`  |

The two Shape meters position their rectangles with coordinates in the
`Shape=` formula, not with `X=`/`Y=`. Keep the rectangle at `34,24,68,40`.

Skin window on screen: about **200 x 68 px** (`DynamicWindowSize=1`). The
window grows to fit the widest right-aligned text and shrinks again when the
ETA line hides.

## Non-layout settings worth keeping

- `[mLevelCalc]` must keep `MinValue=0` / `MaxValue=100`; otherwise Rainmeter
  scales the 0-100 value against its default 0..1 range and the fill is always
  full.
- `[measureStatus]` regex uses `(?si)` — **no `U` flag**; ungreedy would make
  `\d+` capture a single digit (`57` -> `5`).
- `UpdateDivider=2` on `[measureStatus]` with `Update=1000` gives the skin its
  own 2 s read cadence. It is independent of the feed's advisory `poll` field.
  Keep the literal `2` (Rainmeter ignores variables in `UpdateDivider`).
- `StaleSeconds=90` is three times the tray's 30 s status heartbeat. Do not
  lower it below the heartbeat or a healthy idle feed will blank the widget.
- `[mPulse]` and the glow measures still tick every second so the charging
  animation stays smooth; only the WebParser file read is slowed to 2 s.
- `@include=#@#Variables.inc` resolves to this skin's own `@Resources`
  (Rainmeter does not share `@Resources` between configs).
