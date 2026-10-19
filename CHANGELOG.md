# Changelog

## 0.4.1 — clipboard and desktop sharing preview

- Support copying one local PNG, JPEG or static WebP file from Explorer through the independent image clipboard permission. Bound source files to 16 MiB, retain the 16 MiB PNG/64 MiB RGBA/16,384-pixel limits and convert to a generated PNG without preserving the source filename/format or adding another prompt. Folders, multiple-file selections, UNC and mapped network files remain unsupported; original-file transfer still uses explicit **Send file** and recipient approval.
- Correct clipboard baseline transitions between text and images so a new copy is not swallowed when its kind changes, and an earlier image can be copied again. Mark received image clipboard representations to suppress metadata/reflection echoes.
- Reduce optional tray notifications: receive-only clipboard messages with a 30-second cooldown, network messages with a five-second cooldown, and discard queued events older than five seconds. Keep messages content-free and disabled by default.
- Retain `EMC02` compatibility with 0.4.0, paired identities, the native routing/injection baseline, text limits, file consent and image/cache bounds. Use the same 0.4.1 build on both PCs for the fixes.

The locally built 0.4.1 preview has a [limited two-PC report](docs/manual-testing.md#confirmed-041-local-build-use) of approximately half a day behaving as expected. One keyboard interruption recovered after reselecting its Bluetooth host; the cause is unconfirmed. This is normal-use evidence for the retained local artifacts, separate from CI-produced release bytes and the broader [feature matrix](docs/manual-testing.md#041-feature-acceptance-checklist). Earlier 0.3.7 and 0.4.0 reports retain their own scopes; the published installer needs its separate download/install smoke check.

## 0.4.0 — local feature preview, unpublished

- Add independent opt-in image clipboard sharing for screenshots and supported copied images. Transfer canonical PNG up to 16 MiB with 64 MiB decoded RGBA and 16,384-pixel dimension bounds; publish bitmap formats and a bounded marked PNG cache for Explorer paste.
- Add a Troubleshooting tab with explicit Windows Firewall setup. A user-selected Windows approval launches a fixed helper for the current app, one peer IP, inbound TCP 24859 and Private/Domain profiles; the app remains unelevated and adds no rule automatically.
- Replace the Devices promotional panel with an equal-height Preferences column. Add optional peer keep-awake for display-off/idle sleep during local activity and rate-limited content-free clipboard/network tray notifications. Windows screen savers and security locks remain effective.
- Make the Windows Preview footer open the fixed upstream GitHub repository.
- Version the wire preamble to `EMC02` for image/activity messages. Update both PCs together; 0.3.7 does not interoperate with 0.4.0. Preserve version-1 DPAPI identities/settings and paired keys without Forget device. The explicit outbound `emc1` import alone retains its old `EMC01` header; short-code pairing never downgrades.
- Keep text clipboard at 1 MiB and explicit app file transfers strictly below 100,000,000 bytes with recipient consent. Native routing, hook, edge, geometry and ordered insertion behavior remain the preserved baseline.

This preview remains unpublished. A limited [two-direction Snipping Tool-to-Explorer check](docs/manual-testing.md#confirmed-040-image-copy-checks) is reported for 0.4.0; it does not establish Explorer image-file copy, repeated cross-kind reliability or the broader feature matrix. The published 0.3.7 checks remain a separate tested release reference. Current follow-up uses [0.4.1](docs/manual-testing.md#041-feature-acceptance-checklist).

## 0.3.7 — policy and release preview

- Honor Stop/Pause and file/clipboard permission reductions for the current run when saving settings fails; report the persistence error without enabling unsaved permissions.
- Bind clipboard baselines and pending reads/sends to their original control, bulk and policy revisions. Fast off/on transitions and reconnects establish a new baseline instead of sending pre-existing clipboard text.
- Add focused policy regressions, an AI-assisted source security review, and Windows compatibility/acceptance guidance.
- Prepare the Preview installer, portable executable, instructions, sanitized source and checksums in CI. An owner-requested manual run creates an unpublished GitHub prerelease draft after validation. Pin official Actions to verified commits and preserve byte-identical source across Windows/Linux runners.

Native hooks, the ordered injection worker, protocol, encryption, pairing, identity format and dependencies are unchanged from 0.3.6. The source review is not an independent professional audit. The published 0.3.7 GitHub build has [reported two-PC checks](docs/manual-testing.md#confirmed-037-release-checks) for crossing, typing, rapid scrolling/movement, basic file transfer and remote-to-local text clipboard. The wider matrix remains incomplete; 0.3.5 remains the preserved native-input comparison baseline.

## 0.3.6 — maintenance preview

- Upgrade diagnostic ZIP support from `zip` 2.6.1 to 9.0.5, outside the affected ranges in [RUSTSEC-2026-0367](https://rustsec.org/advisories/RUSTSEC-2026-0367.html), [0368](https://rustsec.org/advisories/RUSTSEC-2026-0368.html) and [0369](https://rustsec.org/advisories/RUSTSEC-2026-0369.html). These advisories concern extraction; production uses ZIP writing for diagnostic exports. The existing export test handles the updated filename API.
- Wait for rendered navigation in browser regressions and exercise deferred hash-change delivery. Failed Windows UI checks retain the isolated failure screenshot in Actions artifacts.
- Configure Dependabot for security-update PRs only. Routine dependency migrations remain deliberate maintenance changes.

Input routing, the ordered injection worker, encryption, pairing, persistent identities and clipboard/file policies are unchanged. Crypto/randomness dependencies retain their existing versions. Use the same version on both PCs; hardware observations from 0.3.5 do not establish acceptance for a new build.

## 0.3.5 — initial public preview

Windows x64 source preview with encrypted mouse/keyboard sharing, optional text clipboard and approved file transfers, tray controls and bounded local diagnostics. The full hardware acceptance matrix and independent security review remain incomplete.
