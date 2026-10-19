# Windows preview packaging

The standard Windows build is `scripts/build-windows.ps1`, which uses Tauri's NSIS pipeline and WebView2 installation support. Windows CI completes that build check, then packages the same Windows x64 executable with the smaller `windows-preview.nsi` recipe for download. Public preview assets use the established **Easy Mouse Connect Preview** installer format, install directory and uninstall registration. The normal Tauri installer remains a build check; do not offer it as a second interchangeable preview download. Release artifacts need their own signing and provenance review.

The preview installer copies the executable and instructions into the current user's local Programs directory, creates Start menu shortcuts and a per-user uninstall registration, and includes an uninstaller. The components page offers a desktop shortcut, selected by default. The final page can open the app. It requires an existing Microsoft Edge WebView2 Runtime. It does not install a service, add autostart, alter firewall rules, transfer device identities or erase user configuration/downloads on uninstall. It is unsigned.

To reproduce the small package with Python 3 and NSIS installed:

```sh
python scripts/package-preview.py --binary path/to/easy-mouse-connect.exe --output artifacts/windows-preview-0.3.7
```

`--makensis` can select a specific compiler. NSIS's `NSISDIR` environment variable can select its data directory when using a standalone/extracted toolchain. CI reuses the compiler under `%LOCALAPPDATA%\tauri\NSIS` after the Tauri build, or an available `makensis.exe` on `PATH`, with its accompanying include files. The helper treats compiler warnings as failures and derives the installer version from Tauri's configuration, checking it against `package.json`. Build the executable from that same revision before packaging.

Run the native installer smoke test in a fresh Windows test account. It installs to an isolated project folder, verifies the installed executable's SHA-256, launches it to check its window/listener, verifies that Close keeps the tray process alive, and uninstalls it. Before changing an installation it refuses an existing preview registration, smoke-test folder, running app, TCP listener or identity profile. It temporarily creates the preview's per-user desktop/Start menu/uninstall entries; its newly generated unpaired configuration remains in that test account's normal local application directory. The startup helper stops only its own test child. It does not test graceful tray Quit or forwarding.

```powershell
.\scripts\test-preview-installer.ps1 -PackageDirectory .\artifacts\windows-preview-0.3.7
```

To stage package files and a sanitized source ZIP in a new directory:

```sh
python scripts/stage-preview.py --package artifacts/windows-preview-0.3.7 --destination artifacts/staged-preview-0.3.7
```

The source snapshot uses explicit repository roots and excludes build tools, runtime identities/backups, diagnostic archives, local environment/signing-key files and generated output. `LICENSE` and optional notice files are included. Staging refuses a nonempty destination, computes `SHA256SUMS.txt`, and checks copied package files against their originals. Checksums identify matching bytes; they are not a publisher signature. Keep published versioned artifacts immutable and use a separate directory for a later source/documentation snapshot.

Before an upgrade, use **Quit Easy Mouse Connect** from the tray on both PCs; Close hides the window while the app keeps running. Install the same new build on each and retain protected identity/settings data and pairing. Normal uninstall is available in Windows Settings under **Easy Mouse Connect Preview**. It removes the installed executable, shortcuts and registration while preserving identities, diagnostics and received files. The portable executable creates no uninstall entry.

See [release preparation](../docs/releases.md) for the manual draft workflow, [development](../docs/development.md) for automated checks and [manual testing](../docs/manual-testing.md) for two-PC acceptance. Compilation, a browser preview and an installer smoke test cover different boundaries; none establishes physical forwarding on its own. Run installation tests only in a fresh test account, without replacing an existing paired installation.
