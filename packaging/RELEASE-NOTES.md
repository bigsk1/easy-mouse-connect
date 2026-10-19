# Easy Mouse Connect 0.4.1 preview

Windows 11 x64 preview for sharing a mouse and keyboard between two explicitly paired PCs. Install the same 0.4.1 build on both PCs. Microsoft Edge WebView2 Runtime is required; Rust and Node are development tools only.

## Changes

- Copy one local PNG, JPEG or static WebP image file from Explorer through **Share image clipboard**. The opt-in conversion creates a generated PNG without another recipient prompt and does not preserve the source filename or file format. Source files and canonical PNG each stay within 16 MiB, decoded RGBA within 64 MiB and each dimension within 16,384 pixels. Folders, multi-file selections, UNC/mapped network files and animated/unsupported formats are outside this path.
- Correct clipboard baselines when moving between text and images or recopying an earlier local image. Received-image markers suppress echoes caused by extra native formats and file metadata.
- Make optional clipboard tray notices receive-only, with a 30-second cooldown. Network notices have a five-second cooldown, and pending events older than five seconds are discarded. Messages remain content-free and disabled by default.
- Retain the `EMC02` wire format used by 0.4.0. Those two versions share the encoding, but use the same 0.4.1 build on both PCs for the fixes. Neither connects to 0.3.7. Existing version-1 DPAPI identities/pinned keys are retained without Forget device; short-code pairing never downgrades.

The preview also includes bitmap/PNG/Explorer clipboard paste with a four-image marked PNG cache, the explicit narrow UAC firewall helper, balanced Desk controls/Preferences, optional peer keep-awake and the fixed project link. Keep-awake covers display-off and idle sleep only; Windows screen savers and security locks remain effective. Cached images contain private copied pixels and remain excluded from diagnostic/source exports.

Text remains limited to 1 MiB UTF-8. App-based **Send file** preserves original bytes/name, stays strictly below 100,000,000 bytes and requires recipient approval. Image clipboard is an independent image-conversion permission, not arbitrary-file transfer authority. Native input routing/injection behavior and the existing limits remain preserved; the clipboard fixes need their own physical checks.

## Download and upgrade

Install `EasyMouseConnect-0.4.1-preview-setup.exe` for the per-user **Easy Mouse Connect Preview** installation, or run a local copy of `easy-mouse-connect.exe`. Quit both existing tray processes first, install the same build on both PCs and retain `%LOCALAPPDATA%\EasyMouseConnect\identity.dat`. Do not Forget device or copy one PC's profile to the other merely to upgrade.

Download the installer or portable executable, `START-HERE.txt`, the matching `Easy-Mouse-Connect-source-0.4.1-preview.zip` and `SHA256SUMS.txt` from this release. CI builds the package from the release commit and provides an allowlisted source snapshot. The checksum manifest identifies these release files; it is not a publisher signature. The preview is unsigned, so Windows may show a publisher or reputation warning.

The installer offers shortcuts and normal Windows uninstall registration, needs an existing WebView2 Runtime and installs no service or automatic firewall exception. Only the separate optional firewall action needs Windows administrator approval. Uninstall preserves user data, including identities, diagnostics, received files and clipboard image-cache files.

## Acceptance and limits

A retained local 0.4.1 build has a maintainer report of approximately **half a day of two-PC use behaving as expected**. One keyboard interruption recovered after reselecting its Bluetooth host; its cause is unconfirmed. This is normal-use evidence for the retained local package, not a pass of every image format, source/target combination, policy boundary, timer or fault case. The exact scope and local artifact hashes are in `docs/manual-testing.md`.

These GitHub assets are a separate CI build from the release commit. Its runtime inputs are compared with the retained tested snapshot, while executable/source-ZIP hashes and provenance are recorded separately. A short download/install/reconnect/input/clipboard smoke check of the public package remains follow-up evidence; it is not already passed by the local report or CI compilation.

The earlier 0.3.7 release and limited 0.4.0 Snipping Tool-to-Explorer report retain their original scopes. Follow the source-format, cross-kind, recopy, received-marker, bounds/cache/privacy and notification-expiry cases in the manual guide as needed. Keep the earlier local artifacts intact. Automated fixtures are separate evidence from real two-PC behavior. The earlier AI-assisted source review is not an independent professional audit or a complete security review of the new features. See `SECURITY.md`, `docs/protocol.md` and the acceptance guide in the source package.

Pair only PCs you trust. Elevated applications, UAC control, lock screens and secure desktops remain outside supported input scope. Notifications follow Windows policy. Source-pointer hiding, automatic discovery/NAT traversal and direct Explorer-to-Explorer file dragging remain unimplemented. Pause, Quit and Ctrl+Alt+Escape provide local release controls.
