# T-446 / T-021 — CodeWiki one-liner installer (Windows PowerShell)
# Usage: iwr https://raw.githubusercontent.com/your-org/codewiki/main/install.ps1 | iex
#    Or: .\install.ps1 [-Version <tag>] [-Dir <path>] [-Uninstall]
#
# Downloads the correct pre-built Rust binary from GitHub Releases,
# verifies SHA-256, and places it in the chosen install directory.
# T-021: Adds the install directory to [HKCU\Environment]\Path if not present,
#         then prompts the user to restart their terminal.

[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$Dir = "$env:LOCALAPPDATA\Programs\codewiki",
    [switch]$Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Repo = "your-org/codewiki"

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

# ── Version resolution ────────────────────────────────────────────────────────

if ($Version -eq "latest") {
    Write-Host "Resolving latest version..."
    $releaseUrl = "https://api.github.com/repos/$Repo/releases/latest"
    $release = Invoke-RestMethod -Uri $releaseUrl -UseBasicParsing
    $Version = $release.tag_name
    if (-not $Version) {
        throw "Could not resolve latest version. Set -Version explicitly."
    }
}

Write-Host "Installing codewiki $Version for $TargetTriple..."

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

    # ── Extract and install ───────────────────────────────────────────────────

    Expand-Archive -Path $ArchivePath -DestinationPath $TmpDir -Force
    $BinaryPath = Get-ChildItem -Path $TmpDir -Filter "codewiki.exe" -Recurse | Select-Object -First 1 -ExpandProperty FullName
    if (-not $BinaryPath) {
        throw "Could not find codewiki.exe in archive."
    }

    if (-not (Test-Path $Dir)) {
        New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    }
    $DestBinary = Join-Path $Dir "codewiki.exe"
    Copy-Item -Path $BinaryPath -Destination $DestBinary -Force

    Write-Host "codewiki $Version installed to $DestBinary"

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
