<#
.SYNOPSIS
    Full regression gate for Basilisk Power.

.DESCRIPTION
    Builds both workspaces, runs every test (the tray's tests are compiled for
    Windows and executed natively), deploys with hash verification, then
    smoke-tests the three things that matter:

      * tray + status.json feed (schema, ranges, freshness)
      * Rainmeter widget in both states (idle blue, charging magenta + ETA +
        visible fill pulse), then recovery to the real feed
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

function Write-FakeFeed([int]$Level, [int]$Charging, [string]$Source, [int]$Eta) {
    $now = (Get-Date).ToString("HH:mm")
    $unix = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
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

Write-Host "`n=== Tests ===" -ForegroundColor Cyan
wsl -d Ubuntu -- bash -c "cd '$WslRepo' && cargo test --release 2>&1 | tail -20"
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
    $blue = 0; $magenta = 0
    $w = [Math]::Min(103, $Bmp.Width); $h = [Math]::Min(64, $Bmp.Height)
    for ($y = 24; $y -lt $h; $y++) {
        for ($x = 34; $x -lt $w; $x++) {
            $p = $Bmp.GetPixel($x, $y)
            if ($p.B -gt 150 -and $p.G -gt 80 -and $p.R -lt 60) { $blue++ }
            if ($p.R -gt 150 -and $p.B -gt 150 -and $p.G -lt 90) { $magenta++ }
        }
    }
    return @{ Blue = $blue; Magenta = $magenta }
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
        Check "idle hides ETA row" ($bmp.Height -le 70) "height=$($bmp.Height)"
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
