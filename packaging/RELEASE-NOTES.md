# Easy Mouse Connect 0.3.7 preview

Unsigned Windows x64 preview for sharing a mouse and keyboard between two explicitly paired PCs. Use version 0.3.7 on both PCs. Microsoft Edge WebView2 Runtime is required; Rust and Node are needed only to build from source.

## Changes

- Keep Stop/Pause and requested file/clipboard revocations effective for the current run even if saving settings fails. The app reports the persistence error; an unsuccessful save cannot enable new sharing authority.
- Bind clipboard activation baselines and asynchronous reads/sends to their original control, bulk and policy revisions. A fast off/on transition or reconnect cannot turn pre-existing clipboard text into a fresh update or send an older sampled value through a replacement session.
- Package the existing Preview installer, portable executable, instructions, sanitized source and checksums from one CI run, with an owner-requested unpublished prerelease draft for review.

The runtime fixes are confined to core sharing policy. Native hooks/injection, wire protocol, encryption, pairing, identity format and dependencies are unchanged from 0.3.6. See the [changelog](https://github.com/bigsk1/easy-mouse-connect/blob/main/CHANGELOG.md) for details.

## Download and upgrade

Install `EasyMouseConnect-0.3.7-preview-setup.exe` for the per-user **Easy Mouse Connect Preview** installation, or run a local copy of `easy-mouse-connect.exe` on each PC. The installer offers shortcuts and an uninstaller. It requires an existing WebView2 Runtime, installs no service and adds no firewall exceptions.

Read `START-HERE.txt` before pairing. `Easy-Mouse-Connect-source-0.3.7-preview.zip` contains the sanitized source snapshot. `SHA256SUMS.txt` identifies the four payload files; it is not a publisher signature. The executables are unsigned, so Windows can show a publisher or reputation warning.

Quit both tray apps before upgrading. Retain `%LOCALAPPDATA%\EasyMouseConnect\identity.dat` and existing pairing; versions 0.2.0 through 0.3.7 preserve identity/settings data and established peers. Do not Forget device merely to upgrade. If a separate normal Tauri **Easy Mouse Connect** installation exists, Quit and uninstall it before installing this preview format; uninstall preserves identity/settings and received files.

## Evidence and limits

The 0.3.5 baseline has limited two-PC Windows 11 mouse/keyboard/scrolling and basic file-send observations. Physical acceptance of 0.3.7 remains pending. The attached assets come from the successful release workflow run and its exact source commit; CI verifies portable/platform crates, native API contracts, browser regressions and the desktop/package build. These checks do not establish physical forwarding, end-to-end latency or every mouse/display/network combination.

An AI-assisted source review of the 0.3.6 baseline identified the two policy issues addressed here. That review is not an independent professional security audit and does not establish that the app has no other vulnerabilities. See [SECURITY.md](https://github.com/bigsk1/easy-mouse-connect/blob/main/SECURITY.md) and the [acceptance matrix](https://github.com/bigsk1/easy-mouse-connect/blob/main/docs/manual-testing.md) for the trust model and remaining verification.

Pair only PCs you trust: enabled input sharing grants control of the signed-in desktop. Clipboard and file transfer start off; files require recipient approval. Elevated applications, UAC, lock screens and secure desktops are outside supported scope. Source-pointer hiding, clipboard images, automatic discovery/NAT traversal and direct Explorer file dragging are not implemented. Pause, Quit and Ctrl+Alt+Escape provide local release controls.
