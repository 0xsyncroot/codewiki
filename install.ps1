# T-446 / T-021 — CodeWiki one-liner installer (Windows PowerShell)
# Usage: iwr https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.ps1 | iex
#    Or: .\install.ps1 [-Version <tag>] [-Dir <path>] [-Uninstall]
#
# Downloads the correct pre-built Rust binary from GitHub Releases,
# verifies SHA-256, and places it in the chosen install directory.
# T-021: Adds the install directory to [HKCU\Environment]\Path if not present,
#         then prompts the user to restart their terminal.
#
# RE-RUN / UPGRADE: running this again detects the installed version, compares
# it against the target (latest release, or a pinned -Version), and:
#   * exits as a no-op when already on the target version,
#   * upgrades (or downgrades, with a notice) otherwise,
# backing up the current binary and rolling back if the new one is broken or if
# the running .exe is locked.

[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$Dir = "$env:LOCALAPPDATA\Programs\codewiki",
    [switch]$Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Repo = "0xsyncroot/codewiki"
# Override for tests: a URL returning the releases/latest JSON.
$ReleaseApi = if ($env:CODEWIKI_RELEASE_API) { $env:CODEWIKI_RELEASE_API } `
              else { "https://api.github.com/repos/$Repo/releases/latest" }

# ── Version helpers ────────────────────────────────────────────────────────────

# Parse a tag into a [version] of MAJOR.MINOR.PATCH, or $null if unparseable.
# Strips a leading 'v'/'V' and drops any -prerelease/+build suffix.
function ConvertTo-CodewikiVersion {
    param([string]$Raw)
    if ([string]::IsNullOrWhiteSpace($Raw)) { return $null }
    $s = $Raw.Trim()
    if ($s.StartsWith("v") -or $s.StartsWith("V")) { $s = $s.Substring(1) }
    # Cut at the first '-' or '+'.
    $cut = $s.IndexOfAny([char[]]@('-', '+'))
    if ($cut -ge 0) { $s = $s.Substring(0, $cut) }
    if ([string]::IsNullOrWhiteSpace($s)) { return $null }
    $parts = $s.Split('.')
    if ($parts.Count -lt 1 -or $parts.Count -gt 3) { return $null }
    $nums = @(0, 0, 0)
    for ($i = 0; $i -lt $parts.Count; $i++) {
        $n = 0
        if (-not [int]::TryParse($parts[$i], [ref]$n)) { return $null }
        if ($n -lt 0) { return $null }
        $nums[$i] = $n
    }
    return [version]::new($nums[0], $nums[1], $nums[2])
}

# Run `codewiki.exe --version` and return the raw version token, or $null.
function Get-InstalledCodewikiVersion {
    param([string]$BinPath)
    if (-not (Test-Path $BinPath)) { return $null }
    try {
        $out = & $BinPath --version 2>$null
        if (-not $out) { return $null }
        $first = ($out | Select-Object -First 1).ToString().Trim()
        # Last whitespace-separated token (e.g. "codewiki 0.1.1" -> "0.1.1").
        $token = ($first -split '\s+')[-1]
        if ([string]::IsNullOrWhiteSpace($token)) { return $null }
        return $token
    } catch {
        return $null
    }
}

# ── Uninstall path ────────────────────────────────────────────────────────────

if ($Uninstall) {
    $target = Join-Path $Dir "codewiki.exe"
    if (Test-Path $target) {
        Remove-Item $target -Force
        Write-Host "Removed $target"
    } else {
        Write-Host "codewiki not found at $Dir"
    }
    exit 0
}

# ── Arch detection ────────────────────────────────────────────────────────────

$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$TargetTriple = switch ($Arch) {
    "X64"   { "x86_64-pc-windows-msvc" }
    "Arm64" { "aarch64-pc-windows-msvc" }
    default { throw "Unsupported architecture: $Arch" }
}

# ── Target version resolution ──────────────────────────────────────────────────

if ($Version -eq "latest") {
    Write-Host "Resolving latest version..."
    $release = Invoke-RestMethod -Uri $ReleaseApi -UseBasicParsing
    $Version = $release.tag_name
    if (-not $Version) {
        throw "Could not resolve latest version. Set -Version explicitly."
    }
}

$DestBinary = Join-Path $Dir "codewiki.exe"

# ── Re-run / upgrade decision ──────────────────────────────────────────────────

$InstalledRaw  = Get-InstalledCodewikiVersion -BinPath $DestBinary
$InstalledVer  = ConvertTo-CodewikiVersion -Raw $InstalledRaw
$TargetVer     = ConvertTo-CodewikiVersion -Raw $Version

if ($null -ne $InstalledVer -and $null -ne $TargetVer) {
    $cmp = $InstalledVer.CompareTo($TargetVer)
    if ($cmp -eq 0) {
        Write-Host "codewiki is already on $Version (installed: $InstalledRaw)."
        exit 0
    } elseif ($cmp -gt 0) {
        Write-Host "Notice: downgrading $InstalledRaw -> $Version."
    } else {
        Write-Host "Upgrading $InstalledRaw -> $Version..."
    }
} elseif ($InstalledRaw) {
    Write-Host "Reinstalling codewiki (installed version '$InstalledRaw' unparseable) -> $Version..."
} else {
    Write-Host "Installing codewiki $Version for $TargetTriple..."
}

# ── Download ──────────────────────────────────────────────────────────────────

$BaseUrl = "https://github.com/$Repo/releases/download/$Version"
$Archive = "codewiki-$TargetTriple.zip"
$ChecksumFile = "$Archive.sha256"

$TmpDir = New-TemporaryFile | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_.FullName }

try {
    $ArchivePath = Join-Path $TmpDir $Archive
    $ChecksumPath = Join-Path $TmpDir $ChecksumFile

    Write-Host "Downloading $Archive..."
    Invoke-WebRequest -Uri "$BaseUrl/$Archive" -OutFile $ArchivePath -UseBasicParsing
    Invoke-WebRequest -Uri "$BaseUrl/$ChecksumFile" -OutFile $ChecksumPath -UseBasicParsing

    # ── SHA-256 verification ──────────────────────────────────────────────────

    Write-Host "Verifying checksum..."
    $ExpectedHash = (Get-Content $ChecksumPath -Raw).Trim().Split(' ', 2)[0].ToUpper()
    $ActualHash = (Get-FileHash -Path $ArchivePath -Algorithm SHA256).Hash.ToUpper()
    if ($ExpectedHash -ne $ActualHash) {
        throw "SHA-256 mismatch! Expected: $ExpectedHash  Got: $ActualHash"
    }
    Write-Host "Checksum OK"

    # ── Extract ─────────────────────────────────────────────────────────────��─

    Expand-Archive -Path $ArchivePath -DestinationPath $TmpDir -Force
    $BinaryPath = Get-ChildItem -Path $TmpDir -Filter "codewiki.exe" -Recurse | Select-Object -First 1 -ExpandProperty FullName
    if (-not $BinaryPath) {
        throw "Could not find codewiki.exe in archive."
    }

    if (-not (Test-Path $Dir)) {
        New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    }

    # ── Install / replace with backup + smoke test + rollback ──────────────────

    $Backup = $null
    if (Test-Path $DestBinary) {
        # Back up the current binary so we can restore it on any failure.
        $Backup = "$DestBinary.bak.$PID"
        Copy-Item -Path $DestBinary -Destination $Backup -Force
    }

    # Copy-Item -Force fails (UnauthorizedAccess / IOException) if codewiki.exe is
    # currently running and locked. Catch that, roll back, and tell the user to
    # close any running codewiki — never leave a half-replaced binary.
    try {
        Copy-Item -Path $BinaryPath -Destination $DestBinary -Force
    } catch {
        Write-Host ""
        Write-Host "  ERROR: could not replace $DestBinary." -ForegroundColor Red
        Write-Host "  This usually means codewiki is currently running and the file is locked."
        Write-Host "  Please close any running codewiki (and MCP server) processes and re-run."
        if ($Backup) {
            # The destination is the original (locked) binary; the copy never
            # succeeded, so just discard the backup — nothing to restore.
            Remove-Item $Backup -Force -ErrorAction SilentlyContinue
        }
        throw "codewiki.exe is locked; upgrade aborted (existing install left intact)."
    }

    # Smoke test: the freshly-installed binary must run `--version`.
    $smokeOk = $true
    try {
        & $DestBinary --version 2>$null | Out-Null
        if ($LASTEXITCODE -ne 0) { $smokeOk = $false }
    } catch {
        $smokeOk = $false
    }

    if (-not $smokeOk) {
        Write-Host "ERROR: the new codewiki binary failed its smoke test." -ForegroundColor Red
        if ($Backup) {
            Write-Host "Rolling back to the previous version..."
            Copy-Item -Path $Backup -Destination $DestBinary -Force
            Remove-Item $Backup -Force -ErrorAction SilentlyContinue
        } else {
            Remove-Item $DestBinary -Force -ErrorAction SilentlyContinue
        }
        throw "Upgrade failed; rolled back to the previous version."
    }

    # Success → drop the backup.
    if ($Backup) {
        Remove-Item $Backup -Force -ErrorAction SilentlyContinue
    }

    if ($null -ne $InstalledVer -and $null -ne $TargetVer -and $InstalledVer -ne $TargetVer) {
        Write-Host "Upgraded $InstalledRaw -> $Version ($DestBinary)"
    } else {
        Write-Host "codewiki $Version installed to $DestBinary"
    }

    # ── T-021: PATH registration ──────────────────────────────────────────────

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath -notmatch [regex]::Escape($Dir)) {
        Write-Host "Adding $Dir to user PATH..."
        $NewPath = if ($UserPath) { "$UserPath;$Dir" } else { $Dir }
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Host ""
        Write-Host "  PATH updated. Please restart your terminal for changes to take effect."
        Write-Host "  Or run the following in your current session:"
        Write-Host "    `$env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')"
        Write-Host ""
    } else {
        Write-Host "$Dir is already in your PATH."
    }

    # Verify.
    & $DestBinary --version 2>$null
} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}
