# Easy Mouse Connect

Share a mouse and keyboard between two Windows PCs over an authenticated, encrypted connection. Easy Mouse Connect uses Rust for networking and native input, with Tauri 2 and a small HTML/CSS/JavaScript settings interface.

**Current version: 0.4.1 — Windows 11 x64 preview.** [Downloads](https://github.com/bigsk1/easy-mouse-connect/releases) · [Changelog](CHANGELOG.md) · [Security](SECURITY.md)

![Easy Mouse Connect settings interface](docs/images/desktop-preview.png)

The image shows the unpaired browser preview of the settings interface.

## Features

| Feature | Behavior |
| --- | --- |
| Mouse and keyboard | Standard Windows mouse buttons, wheel events and keyboard scan codes; move across a configured left/right edge to switch PCs |
| Multiple monitors | Uses Windows display rectangles, including stacked displays, negative coordinates and monitor gaps |
| Direct connections | Enter a reachable IPv4 or IPv6 address; use a routed LAN or an existing VPN such as Tailscale |
| Encrypted pairing | Type a short-lived 12-character code; subsequent connections verify the stored device identity |
| Clipboard | Independent text and image permissions, off by default: text up to 1 MiB; PNG images up to 16 MiB |
| Files | Optional app-based transfer, off by default, with recipient approval; each file must be smaller than 100,000,000 bytes |
| Preferences | Optional peer keep-awake during local activity, plus content-free clipboard/network tray notifications |
| Troubleshooting | Explicit peer-scoped Windows Firewall setup with Windows administrator approval |
| Tray controls | Open, Pause/Resume, Stop sharing and Quit; closing the window keeps sharing available in the tray |
| Diagnostics | Local rolling logs capped at 3 × 2 MiB and a user-controlled diagnostic ZIP export |

Input capture, encryption and networking stay in Rust. Mouse events do not pass through the settings WebView. The installed app has no Node server or runtime JavaScript package dependencies.

## Install and pair

Use the same version on both PCs. **0.4.0 and 0.4.1 use the `EMC02` wire version and do not connect to 0.3.7.** Version 0.4.1 retains 0.4.0 wire compatibility; install the same 0.4.1 build on both PCs for the updated clipboard behavior. Existing paired identities are retained. Choose the Windows x64 preview in [GitHub Releases](https://github.com/bigsk1/easy-mouse-connect/releases) and download `EasyMouseConnect-<version>-preview-setup.exe` or `easy-mouse-connect.exe`, plus `START-HERE.txt`. The release also contains a sanitized source ZIP and `SHA256SUMS.txt`; [release instructions](docs/releases.md) explain the files and checksum check. If no preview is published, [build from source](#build-from-source).

The preview installer is per-user and uses the **Easy Mouse Connect Preview** installation entry. Microsoft Edge [WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) is required; Rust and Node are needed only when building from source. Preview packages are unsigned, so Windows may show a publisher or reputation warning.

1. Install the app locally on each PC, or run a local copy of the portable executable. Each PC creates its own identity; do not copy identity or settings data between PCs.
2. Make the destination reachable on TCP port **24859**. Different LAN subnets need a route. Across separate networks, use an existing VPN such as Tailscale and enter the other PC's VPN address. Entering a private IP does not create a route. Use **Troubleshooting → Add a firewall rule** if a peer-scoped inbound rule is needed. It requests Windows administrator approval for the helper; the application stays a normal user process. See [network setup](docs/networking.md) for rule scope and route checks.
3. On PC B, open **Devices → Pair a device → Show a pairing code → Generate code**.
4. On PC A, choose **Connect to a device**, enter B's IP address and type B's 12-character code. No shared clipboard or file is needed. The code expires after five minutes or five failed/abandoned attempts; generate a fresh code after either limit and keep it private.
5. Set **Peer on left/right** on each PC to match the other PC's physical position. The two settings should be opposite. Enable **Desk sharing** and **Move across screen edges**.
6. Keep local input available on both PCs and test **Ctrl+Alt+Escape** before relying on remote control. Release local keys/buttons, then move outward through the configured exposed side edge. Moving between monitors on the same PC stays local. Corners and offset monitor shoulders have a small handoff margin.

The PC that entered the code initiates automatic reconnect attempts every three seconds while sharing is enabled. Use **Reconnect** from that PC; the code-generating PC listens for connections. An IP address or device name never substitutes for the paired identity.

Close hides the window. Open it again from the tray, Start menu or desktop shortcut; a second launch focuses the existing instance. Use tray **Quit Easy Mouse Connect** to exit. **Pause** or **Stop sharing** releases input and stops sharing until re-enabled. **Forget device** revokes trust and requires explicit pairing to restore it.

## Clipboard and files

In **Sharing**, enable each desired feature on **both PCs**. Text and image clipboard sharing are independent permissions; neither requires file transfers to be enabled. Copy new content after connecting: enabling a clipboard feature establishes a baseline and does not immediately send the old clipboard.

**Plain-text clipboard** accepts up to **1 MiB of UTF-8**. This is roughly one million ASCII characters; text containing multibyte characters reaches the byte limit sooner. Pairing-code-shaped text and legacy invitations are excluded from text synchronization.

**Image clipboard** shares copied screenshots and bitmap images as PNG. Copy with the Snipping Tool, an application's Copy action or Ctrl+C, cross back to the other PC and paste into an image-capable application. You can also copy **one local PNG, JPEG or static WebP file** in Explorer and paste its image on the other PC. This opt-in image conversion needs no additional recipient prompt; it creates a PNG with a generated name rather than preserving the original filename or format. Use **Send file** when you want the original file.

The receiver publishes bitmap formats for applications and a local PNG file representation for File Explorer paste. A copied source image file and the canonical PNG must each be at most **16 MiB**, decoded RGBA at most **64 MiB**, and each dimension at most **16,384 pixels**. Folders, multiple-file selections, animated WebP, UNC paths and mapped network files are outside image clipboard support. Unsupported/oversized images are rejected; clipboard contents never appear in diagnostics or the settings UI. New text/image transitions and copying an earlier local image again count as new clipboard activity, without reflecting received images back to the peer.

For Explorer compatibility, received images leave managed PNG files under `%LOCALAPPDATA%\EasyMouseConnect\clipboard-images`. The cache retains at most **four managed images of up to 16 MiB each**, with Windows internet-zone marking. These files contain the copied image and are ordinary private user-profile data, not DPAPI-encrypted identity storage. They are excluded from diagnostic exports and source packages. Copy an image into a permanent folder before relying on its cache path; later clipboard images can evict older cached files. Image sharing is separate from the consent-based **Send file** feature.

For files, enable **Allow file transfers**, drop a regular file into the app, select **Send file**, and choose **Accept file** on the receiving PC. Files must be **smaller than 100,000,000 bytes**; the exact limit and larger files are rejected. The default destination is `%USERPROFILE%\Downloads\EasyMouseConnect\<transfer-id>\<filename>` on the recipient. Sharing displays the receiving directory. Each transfer gets its own generated child directory; existing files are never overwritten.

Transfers use length and SHA-256 checks and Windows internet-zone marking before the final save. The sender reports success only after the recipient confirms completion. Received files are never opened or executed automatically. Interrupted transfers do not resume automatically. Direct Explorer-to-Explorer dragging across PCs is not implemented.

## Preferences and troubleshooting

On **Devices → Preferences**, enable **Keep the other PC awake** on both PCs to prevent the peer's display-off and idle-sleep timers while you are active on this PC. Activity remains current for 30 seconds, is signaled at most every three seconds and grants a 15-second renewable receiver lease. After local activity stops, the lease expires; disconnection or disabling the option clears it. **Windows screen savers and security locking remain in effect.** This uses the user-session power API, without synthetic mouse/keyboard input, a service or an elevated application.

**Show clipboard and network notifications** is optional and off by default. Clipboard messages describe received updates only, with a 30-second cooldown; network status messages have a five-second cooldown. Pending messages older than five seconds are discarded rather than replayed later. Messages contain no text, image previews, names, IP addresses or paths. Windows notification preferences and Focus Assist may suppress them.

**Troubleshooting** contains the explicit firewall setup action and links to connection checks. The rule permits only this executable, one literal peer IP and inbound TCP **24859** on **Private/Domain** profiles. It does not open Public networks or create a route. Windows approval is required only for that separate helper. The **Windows Preview** footer link opens the [project repository](https://github.com/bigsk1/easy-mouse-connect).

## Update or uninstall

Quit both tray apps before installing an update, then use the same version on both PCs. Keep `%LOCALAPPDATA%\EasyMouseConnect\identity.dat`: versions 0.2.0 through 0.4.1 preserve the version-1 identity/settings format and established paired peers. Versions 0.4.0 and 0.4.1 share `EMC02`, while 0.3.7 uses the older wire version. Use the same 0.4.1 build on both PCs for the fixes; retaining identities does not bridge different wire versions. **Do not forget a working peer just to upgrade.** Update both PCs before creating a new pairing.

Uninstall through **Windows Settings → Apps → Installed apps**, using **Easy Mouse Connect** or **Easy Mouse Connect Preview** for the small preview installer. Its Start menu also has an uninstall entry. Quit the tray app first. Uninstall removes the installed app, shortcuts and registration while preserving identities/settings, diagnostics, received files and cached clipboard images. The portable executable creates no uninstall entry.

## Diagnostics and support

Open **Diagnostics** for routing state, connection checks and aggregate input/processing counters. Network round-trip time does not measure end-to-end input latency.

Logs are stored in `%LOCALAPPDATA%\EasyMouseConnect\logs` as `emc.log`, `emc.1.log` and `emc.2.log`, at most **2 MiB each / 6 MiB total**. Records contain typed operational events and failure categories. They exclude raw keys/input, typed text, clipboard/file contents, credentials, file names/paths, peer IPs and device names.

**Export diagnostic ZIP** creates `%LOCALAPPDATA%\EasyMouseConnect\diagnostic-exports\emc-diagnostics.zip`. It includes sanitized log records and numeric/boolean status, including aggregate counters and display dimensions, but no identity/settings files. Each export replaces that one archive; copy an older export elsewhere first if needed. Nothing uploads automatically. Review ZIPs and screenshots before sharing them.

For ordinary bugs, use [GitHub issues](https://github.com/bigsk1/easy-mouse-connect/issues) with the app version, Windows versions, minimal reproduction, affected direction and relevant redacted diagnostics. [Troubleshooting](docs/troubleshooting.md) explains collection and counter interpretation. Report vulnerabilities [privately](https://github.com/bigsk1/easy-mouse-connect/security/advisories/new) using the process in [SECURITY.md](SECURITY.md).

## Compatibility and security

Windows 11 x64 is the supported desktop target. The two PCs do not need identical Windows feature versions or build numbers. Record `winver` results when reporting a problem: Windows updates, input drivers, display scaling and WebView2 can affect behavior. The [acceptance guide](docs/manual-testing.md) distinguishes the supported target from configurations actually tested.

A locally built 0.4.1 preview has a maintainer report of approximately half a day of use on two PCs, behaving as expected. One keyboard interruption recovered after reselecting the keyboard's Bluetooth host; its cause is unconfirmed. The report does not establish every image format, policy/fault case or hardware configuration. GitHub installers are separate CI-produced bytes; their download/install smoke check is recorded separately from that local-build report. See the [0.4.1 acceptance record](docs/manual-testing.md#confirmed-041-local-build-use).

The published 0.3.7 GitHub preview has reported two-PC confirmation for screen crossing, typing, rapid scrolling, very fast mouse movement, basic file transfer and remote-to-local text clipboard sharing through both context-menu Copy and Ctrl+C. These observations remain specific to that published 0.3.7 build. The full acceptance matrix remains incomplete. See the [confirmed cases](docs/manual-testing.md#confirmed-037-release-checks) and [release verification status](docs/releases.md#verification-status) for the scope and remaining checks.

The local 0.4.0 preview has a separate limited report that Snipping Tool copies paste into Explorer in both directions. It does not establish Explorer image-file copy or repeated cross-kind reliability. See the [0.4.0 image-copy checks](docs/manual-testing.md#confirmed-040-image-copy-checks); these observations do not pass 0.4.1.

Pair only PCs you trust: input sharing grants control of your signed-in desktop. An [AI-assisted source review](docs/security-review.md) identified two policy issues fixed in 0.3.7. It is not an independent professional audit or a guarantee that all vulnerabilities have been excluded. [SECURITY.md](SECURITY.md) describes authentication, data protection, trust boundaries and private reporting.

## Current limits

- Windows x64 user-session input only. Linux supports portable protocol/core development, not desktop input sharing.
- Elevated applications, UAC prompts, the lock screen and secure desktops are outside the supported control scope. The app runs without elevation or a service. Only the explicitly selected peer-scoped firewall helper requests Windows administrator approval; it adds no desktop-control authority.
- The sending PC's pointer remains parked at its edge; intentional source-pointer hiding is not implemented.
- A receiving cursor fallback can be covered by some shell/fullscreen surfaces. Intentional application cursor hiding can also be overridden by the conservative fallback.
- Standard Logitech events use Windows input paths, but compatibility across all devices, high polling rates, vendor gestures and remapped/injected macros is not guaranteed. Unsupported absolute raw devices release control.
- Return crossing has no exit-height metadata, so it does not preserve the exact vertical position between PCs.
- Automatic discovery, automatic NAT traversal and direct Explorer-to-Explorer file dragging are not implemented. Clipboard image paste depends on the receiving application and its supported formats. The app creates no firewall exceptions automatically.
- Keep awake covers display-off and idle sleep only; it does not suppress Windows screen savers, security locks or user-requested sleep. Notifications follow Windows notification policy.
- Input overload, stale work or native failure ends the session and releases local input rather than replaying a backlog. The local processing/send budget is 200 ms; a low network RTT does not rule out native processing delays.

## Build from source

Use a native Windows PowerShell terminal for the desktop build. Install:

1. [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with **Desktop development with C++** and a Windows SDK.
2. [Rust via rustup](https://www.rust-lang.org/tools/install), using the MSVC host toolchain. This workspace requires Rust **1.99 or newer** and is tested with 1.99.
3. [Node.js LTS](https://nodejs.org/en/download), for development tooling.
4. [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/), if absent.

These follow [Tauri's Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows).

```powershell
git clone https://github.com/bigsk1/easy-mouse-connect.git
cd easy-mouse-connect
rustup component add rustfmt clippy
.\scripts\setup-windows.ps1 -InstallDevDependencies
.\scripts\check.ps1
.\scripts\dev.ps1
```

If PowerShell policy blocks scripts, inspect them and use `Set-ExecutionPolicy -Scope Process Bypass` in that terminal before retrying. The helpers do not change machine policy or install system prerequisites.

Build the Windows installer with:

```powershell
.\scripts\build-windows.ps1
```

Tauri writes the NSIS installer under `target\release\bundle\nsis\`. The first build requires network access for dependencies and packaging tools. See [preview packaging](packaging/README.md) for the Preview installer recipe used by downloadable releases.

Portable checks can run without desktop WebView dependencies:

```sh
cargo fmt --all -- --check
cargo test --locked -p emc-protocol -p emc-core -p emc-platform
cargo clippy --locked -p emc-protocol -p emc-core -p emc-platform --all-targets -- -D warnings
node --check ui/app.js
```

Run browser checks with `npm ci`, `npx playwright install chromium`, then `npm run test:ui`. Use `npm run preview` for the settings preview. Browser tests use synthetic native responses; they do not establish native Tauri or two-PC behavior. Windows `scripts/check.ps1` also selects six isolated native API contracts; those install no hooks, inject no real input and access no app profile.

See [CONTRIBUTING.md](CONTRIBUTING.md), [development checks](docs/development.md), [architecture](docs/architecture.md), [protocol](docs/protocol.md) and [manual acceptance cases](docs/manual-testing.md) for development guidance. Input, crypto and networking live in `crates/`; the Tauri shell is in `apps/desktop/`, and the vanilla settings interface is in `ui/`.

## License

Copyright (c) 2026 bigsk1. Licensed under the [MIT License](LICENSE).

