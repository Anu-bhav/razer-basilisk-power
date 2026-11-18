# Basilisk Power — Rainmeter skin layout (FROZEN)

The X/Y values below were tuned by hand by the user and visually approved.
**Do not change any `X=` or `Y=` value in `basilisk.ini` unless the user
explicitly asks for it.** They are recorded here so they can be restored
if the file is ever overwritten or a future edit goes wrong.

Skin path (live):
`%USERPROFILE%\Documents\Rainmeter\Skins\BasiliskPower\basilisk.ini`

Backups (all byte-identical to the approved file):
- `%LOCALAPPDATA%\BasiliskPower\basilisk.ini.backup`
- `skin/basilisk.ini` in this repository

Approved file SHA-256:
`8931BF286507DB2D40FC5C898A495339D4B1FF01BF64D0E0797DCBB13F6CAB72`

## Meter geometry (as approved)

| Meter          | Type   | X   | Y   | W  | H  | Notes                                    |
|----------------|--------|-----|-----|----|----|------------------------------------------|
| MeterFillCyan  | Bar    | 34  | 24  | 68 | 40 | fill inside battery body                 |
| MeterFillMagenta | Bar  | 34  | 24  | 68 | 40 | same rect, shown while charging          |
| MeterBattery   | IMAGE  | 30  | 20  | 92 | 48 | battery.png outline (alpha)              |
| MeterBolt      | IMAGE  | 57  | 29  | 22 | 30 | bolt.png, charging only                  |
| MeterLevel     | String | 200 | 20  | —  | —  | right-aligned, 16pt cyan `%1%`           |
| MeterSource    | String | 200 | 47  | —  | —  | right-aligned, 8pt magenta `DOCKED` etc. |
| MeterLine      | IMAGE  | 0   | 90  | 250| 2  | divider, white                           |
| MeterName      | String | 36  | 99  | —  | —  | `BASILISK`, 11pt white                   |
| MeterChargingWord | String | 208 | 62 | — | —  | right-aligned, 8pt magenta `CHARGING`    |
| MeterEta       | String | 228 | 102 | —  | —  | right-aligned, 8pt white `%1 MIN TO FULL`|

Skin window on screen: 250 x 118 px (DynamicWindowSize=1).

## Non-layout settings worth keeping

- `[mLevelCalc]` must keep `MinValue=0` / `MaxValue=100`; otherwise Rainmeter
  scales the 0-100 value against its default 0..1 range and the fill is
  always full.
- `[measureStatus]` regex uses `(?si)` — **no `U` flag**; ungreedy would make
  `\d+` capture a single digit (`57` -> `5`).
- `UpdateDivider=2` on `[measureStatus]` keeps the widget within ~2 s of the
  app's feed writes.
- `@include=#@#Variables.inc` resolves to this skin's own `@Resources`
  (Rainmeter does not share `@Resources` between configs).
