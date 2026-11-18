<#
.SYNOPSIS
    Build and deploy Basilisk Power to %LOCALAPPDATA%\BasiliskPower.

.DESCRIPTION
    Rebuilds the three executables inside WSL, stops the running tray, copies
    the fresh binaries, and verifies each copy by hash. Exits non-zero if the
    build or any copy fails, so a failed deploy fails loudly instead of
    silently leaving an old exe in place.

    This is the only supported way to update the installed copy: it always
    installs the binaries just built from the current source tree, never a
    leftover from an earlier build.

    Copy it out of the WSL share before running it, because PowerShell cannot
    execute a script directly from a UNC path:

        Copy-Item <repo>\deploy.ps1 "$env:TEMP\opencode\deploy.ps1" -Force
        pwsh -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\opencode\deploy.ps1"

.EXAMPLE
    pwsh -File deploy.ps1
    pwsh -File deploy.ps1 -SkipBuild      # copy the existing build output
    pwsh -File deploy.ps1 -Start          # deploy, then start the tray
#>
[CmdletBinding()]
param(
    [string]$Source   = "\\wsl.localhost\Ubuntu\home\anon\razer_basilisk_power",
    [string]$WslRepo  = "/home/anon/razer_basilisk_power",
    [string]$Dest     = "$env:LOCALAPPDATA\BasiliskPower",
    [string]$Skin     = "$env:USERPROFILE\Documents\Rainmeter\Skins\BasiliskPower",
    [switch]$SkipBuild,
    [switch]$SkipSkin,
    [switch]$Start
)

$ErrorActionPreference = 'Stop'

$builds = @(
    @{ Path = "app\target\x86_64-pc-windows-gnu\release\basilisk-power.exe"; Name = "basilisk-power.exe" }
    @{ Path = "target\x86_64-pc-windows-gnu\release\basilisk-power-cli.exe";  Name = "basilisk-power-cli.exe" }
    @{ Path = "target\x86_64-pc-windows-gnu\release\led-picker.exe";         Name = "led-picker.exe" }
)

function Get-Hash([string]$p) {
    if (-not (Test-Path $p)) { return $null }
    (Get-FileHash -Algorithm SHA256 -Path $p).Hash
}

# 1. Build the latest source. Cargo can skip a rebuild when the target's
#    timestamp looks newer than the source (a WSL/Windows clock quirk), so the
#    tray binary is removed first to force a real compile.
if (-not $SkipBuild) {
    Write-Host "Building release binaries in WSL..." -ForegroundColor Cyan
    # The tray is its own workspace under app/, so it is built separately from
    # the root workspace that produces the CLI and the colour picker.
    $buildCmd = "cd '$WslRepo/app' && rm -f target/x86_64-pc-windows-gnu/release/basilisk-power.exe && " +
                "cargo build --release --target x86_64-pc-windows-gnu && " +
                "cd '$WslRepo' && cargo build --release --target x86_64-pc-windows-gnu -p basilisk-power && " +
                "cargo build --release --target x86_64-pc-windows-gnu -p led-picker"
    wsl -d Ubuntu -- bash -c $buildCmd
    if ($LASTEXITCODE -ne 0) {
        Write-Host "`nBUILD FAILED -- nothing was deployed." -ForegroundColor Red
        exit 1
    }
}

# 2. Stop anything that would hold a file open.
Write-Host "`nStopping running instances..." -ForegroundColor Cyan
foreach ($n in @('basilisk-power', 'led-picker')) {
    Get-Process -Name $n -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "  stopping $($_.ProcessName) (pid $($_.Id))"
        $_.Kill()
        $_.WaitForExit(5000) | Out-Null
    }
}
Start-Sleep -Milliseconds 700

if (-not (Test-Path $Dest)) { New-Item -ItemType Directory -Path $Dest -Force | Out-Null }

# 3. Copy and verify each exe.
$failed = $false
Write-Host "`nDeploying executables..." -ForegroundColor Cyan
foreach ($b in $builds) {
    $src = Join-Path $Source $b.Path
    $dst = Join-Path $Dest $b.Name

    if (-not (Test-Path $src)) {
        Write-Host "  MISSING SOURCE: $src" -ForegroundColor Red
        $failed = $true
        continue
    }

    # Keep one generation of the previous binary. If the new tray cannot
    # produce a valid feed, rollback is a copy away instead of a rebuild.
    if (Test-Path $dst) {
        Copy-Item -LiteralPath $dst -Destination "$dst.backup" -Force
    }

    $srcHash = Get-Hash $src
    try {
        Copy-Item -LiteralPath $src -Destination $dst -Force
    } catch {
        Write-Host "  COPY FAILED ($($b.Name)): $($_.Exception.Message)" -ForegroundColor Red
        $failed = $true
        continue
    }

    $dstHash = Get-Hash $dst
    if ($srcHash -eq $dstHash) {
        $size = [math]::Round((Get-Item $dst).Length / 1KB, 0)
        Write-Host "  OK   $($b.Name)  ($size KB)" -ForegroundColor Green
    } else {
        Write-Host "  HASH MISMATCH: $($b.Name)" -ForegroundColor Red
        Write-Host "       src $srcHash" -ForegroundColor Red
        Write-Host "       dst $dstHash" -ForegroundColor Red
        $failed = $true
    }
}

# 4. Skin files.
if (-not $SkipSkin) {
    Write-Host "`nDeploying skin..." -ForegroundColor Cyan
    $skinSrc = Join-Path $Source "skin"
    $pairs = @(
        @{ Src = "basilisk.ini";             Dst = (Join-Path $Skin "basilisk.ini") }
        @{ Src = "@Resources\Variables.inc"; Dst = (Join-Path $Skin "@Resources\Variables.inc") }
    )
    foreach ($p in $pairs) {
        $src = Join-Path $skinSrc $p.Src
        if (-not (Test-Path $src)) {
            Write-Host "  MISSING SOURCE: $src" -ForegroundColor Red
            $failed = $true
            continue
        }
        $srcHash = Get-Hash $src
        Copy-Item -LiteralPath $src -Destination $p.Dst -Force
        $dstHash = Get-Hash $p.Dst
        if ($srcHash -eq $dstHash) {
            Write-Host "  OK   $($p.Src)" -ForegroundColor Green
        } else {
            Write-Host "  HASH MISMATCH: $($p.Src)" -ForegroundColor Red
            $failed = $true
        }
    }

    # stale.inc was removed from the skin; delete any deployed copy so a
    # stale include cannot be loaded by an older basilisk.ini.
    $old = Join-Path $Skin "@Resources\stale.inc"
    if (Test-Path $old) {
        Remove-Item $old -Force
        Write-Host "  removed obsolete @Resources\stale.inc" -ForegroundColor DarkGray
    }

    # Rainmeter does not reload a skin when the .ini is replaced from outside
    # its own editor, so the new file would sit unused until the next launch.
    # Restart it when it is running; a closed Rainmeter is left alone.
    $rainmeter = Get-Process Rainmeter -ErrorAction SilentlyContinue
    if ($rainmeter) {
        Write-Host "  restarting Rainmeter to load the new skin" -ForegroundColor DarkGray
        $rainmeter | Stop-Process -Force
        Start-Sleep -Seconds 2
        Start-Process "C:\Program Files\Rainmeter\Rainmeter.exe"
    }
}

# 5. Remove a stale feed so the tray regenerates it.
$feed = Join-Path $Dest "status.json"
if (Test-Path $feed) {
    Write-Host "`nRemoving stale status.json..." -ForegroundColor Cyan
    Remove-Item $feed -Force
}

if ($failed) {
    Write-Host "`nDeploy FAILED -- see messages above." -ForegroundColor Red
    exit 1
}

$feedPath = Join-Path $Dest "status.json"

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
        if ($age -gt 60 -or $age -lt -60) { return $false }
    }
    return $true
}

function Restore-Backups {
    $restored = $false
    foreach ($b in $builds) {
        $dst = Join-Path $Dest $b.Name
        if (Test-Path "$dst.backup") {
            Copy-Item -LiteralPath "$dst.backup" -Destination $dst -Force
            $restored = $true
        }
    }
    return $restored
}

if ($Start) {
    Write-Host "`nStarting tray..." -ForegroundColor Cyan
    Start-Process (Join-Path $Dest "basilisk-power.exe")

    Write-Host "Waiting for a healthy status.json..." -ForegroundColor Cyan
    $healthy = $false
    $sawFeed = $false
    for ($i = 0; $i -lt 20; $i++) {
        Start-Sleep -Seconds 1
        if (Test-FeedHealth $feedPath) {
            $sawFeed = $true
            $j = Get-Content $feedPath -Raw | ConvertFrom-Json
            # Prefer a feed that actually names a device; the first write is
            # often the legitimate "nothing answered yet" placeholder.
            if ($j.source -and $j.source -ne 'none') { $healthy = $true; break }
        }
    }
    if (-not $healthy -and $sawFeed) {
        Write-Host "  note: feed is valid, but no device answered yet" -ForegroundColor Yellow
        $healthy = $true
    }

    if (-not $healthy) {
        Write-Host "HEALTH CHECK FAILED: no valid feed after 30 s." -ForegroundColor Red
        Get-Process -Name basilisk-power -ErrorAction SilentlyContinue | ForEach-Object {
            $_.Kill(); $_.WaitForExit(5000) | Out-Null
        }
        Start-Sleep -Milliseconds 700
        if (Restore-Backups) {
            Write-Host "Restored the previous executables; restarting them." -ForegroundColor Yellow
            Start-Process (Join-Path $Dest "basilisk-power.exe")
        } else {
            Write-Host "No previous executables to restore." -ForegroundColor Red
        }
        exit 1
    }

    $j = Get-Content $feedPath -Raw | ConvertFrom-Json
    Write-Host ("  OK   feed: {0}%, charging={1}, {2}, poll={3}s" -f `
        $j.level, $j.charging, $j.source, $j.poll) -ForegroundColor Green
}

Write-Host "`nDeploy OK." -ForegroundColor Green
if (-not $Start) {
    Write-Host "Start the tray with:  Start-Process `"`"$Dest\basilisk-power.exe`"`"" -ForegroundColor DarkGray
}
exit 0
