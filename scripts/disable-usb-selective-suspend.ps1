# Disable Windows USB selective suspend for the active power plan.
#
# Why: selective suspend puts idle USB HID devices (Razer dock relay
# interfaces included) into a low-power state. After the mouse has been idle
# / asleep for a while the relay can stop answering feature reports, and the
# only reliable recovery is a physical replug. Disabling it keeps the dock's
# HID handles responsive without any user action.
#
# Idempotent: safe to run on every boot / before a hardware check.

$ErrorActionPreference = 'Stop'

$subGroup = '2a737441-1930-4402-8d77-b2bebba308a3' # USB settings
$setting  = '48e6b7a6-50f5-4782-a5d4-53bb8f07e226' # USB selective suspend

& powercfg /SETACVALUEINDEX SCHEME_CURRENT $subGroup $setting 0 | Out-Null
& powercfg /SETDCVALUEINDEX SCHEME_CURRENT $subGroup $setting 0 | Out-Null
& powercfg /SETACTIVE SCHEME_CURRENT | Out-Null

$current = & powercfg /QUERY SCHEME_CURRENT $subGroup $setting
if ("$current" -notmatch 'Disabled') {
    throw "USB selective suspend could not be disabled (query: $current)."
}

Write-Host 'USB selective suspend disabled for the active power plan.'
