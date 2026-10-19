# Windows preview releases

Download Windows x64 previews from [GitHub Releases](https://github.com/bigsk1/easy-mouse-connect/releases). Use assets from one release and install the same application version on both PCs. The per-user installer and portable executable require Microsoft Edge WebView2 Runtime; Rust and Node are needed only to build from source. Preview executables are unsigned, so Windows may show a publisher or reputation warning.

## Choose your download

| Asset | Purpose |
| --- | --- |
| `EasyMouseConnect-<version>-preview-setup.exe` | Per-user **Easy Mouse Connect Preview** installer, shortcuts and normal Windows uninstall registration |
| `easy-mouse-connect.exe` | Portable executable; run a local copy on each PC and keep each PC's own identity |
| `START-HERE.txt` | Pairing, upgrade, uninstall and troubleshooting instructions |
| `Easy-Mouse-Connect-source-<version>-preview.zip` | Sanitized source snapshot corresponding to the packaged revision |
| `SHA256SUMS.txt` | SHA-256 hashes for the included payload files |
| `BUILD-INFO.json`, when included | Source commit and build-run metadata for that package |

Choose the installer for a normal per-user installation. The portable executable creates no uninstall entry. GitHub's automatically generated source archives are separate from the explicitly sanitized source ZIP.

## Check a downloaded file

Download `SHA256SUMS.txt` from the same release as the executable. In PowerShell, calculate its SHA-256:

```powershell
Get-FileHash .\EasyMouseConnect-0.4.1-preview-setup.exe -Algorithm SHA256
```

Compare the entire hash with the entry for that filename in the manifest. A mismatch means the files do not match; download them again from the intended release before installing. Checksums identify matching bytes. They are not a publisher signature, an independent security audit or a claim that another build produces the same executable.

## Upgrade both PCs

1. Choose **Quit Easy Mouse Connect** from the tray on both PCs. The window's X hides the app and keeps sharing active.
2. Install the same preview on each PC, or replace each local portable copy while the app is closed.
3. Retain `%LOCALAPPDATA%\EasyMouseConnect\identity.dat` and the existing pairing. Each PC keeps its own protected identity/settings; do not copy one PC's profile to the other or use **Forget device** merely to upgrade.
4. Relaunch both apps, confirm the trusted connection and try crossing/return, typing and any enabled clipboard/file features.

Versions 0.4.0 and 0.4.1 share the `EMC02` wire format; they do not connect to 0.3.7. Use the same 0.4.1 build on both PCs for its clipboard fixes. Preserving identities across an upgrade does not make different wire versions compatible.

Existing **Easy Mouse Connect Preview** installations use the same preview installer format. If Windows lists a separate **Easy Mouse Connect** installation from the normal Tauri installer, Quit and uninstall that installation before installing the preview to avoid keeping two app copies. Uninstall preserves identity/settings, diagnostics, received files and cached clipboard images. Received-file and image-cache locations are described in the [README](../README.md#clipboard-and-files).

## Verification status

The retained local 0.4.1 package has a maintainer report of approximately half a day of ordinary two-PC use, behaving as expected. One keyboard interruption recovered after reselecting its Bluetooth host; the cause is unconfirmed. This is limited normal-use evidence, rather than a pass of every image format, policy/fault case, timer, network path or hardware configuration. The [0.4.1 test record](manual-testing.md#confirmed-041-local-build-use) identifies that package and the cases actually reported.

Public downloads can be separately produced CI binaries, with their own checksums and build metadata. A local test report does not make another executable byte-identical or pass its download/install smoke check. Record GitHub-package acceptance separately. The earlier [0.3.7 checks](manual-testing.md#confirmed-037-release-checks) and [0.4.0 image-copy checks](manual-testing.md#confirmed-040-image-copy-checks) retain their original scopes; version 0.3.5 remains the preserved native-input comparison baseline.

Automated protocol/core tests, Windows API contracts, browser fixtures and package compilation cover different boundaries. They do not establish physical forwarding, every application/driver/display combination or measured end-to-end latency. See [manual testing](manual-testing.md) for the remaining checks and [troubleshooting](troubleshooting.md) for diagnosis and diagnostic export.

The [AI-assisted source review](security-review.md) is additional evidence, not an independent professional security audit or proof that no vulnerabilities remain. [SECURITY.md](../SECURITY.md) describes the trust model and private reporting process.
