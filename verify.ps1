<#
.SYNOPSIS
    Full regression gate for Basilisk Power.

.DESCRIPTION
    Builds both workspaces, runs every test (the tray's tests are compiled for
    Windows and executed natively), deploys with hash verification, then
    smoke-tests the three things that matter:

      * tray + status.json feed (schema, ranges, freshness)
      * Rainmeter widget states (idle blue, charging magenta + ETA + visible
        pulse, stale NO SIGNAL, low-battery red), then recovery to the real feed
      * autostart install/uninstall and the diagnostics bundle
      * LED colour read-back against led-colors.txt

    Exits non-zero if anything fails, so a feature cannot be called done while
    it has broken the app, the widget, or the LEDs.

    PowerShell cannot execute a script from a UNC path. Copy deploy.ps1 and
    verify.ps1 to the same local folder first:

        Copy-Item <repo>\deploy.ps1,<repo>\verify.ps1 "$env:TEMP\opencode\" -Force
        pwsh -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\opencode\verify.ps1"

.EXAMPLE
    pwsh -File verify.ps1                    # full gate
    pwsh -File verify.ps1 -SkipWidget        # no fake-feed/Rainmeter checks
    pwsh -File verify.ps1 -SkipDeploy        # build + tests only
#>
[CmdletBinding()]
param(
    [string]$Source       = "\\wsl.localhost\Ubuntu\home\anon\razer_basilisk_power",
    [string]$WslRepo      = "/home/anon/razer_basilisk_power",
    [string]$Dest         = "$env:LOCALAPPDATA\BasiliskPower",
    [string]$Skin         = "$env:USERPROFILE\Documents\Rainmeter\Skins\BasiliskPower",
    [string]$DeployScript = (Join-Path $PSScriptRoot "deploy.ps1"),
    [switch]$SkipBuild,
    [switch]$SkipDeploy,
    [switch]$SkipWidget,
    [switch]$SkipLed
)

$ErrorActionPreference = 'Stop'
$script:failures = 0

function Check([string]$Name, [bool]$Ok, [string]$Detail = "") {
    if ($Ok) {
        Write-Host "  PASS  $Name" -ForegroundColor Green
    } else {
        $script:failures++
        Write-Host "  FAIL  $Name  $Detail" -ForegroundColor Red
    }
}

function Test-FeedHealth([string]$Path) {
    if (-not (Test-Path $Path)) { return $false }
    try {
        $j = Get-Content $Path -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
    } catch {
        return $false
    }
    if ($null -eq $j.level -or $null -eq $j.charging -or
        $null -eq $j.source -or $null -eq $j.poll) { return $false }
    if ([int]$j.level -lt -1 -or [int]$j.level -gt 100) { return $false }
    if (@(0, 1) -notcontains [int]$j.charging) { return $false }
    if ($j.source -notin @('dock', 'wireless', 'wired', 'none', 'unknown')) { return $false }
    if ([int]$j.poll -lt 1) { return $false }
    if ($null -ne $j.updated_unix -and [int64]$j.updated_unix -gt 0) {
        $age = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - [int64]$j.updated_unix
        if ($age -gt 120 -or $age -lt -120) { return $false }
    }
    return $true
}

function Wait-HealthyFeed([int]$Seconds = 30) {
    for ($i = 0; $i -lt $Seconds; $i++) {
        Start-Sleep -Seconds 1
        if (Test-FeedHealth (Join-Path $Dest "status.json")) { return $true }
    }
    return $false
}

function Write-FakeFeed([int]$Level, [int]$Charging, [string]$Source, [int]$Eta, [int]$AgeSeconds = 0) {
    $now = (Get-Date).ToString("HH:mm")
    $unix = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - $AgeSeconds
    $json = '{"level":' + $Level + ',"charging":' + $Charging + ',"source":"' + $Source +
            '","eta_full_min":' + $Eta + ',"name":"Regression Test","updated":"' + $now +
            '","updated_unix":' + $unix + ',"poll":60}'
    Set-Content (Join-Path $Dest "status.json") $json -Encoding ASCII
}

function Start-Tray {
    Start-Process (Join-Path $Dest "basilisk-power.exe")
}

function Stop-Tray {
    Get-Process -Name basilisk-power -ErrorAction SilentlyContinue | ForEach-Object {
        $_.Kill(); $_.WaitForExit(5000) | Out-Null
    }
    Start-Sleep -Milliseconds 700
}

# ---------------------------------------------------------------- build/tests
Write-Host "`n=== Build ===" -ForegroundColor Cyan
if (-not $SkipBuild) {
    wsl -d Ubuntu -- bash -c "cd '$WslRepo/app' && rm -f target/x86_64-pc-windows-gnu/release/basilisk-power.exe && cargo build --release --target x86_64-pc-windows-gnu"
    Check "tray build" ($LASTEXITCODE -eq 0)

    wsl -d Ubuntu -- bash -c "cd '$WslRepo' && cargo build --release --target x86_64-pc-windows-gnu -p basilisk-power && cargo build --release --target x86_64-pc-windows-gnu -p led-picker"
    Check "cli + picker build" ($LASTEXITCODE -eq 0)
}

Write-Host "`n=== Style (fmt + clippy) ===" -ForegroundColor Cyan
# Both workspaces must stay rustfmt-clean and clippy-warning-free; these were
# added after the 2026-11 audit (F11) so the gate catches drift immediately.
wsl -d Ubuntu -- bash -c "cd '$WslRepo' && cargo fmt --check"
Check "root fmt --check" ($LASTEXITCODE -eq 0)
wsl -d Ubuntu -- bash -c "cd '$WslRepo' && cargo clippy --release --target x86_64-pc-windows-gnu --quiet 2>&1 | tail -20"
Check "root clippy" ($LASTEXITCODE -eq 0)
wsl -d Ubuntu -- bash -c "cd '$WslRepo/app' && cargo fmt --check"
Check "tray fmt --check" ($LASTEXITCODE -eq 0)
wsl -d Ubuntu -- bash -c "cd '$WslRepo/app' && cargo clippy --release --target x86_64-pc-windows-gnu --quiet 2>&1 | tail -20"
Check "tray clippy" ($LASTEXITCODE -eq 0)

Write-Host "`n=== Tests ===" -ForegroundColor Cyan
wsl -d Ubuntu -- bash -c "set -o pipefail; cd '$WslRepo' && cargo test --release 2>&1 | tail -20"
Check "root workspace tests" ($LASTEXITCODE -eq 0)

# The tray is a separate workspace and needs GTK to build for Linux, so its
# tests are cross-compiled and executed natively on Windows.
$jsonLines = wsl -d Ubuntu -- bash -c "cd '$WslRepo/app' && cargo test --no-run --release --target x86_64-pc-windows-gnu --message-format=json 2>/dev/null"
$testExes = @()
foreach ($line in $jsonLines) {
    try { $obj = $line | ConvertFrom-Json } catch { continue }
    if ($obj.executable -and $obj.executable -match 'basilisk_power-[0-9a-f]+\.exe$') {
        $testExes += $obj.executable
    }
}
Check "tray test build" ($testExes.Count -gt 0) "no test executable was produced"
foreach ($exe in $testExes) {
    $unc = "\\wsl.localhost\Ubuntu" + ($exe -replace '/', '\')
    & $unc --test-threads=1
    Check "tray tests ($([System.IO.Path]::GetFileName($exe)))" ($LASTEXITCODE -eq 0)
}

# --------------------------------------------------------------------- deploy
if (-not $SkipDeploy) {
    Write-Host "`n=== Deploy ===" -ForegroundColor Cyan
    if (-not (Test-Path $DeployScript)) {
        Check "deploy script present" $false $DeployScript
    } else {
        & pwsh -NoProfile -ExecutionPolicy Bypass -File $DeployScript -SkipBuild -Start
        Check "deploy + tray health" ($LASTEXITCODE -eq 0)
    }
}

# ------------------------------------------------------------------ autostart
Write-Host "`n=== Autostart ===" -ForegroundColor Cyan
$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$runName = "Basilisk Power"
$trayExe = Join-Path $Dest "basilisk-power.exe"
$prior = (Get-ItemProperty -Path $runKey -Name $runName -ErrorAction SilentlyContinue).$runName
$autoOut = Join-Path $env:TEMP "basilisk-autostart.txt"

$p = Start-Process $trayExe -ArgumentList "--install-autostart" -Wait -PassThru -RedirectStandardOutput $autoOut
$installed = (Get-ItemProperty -Path $runKey -Name $runName -ErrorAction SilentlyContinue).$runName
Check "autostart installs a Run value" ($p.ExitCode -eq 0 -and $installed) "exit=$($p.ExitCode) value=$installed"
Check "autostart points at the deployed exe" ($installed -and $installed -match [regex]::Escape($trayExe)) "value=$installed"

$p = Start-Process $trayExe -ArgumentList "--uninstall-autostart" -Wait -PassThru -RedirectStandardOutput $autoOut
$removed = (Get-ItemProperty -Path $runKey -Name $runName -ErrorAction SilentlyContinue).$runName
Check "autostart uninstall removes the value" ($p.ExitCode -eq 0 -and -not $removed) "exit=$($p.ExitCode) value=$removed"

# Leave the user's machine exactly as it was before the gate ran.
if ($prior) {
    New-ItemProperty -Path $runKey -Name $runName -Value $prior -PropertyType String -Force | Out-Null
}

# ---------------------------------------------------------------- diagnostics
Write-Host "`n=== Diagnostics ===" -ForegroundColor Cyan
$diagOut = Join-Path $env:TEMP "basilisk-diagnostics.txt"
$p = Start-Process $trayExe -ArgumentList "--diagnostics" -Wait -PassThru -RedirectStandardOutput $diagOut
$diagStdout = Get-Content $diagOut -Raw
$diagName = [regex]::Match($diagStdout, 'diagnostics-\d{4}-\d{2}-\d{2}-\d{6}\.txt').Value
Check "diagnostics command succeeds" ($p.ExitCode -eq 0) "exit=$($p.ExitCode)"
Check "diagnostics names its file" ([bool]$diagName) "stdout=$diagStdout"
if ($diagName) {
    $diagPath = Join-Path $Dest $diagName
    Check "diagnostics file written" (Test-Path $diagPath)
    if (Test-Path $diagPath) {
        $diagReport = Get-Content $diagPath -Raw
        Check "diagnostics has version and feed" ($diagReport -match 'version:' -and $diagReport -match 'status.json')
        Check "diagnostics has HID and widget state" ($diagReport -match 'HID devices' -and $diagReport -match 'widget \(Rainmeter\)')
        Check "diagnostics has history and log tails" ($diagReport -match 'battery-history.csv' -and $diagReport -match 'basilisk-power.log')
        Remove-Item $diagPath -Force
    }
}

# ----------------------------------------------------------------- widget test
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class VerifyCap {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint f);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, Rr, B; }
  public static IntPtr ByTitle(string needle) {
    IntPtr f = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      int n = GetWindowTextLength(h);
      if (n > 0) { var sb = new StringBuilder(n+1); GetWindowText(h, sb, sb.Capacity);
        string t = sb.ToString();
        if (t.IndexOf(needle, StringComparison.OrdinalIgnoreCase) >= 0) { f = h; return false; } }
      return true; }, IntPtr.Zero);
    return f;
  }
}
'@

function Get-WidgetShot {
    $h = [VerifyCap]::ByTitle("basilisk.ini")
    if ($h -eq [IntPtr]::Zero) { return $null }
    $r = New-Object VerifyCap+R
    [VerifyCap]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Rr - $r.L; $ht = $r.B - $r.T
    if ($w -le 0 -or $ht -le 0) { return $null }
    $bmp = New-Object System.Drawing.Bitmap($w, $ht)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    [VerifyCap]::PrintWindow($h, $hdc, 2) | Out-Null
    $g.ReleaseHdc($hdc); $g.Dispose()
    return $bmp
}

function Measure-Battery([System.Drawing.Bitmap]$Bmp) {
    $blue = 0; $magenta = 0; $red = 0
    $w = [Math]::Min(103, $Bmp.Width); $h = [Math]::Min(64, $Bmp.Height)
    for ($y = 24; $y -lt $h; $y++) {
        for ($x = 34; $x -lt $w; $x++) {
            $p = $Bmp.GetPixel($x, $y)
            if ($p.B -gt 150 -and $p.G -gt 80 -and $p.R -lt 60) { $blue++ }
            if ($p.R -gt 150 -and $p.B -gt 150 -and $p.G -lt 90) { $magenta++ }
            if ($p.R -gt 150 -and $p.G -lt 120 -and $p.B -lt 120) { $red++ }
        }
    }
    return @{ Blue = $blue; Magenta = $magenta; Red = $red }
}

function Measure-FillBrightness([System.Drawing.Bitmap]$Bmp) {
    $sum = 0
    for ($y = 30; $y -lt 55 -and $y -lt $Bmp.Height; $y++) {
        for ($x = 40; $x -lt 64 -and $x -lt $Bmp.Width; $x++) {
            $p = $Bmp.GetPixel($x, $y)
            $sum += $p.R + $p.B
        }
    }
    return $sum
}

function Measure-TextPixels([System.Drawing.Bitmap]$Bmp, [int]$X0, [int]$Y0, [int]$X1, [int]$Y1) {
    $grey = 0; $magenta = 0; $blue = 0; $red = 0
    $xEnd = [Math]::Min($X1, $Bmp.Width)
    $yEnd = [Math]::Min($Y1, $Bmp.Height)
    for ($y = $Y0; $y -lt $yEnd; $y++) {
        for ($x = $X0; $x -lt $xEnd; $x++) {
            $p = $Bmp.GetPixel($x, $y)
            if ($p.R -gt 90 -and $p.G -gt 90 -and $p.B -gt 90 -and
                [Math]::Abs($p.R - $p.G) -lt 24 -and [Math]::Abs($p.G - $p.B) -lt 28) { $grey++ }
            if ($p.R -gt 150 -and $p.B -gt 150 -and $p.G -lt 90) { $magenta++ }
            if ($p.B -gt 150 -and $p.G -gt 80 -and $p.R -lt 60) { $blue++ }
            if ($p.R -gt 150 -and $p.G -lt 120 -and $p.B -lt 120) { $red++ }
        }
    }
    return @{ Grey = $grey; Magenta = $magenta; Blue = $blue; Red = $red }
}

if (-not $SkipWidget) {
    Write-Host "`n=== Widget states ===" -ForegroundColor Cyan
    if (-not (Get-Process Rainmeter -ErrorAction SilentlyContinue)) {
        Start-Process "C:\Program Files\Rainmeter\Rainmeter.exe"
        Start-Sleep -Seconds 8
    }

    Stop-Tray

    Write-FakeFeed -Level 50 -Charging 0 -Source "wireless" -Eta -1
    Start-Sleep -Seconds 5
    $bmp = Get-WidgetShot
    Check "widget window visible" ($null -ne $bmp)
    if ($bmp) {
        $m = Measure-Battery $bmp
        Check "idle fill is neon blue" ($m.Blue -gt 600) "blue=$($m.Blue)"
        Check "idle fill is not magenta" ($m.Magenta -lt 60) "magenta=$($m.Magenta)"
        Check "idle fill is not red" ($m.Red -lt 60) "red=$($m.Red)"
        Check "idle hides ETA row" ($bmp.Height -le 70) "height=$($bmp.Height)"
        $t = Measure-TextPixels $bmp 125 44 200 58
        Check "idle source word is magenta" ($t.Magenta -gt 20) "magenta=$($t.Magenta), grey=$($t.Grey)"
        $bmp.Dispose()
    }

    # Dead feed: an epoch 10 minutes old must flip the widget to placeholders
    # instead of freezing on the last reading.
    Write-FakeFeed -Level 50 -Charging 0 -Source "wireless" -Eta -1 -AgeSeconds 600
    Start-Sleep -Seconds 5
    $bmp = Get-WidgetShot
    Check "stale widget visible" ($null -ne $bmp)
    if ($bmp) {
        $m = Measure-Battery $bmp
        Check "stale feed hides the fill" ($m.Blue -lt 80 -and $m.Magenta -lt 80) "blue=$($m.Blue), magenta=$($m.Magenta)"
        $t = Measure-TextPixels $bmp 125 44 200 58
        Check "stale feed shows NO SIGNAL" ($t.Grey -gt 20 -and $t.Magenta -lt 15) "grey=$($t.Grey), magenta=$($t.Magenta)"
        $l = Measure-TextPixels $bmp 140 18 200 40
        Check "stale feed shows --" ($l.Grey -gt 15 -and $l.Blue -lt 20) "grey=$($l.Grey), blue=$($l.Blue)"
        Check "stale feed hides ETA row" ($bmp.Height -le 70) "height=$($bmp.Height)"
        $bmp.Dispose()
    }

    # Recovery: a fresh write must restore the real meters, not leave the
    # placeholders on screen.
    Write-FakeFeed -Level 50 -Charging 0 -Source "wireless" -Eta -1
    Start-Sleep -Seconds 5
    $bmp = Get-WidgetShot
    Check "recovered widget visible" ($null -ne $bmp)
    if ($bmp) {
        $m = Measure-Battery $bmp
        Check "recovered feed restores blue fill" ($m.Blue -gt 600) "blue=$($m.Blue)"
        $t = Measure-TextPixels $bmp 125 44 200 58
        Check "recovered feed restores source word" ($t.Magenta -gt 20 -and $t.Grey -lt 20) "grey=$($t.Grey), magenta=$($t.Magenta)"
        $bmp.Dispose()
    }

    # Low battery, off the dock: red fill and percentage, no extra text.
    Write-FakeFeed -Level 10 -Charging 0 -Source "wireless" -Eta -1
    Start-Sleep -Seconds 5
    $bmp = Get-WidgetShot
    Check "low-battery widget visible" ($null -ne $bmp)
    if ($bmp) {
        $m = Measure-Battery $bmp
        Check "low battery fill is red" ($m.Red -gt 150) "red=$($m.Red)"
        Check "low battery hides blue fill" ($m.Blue -lt 60) "blue=$($m.Blue)"
        Check "low battery hides magenta fill" ($m.Magenta -lt 60) "magenta=$($m.Magenta)"
        Check "low battery hides ETA row" ($bmp.Height -le 70) "height=$($bmp.Height)"
        $l = Measure-TextPixels $bmp 140 18 200 40
        Check "low percentage is red" ($l.Red -gt 15) "red=$($l.Red)"
        $bmp.Dispose()
    }

    # Stale while charging: the charging look and ETA must not appear either.
    Write-FakeFeed -Level 50 -Charging 1 -Source "dock" -Eta 24 -AgeSeconds 600
    Start-Sleep -Seconds 5
    $bmp = Get-WidgetShot
    Check "stale charging widget visible" ($null -ne $bmp)
    if ($bmp) {
        $m = Measure-Battery $bmp
        Check "stale charging hides magenta fill" ($m.Magenta -lt 80) "magenta=$($m.Magenta)"
        Check "stale charging hides ETA row" ($bmp.Height -le 70) "height=$($bmp.Height)"
        $bmp.Dispose()
    }

    Write-FakeFeed -Level 50 -Charging 1 -Source "dock" -Eta 24
    Start-Sleep -Seconds 5
    $bmp = Get-WidgetShot
    Check "charging widget visible" ($null -ne $bmp)
    if ($bmp) {
        $m = Measure-Battery $bmp
        Check "charging fill is magenta" ($m.Magenta -gt 600) "magenta=$($m.Magenta)"
        Check "charging hides cyan fill" ($m.Blue -lt 200) "blue=$($m.Blue)"
        Check "charging hides red fill" ($m.Red -lt 60) "red=$($m.Red)"
        Check "charging shows ETA row" ($bmp.Height -ge 72) "height=$($bmp.Height)"
        $eta = 0
        for ($y = 60; $y -lt $bmp.Height; $y++) {
            for ($x = 120; $x -lt $bmp.Width; $x++) {
                $p = $bmp.GetPixel($x, $y)
                if ($p.R -gt 60 -and [Math]::Abs($p.R-$p.G) -lt 15 -and [Math]::Abs($p.G-$p.B) -lt 15) { $eta++ }
            }
        }
        Check "ETA text drawn" ($eta -gt 20) "grey pixels=$eta"
        $bmp.Dispose()
    }

    # Pulse: four samples over one half-cycle must move the fill brightness.
    $samples = @()
    foreach ($i in 1..4) {
        $bmp = Get-WidgetShot
        if ($bmp) { $samples += (Measure-FillBrightness $bmp); $bmp.Dispose() }
        Start-Sleep -Seconds 1
    }
    if ($samples.Count -ge 2) {
        $spread = ($samples | Measure-Object -Maximum).Maximum - ($samples | Measure-Object -Minimum).Minimum
        $base = ($samples | Measure-Object -Maximum).Maximum
        Check "charging fill pulses" ($base -gt 0 -and ($spread / $base) -gt 0.08) "samples=$($samples -join ','), spread=$spread"
    } else {
        Check "charging fill pulses" $false "no samples"
    }

    # Leave the tray stopped: the LED read-back below opens its own HID handle,
    # and a second handle while the tray runs can make the tray's dock read
    # time out. The tray is restarted after both smoke tests.
}

# -------------------------------------------------------------------- LED test
if (-not $SkipLed) {
    Write-Host "`n=== LEDs ===" -ForegroundColor Cyan
    Stop-Tray
    $saved = Join-Path $Dest "led-colors.txt"
    if (-not (Test-Path $saved)) {
        Write-Host "  SKIP  no led-colors.txt yet" -ForegroundColor Yellow
    } else {
        $text = Get-Content $saved -Raw
        $expected = @{}
        foreach ($line in ($text -split "`n")) {
            if ($line -match '^(mouse|dock)=([0-9A-Fa-f]{6})') {
                $expected[$Matches[1]] = '#' + $Matches[2].ToUpper()
            }
        }
        $out = & (Join-Path $Dest "basilisk-power-cli.exe") --get-color 2>&1
        $hexes = [regex]::Matches(($out -join "`n"), '#[0-9A-Fa-f]{6}') | ForEach-Object { $_.Value.ToUpper() }
        Check "LED read-back returns both devices" ($hexes.Count -ge 2) "output: $out"
        if ($hexes.Count -ge 2 -and $expected.Count -ge 2) {
            Check "mouse colour matches saved" ($hexes[0] -eq $expected['mouse']) "read $($hexes[0]), saved $($expected['mouse'])"
            Check "dock colour matches saved"  ($hexes[1] -eq $expected['dock'])  "read $($hexes[1]), saved $($expected['dock'])"
        }
    }
}

# Restore the real tray after the smoke tests and prove it reads hardware.
if (-not $SkipWidget -or -not $SkipLed) {
    Write-Host "`n=== Restore tray ===" -ForegroundColor Cyan
    Remove-Item (Join-Path $Dest "status.json") -Force -ErrorAction SilentlyContinue
    Start-Tray
    Check "tray restarts with a valid feed" (Wait-HealthyFeed 30)

    $readDevice = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Seconds 1
        $feedPath = Join-Path $Dest "status.json"
        if (Test-Path $feedPath) {
            try { $j = Get-Content $feedPath -Raw | ConvertFrom-Json } catch { continue }
            if ($j.source -and $j.source -ne 'none') { $readDevice = $true; break }
        }
    }
    Check "tray reads a real device" $readDevice "feed stayed at source=none"
}

# ---------------------------------------------------------------------- result
Write-Host ""
if ($script:failures -gt 0) {
    Write-Host "GATE FAILED: $($script:failures) check(s) failed." -ForegroundColor Red
    exit 1
}
Write-Host "GATE OK: builds, tests, feed, widget and LEDs all passed." -ForegroundColor Green
exit 0
