# Release Signing Guide

This document describes the code-signing procedures for the CodeWiki binary on macOS and Windows.
Actual signing requires production certificates and is performed by the CI release workflow (`release.yml`).
**This document is the recipe; do not actually sign locally without the appropriate certificates.**

---

## macOS: Codesign + Notarytool

### Prerequisites

- An Apple Developer ID Application certificate (from the Apple Developer portal).
- The certificate installed in the macOS Keychain.
- An App Store Connect API key or Apple ID + app-specific password for notarization.
- `xcrun` available (ships with Xcode Command Line Tools).

### Step 1: Sign the binary

```bash
IDENTITY="Developer ID Application: Your Name (TEAMID)"
BINARY="target/release/codewiki"

codesign \
  --sign "$IDENTITY" \
  --options runtime \
  --timestamp \
  --verbose=4 \
  "$BINARY"
```

Flags:
- `--options runtime` — enables hardened runtime (required for notarization).
- `--timestamp` — embeds a secure timestamp from Apple's timestamp server.

### Step 2: Create a zip for notarization

```bash
zip codewiki-macos.zip codewiki
```

### Step 3: Submit for notarization

```bash
xcrun notarytool submit codewiki-macos.zip \
  --apple-id "ci@example.com" \
  --team-id "TEAMID" \
  --password "@keychain:NOTARYTOOL_PASSWORD" \
  --wait
```

- `--wait` blocks until notarization completes (typically 30–120 seconds; occasionally up to 30 minutes).
- If notarization fails, `notarytool log <submission-id>` shows the rejection reason.

### Step 4: Staple (for disk-image distributions)

For `.dmg` or `.pkg` distributions, staple the notarization ticket:

```bash
xcrun stapler staple codewiki
xcrun stapler validate codewiki
```

For bare binary distribution (tar.gz), the Gatekeeper check happens online when the user first runs
the binary; stapling is only relevant for offline validation.

### Step 5: Verify

```bash
codesign --verify --verbose=4 codewiki
spctl -a -vvv -t execute codewiki
```

Expected output for `spctl`:
```
codewiki: accepted
source=Notarized Developer ID
```

---

## Windows: Signtool (EV Code Signing Certificate)

### Prerequisites

- An EV (Extended Validation) code signing certificate from a trusted CA
  (DigiCert, Sectigo, GlobalSign, etc.).
- `signtool.exe` from the Windows SDK (ships with Visual Studio Build Tools).
- The certificate installed in the Windows Certificate Store (or on a hardware token).

### Step 1: Sign the binary

```powershell
$BINARY = "targeteleasedewiki.exe"
$CERT_THUMBPRINT = "ABCDEF1234..."   # From: certutil -store My

signtool.exe sign `
  /sha1 $CERT_THUMBPRINT `
  /tr http://timestamp.digicert.com `
  /td sha256 `
  /fd sha256 `
  /v `
  $BINARY
```

Flags:
- `/tr` — RFC 3161 timestamp server URL.
- `/td sha256` — timestamp digest algorithm.
- `/fd sha256` — file digest algorithm (required for Authenticode on modern Windows).

### Step 2: Verify

```powershell
signtool.exe verify /pa /v targeteleasedewiki.exe
```

Expected output:
```
Successfully verified: targeteleasedewiki.exe
```

You can also inspect the signature via:
```powershell
Get-AuthenticodeSignature targeteleasedewiki.exe
```

---

## CI Release Workflow Integration

The `release.yml` GitHub Actions workflow performs signing as follows:

```yaml
- name: Sign macOS binary (arm64)
  if: runner.os == 'macOS'
  env:
    APPLE_CERT_BASE64: ${{ secrets.APPLE_CERT_BASE64 }}
    APPLE_CERT_PASSWORD: ${{ secrets.APPLE_CERT_PASSWORD }}
    APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
    NOTARYTOOL_PASSWORD: ${{ secrets.NOTARYTOOL_PASSWORD }}
  run: |
    echo "$APPLE_CERT_BASE64" | base64 --decode > cert.p12
    security create-keychain -p "" build.keychain
    security import cert.p12 -k build.keychain -P "$APPLE_CERT_PASSWORD" -T /usr/bin/codesign
    security set-key-partition-list -S apple-tool:,apple: -s -k "" build.keychain
    codesign --sign "Developer ID Application" --options runtime --timestamp codewiki
    zip codewiki-macos-arm64.zip codewiki
    xcrun notarytool submit codewiki-macos-arm64.zip \
      --team-id "$APPLE_TEAM_ID" \
      --password "$NOTARYTOOL_PASSWORD" \
      --wait

- name: Sign Windows binary
  if: runner.os == 'Windows'
  env:
    WIN_CERT_BASE64: ${{ secrets.WIN_CERT_BASE64 }}
    WIN_CERT_PASSWORD: ${{ secrets.WIN_CERT_PASSWORD }}
  run: |
    echo $env:WIN_CERT_BASE64 | certutil -decode - cert.pfx
    $thumbprint = (Import-PfxCertificate -FilePath cert.pfx -CertStoreLocation Cert:\CurrentUser\My -Password (ConvertTo-SecureString "$env:WIN_CERT_PASSWORD" -AsPlainText -Force)).Thumbprint
    signtool.exe sign /sha1 $thumbprint /tr http://timestamp.digicert.com /td sha256 /fd sha256 /v targeteleasedewiki.exe
```

Required GitHub Actions secrets:
- `APPLE_CERT_BASE64` — base64-encoded .p12 certificate
- `APPLE_CERT_PASSWORD` — .p12 password
- `APPLE_TEAM_ID` — Apple Developer team ID
- `NOTARYTOOL_PASSWORD` — app-specific password for notarytool
- `WIN_CERT_BASE64` — base64-encoded .pfx EV certificate
- `WIN_CERT_PASSWORD` — .pfx password

---

## Linux: SHA-256 Checksums (no signing)

Linux binaries are not code-signed in v1. Instead, each release publishes
`.sha256` checksum files for each archive:

```bash
sha256sum codewiki-x86_64-unknown-linux-gnu.tar.gz \
  > codewiki-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The `install.sh` script verifies this checksum before installing (T-446).

---

## References

- Apple notarytool documentation: https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution
- Windows Authenticode signing: https://docs.microsoft.com/en-us/windows/win32/seccrypto/signtool
- GitHub Actions macOS codesign guide: https://localazy.com/blog/how-to-automatically-sign-macos-apps-using-github-actions
