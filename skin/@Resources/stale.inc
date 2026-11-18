; Stale-feed detection.
;
; The tray rewrites status.json every poll (at least once a minute). If it
; dies, the file stops changing, so without this the skin would show the last
; battery level and "DOCKED / CHARGING" forever. We compare the feed's own
; "updated" HH:MM stamp against the current local time and blank the readouts
; when the feed is older than #StaleAfter# minutes.
;
; IMPORTANT: this file must only blank text and never touch the battery fill.
; MeterFillCyan / MeterFillMagenta / MeterBolt are owned by cCharging, and
; forcing a measure update or showing/hiding them from here fights cCharging's
; own IfTrue/IfFalse actions -- that feedback suppressed the charging colour
; entirely. Keep this to text only.
;
; Rainmeter's Time measure gives the current hour/minute; WebParser gives the
; feed's stamp. Both are reduced to minutes-since-midnight and subtracted.

[measureNowHour]
Measure=Time
Format=%H

[measureNowMinute]
Measure=Time
Format=%M

[measureNowAbs]
Measure=Calc
Formula=(measureNowHour * 60) + measureNowMinute
DynamicVariables=1

; The feed's "updated":"HH:MM" stamp, split into its two halves.
[measureFeedHour]
Measure=WebParser
Url=[measureStatus]
StringIndex=5
RegExp=(\d+):
StringIndex=1

[measureFeedMinute]
Measure=WebParser
Url=[measureStatus]
StringIndex=5
RegExp=:(\d+)
StringIndex=1

[measureFeedAbs]
Measure=Calc
Formula=(measureFeedHour * 60) + measureFeedMinute
DynamicVariables=1

; Age in minutes. A negative age means the stamp is in the future (clock
; change, or the machine slept past midnight), which also counts as stale.
[measureAge]
Measure=Calc
Formula=measureNowAbs - measureFeedAbs
DynamicVariables=1

; Blank the readouts when the feed has gone quiet. Text only: the battery
; fill belongs to cCharging and must not be touched here.
[measureStale]
Measure=Calc
Formula=(measureAge > StaleAfter) || (measureAge < -720)
DynamicVariables=1
IfCondition=(measureAge > StaleAfter) || (measureAge < -720)
IfTrueAction=[!SetOption MeterLevel Text "--"][!SetOption MeterSource Text "NO SIGNAL"][!SetOption MeterEta Text ""][!HideMeter MeterBoltGlowWide][!HideMeter MeterBoltGlowMid][!HideMeter MeterBoltGlowNear][!HideMeter MeterBolt][!Redraw]
IfFalseAction=[!SetOption MeterLevel MeasureName measureLevel][!SetOption MeterSource MeasureName measureSource][!SetOption MeterEta MeasureName measureEta][!Redraw]