# Security policy

## Report a vulnerability privately

Use [GitHub private vulnerability reporting](https://github.com/bigsk1/easy-mouse-connect/security/advisories/new) to report a suspected vulnerability to the maintainers. Private reporting is enabled for this repository. Do not open a public issue with a working exploit or sensitive diagnostic material.

Include the affected version, Windows build, minimal reproduction, expected and observed behavior, and the access an attacker needs. Use synthetic data where possible. Keep pairing codes, legacy invitations, device private keys, identity files, clipboard content and received files out of reports. Redact screenshots and diagnostics before attaching them. Ordinary connection/setup bugs belong in [GitHub Issues](https://github.com/bigsk1/easy-mouse-connect/issues).

## Status and trust model

Easy Mouse Connect is a Windows 11 x64 preview for the current user session. An [AI-assisted source security review](docs/security-review.md) examined the 0.3.6 baseline and led to two policy fixes in 0.3.7. This is review evidence, not an independent professional audit or proof that all vulnerabilities have been excluded. Physical acceptance remains incomplete; there is no supported production release. Reproduce problems on the current preview where practical; this policy does not promise fixes for older builds or a response deadline.

Pairing grants the other PC the ability to control the signed-in desktop and receive the features explicitly enabled on both ends. Encryption does not protect against a compromised paired PC or a compromised Windows account. See the [threat model](docs/threat-model.md) for assumptions and boundaries.

## Authentication and data protection

- Pairing uses a random 12-character base32 code with 60 random bits, a five-minute lifetime and at most five admitted attempts. Failed or abandoned exchanges consume attempts. Keep the code private.
- RustCrypto SPAKE2 and transcript-bound HKDF-SHA256 derive the pre-shared key for Noise XXpsk3 device-identity exchange. Reconnect uses pinned identities with Noise IK. Every control and bulk channel authenticates independently; there is no plaintext fallback or silent key replacement.
- An address or device name is not proof of identity. Unknown or changed keys require explicit pairing. A failed short-code exchange does not downgrade to the legacy invitation path.
- Windows stores identity/configuration with current-user DPAPI. Each PC needs its own identity; do not distribute or copy profiles between devices. The restrictive-permission Unix store is a development fallback, not a Linux input backend.
- Clipboard sharing is off by default and accepts text up to 1 MiB. File sharing is off by default, needs explicit recipient approval, and rejects files greater than or equal to 100,000,000 bytes.
- Received files stay inside a generated child of the receive directory, never overwrite existing files, and require exact length and SHA-256 verification. Windows internet-zone metadata is applied before final rename; failure does not leave an unmarked final file. Content is never automatically opened or executed. The sender succeeds only after receiver confirmation.

The cryptographic implementation and this integration carry no independent-audit claim. See the [protocol](docs/protocol.md) for wire/transcript binding, limits and compatibility behavior.

## Desktop authority and failure handling

The app runs without a service or elevation. UAC/secure desktop, lock-screen and elevated-application control are outside its supported scope. It does not change firewall rules, global cursor settings, process priority or vendor mouse settings automatically.

Input capture, encrypted transport and ordered native insertion remain in Rust. Every `SendInput` operation runs on one bounded ordered worker outside the hook/raw-input thread. Acknowledgment follows actual insertion and guarded ownership commit, not enqueueing. Original deadlines, connection epochs and separate local ownership tokens reject obsolete work; stale input and queue overflow release/disconnect rather than replaying a backlog. An OS call already in progress cannot be forcibly interrupted, and accepted events cannot be withdrawn; tracked holds are cleaned when insertion returns.

Pause/Stop, disconnection, heartbeat loss, revocation and Quit end active authority and release input. Requested Stop/Pause and file/clipboard permission reductions apply to the current run even if saving settings fails; the error explains that the previous saved permissions may return after restarting. An unsuccessful save cannot enable new permissions. Clipboard baselines and pending reads/sends are bound to their original activation and connection/channel generations. Local reclaim and **Ctrl+Alt+Escape** provide local recovery. Closing the settings window keeps sharing active in the tray; use Pause or Quit when sharing should stop. A second launch focuses the existing instance.

## Logs and diagnostic exports

Local logs live under `%LOCALAPPDATA%\EasyMouseConnect\logs`. The three files `emc.log`, `emc.1.log` and `emc.2.log` are each capped at 2 MiB, for at most 6 MiB of retained log payload. A bounded nonblocking queue keeps disk work away from input callbacks; diagnostic events can be dropped rather than blocking input.

Records contain timestamps, versions, typed operational events and allowlisted failure categories. Summaries contain status flags, counters, durations and numeric monitor count/virtual dimensions. They do not record raw keys, typed text, pointer histories, clipboard/file contents, credentials, filenames/paths, peer addresses or device names. Operational metadata can still describe aspects of a setup, so inspect it before sharing. Logs are local files, not separately DPAPI-encrypted identity data.

Only an explicit **Export diagnostic ZIP** action creates `%LOCALAPPDATA%\EasyMouseConnect\diagnostic-exports\emc-diagnostics.zip`. Export reparses bounded records and keeps allowlisted fields; it does not copy arbitrary edited log bytes, configuration, identities or directory paths. The latest export replaces that one known ZIP. The app does not upload diagnostics automatically. The export-folder opener accepts no arbitrary frontend path.

## Release review

The cross-platform lockfile retains `glib` 0.18.5 through Tauri's Linux GTK dependencies, matching [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html). It is absent from the Windows target dependency graph. The GTK 0.18 dependency family does not accept the patched `glib` 0.20 release, so Dependabot cannot resolve this update independently; adding another direct version would not replace the affected dependency. Linux desktop distribution is unsupported; resolve the affected Linux dependency graph before introducing a Linux desktop release. Keep that platform-specific alert visible rather than treating a Windows-only release as a clean audit of every target in the lockfile.

Changes to authentication, parsing, transfer policy, revocation or native ownership need focused regression coverage and the relevant [manual checks](docs/manual-testing.md). Keep encrypted authentication on every channel, bounded queues/frames and current policy checks at installation and commit. Review dependency advisories/licenses, installer provenance/signing and persistence/protocol compatibility before a binary release. Do not describe a preview as audited or production-ready without evidence.
