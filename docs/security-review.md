# Source security review

A manual AI-assisted source review on 2026-10-18 examined version 0.3.6 at commit `20391faa1bb115ef6d6ff50553c39236ba23031e`. Three agents reviewed authentication/core policy, native input and application boundaries, with a separate integration review of the findings. Two policy defects led to the focused fixes in 0.3.7. This is a source assessment with regression evidence, not an independent professional security audit, penetration test or proof that all vulnerabilities have been excluded.

## Scope and trust boundary

The review covered first-party production Rust, the vanilla settings interface, Tauri permissions/CSP, diagnostics, persistence and packaging scripts. Reviewers traced initial pairing and pinned reconnect, framing and bounds, control/bulk authorization and revocation, clipboard/file handling, native input ownership and cleanup, frontend authority, diagnostic privacy and source-export exclusions. The existing [threat model](threat-model.md) and [security policy](../SECURITY.md) define the intended boundaries.

A paired PC intentionally receives control of the signed-in desktop when input sharing is enabled. Compromise of that PC or the current Windows account is outside the protection offered by encryption. The review did not find a confirmed unauthenticated desktop-control path, plaintext downgrade, peer-controlled path traversal or peer-string execution in the settings WebView. That statement describes this review's findings, not a guarantee that such flaws cannot exist.

## Findings addressed in 0.3.7

| Finding | Conditions and impact | Change and regression coverage |
| --- | --- | --- |
| Failed settings persistence could undo a requested revocation | A disk/permission/write failure during Stop/Pause or a feature-disable action could restore the previous in-memory settings before disconnect/revocation ran. An already paired peer could retain authority after the error. This did not bypass pairing. | Requested reductions remain active for the current run and retire the relevant channel/decisions even if saving fails. Unsaved permission increases are refused. The error explains persistence and restart behavior. Isolated tests cover full stop, file/clipboard revocation and failed permission increases. |
| Clipboard baseline/read could outlive its activation | A fast clipboard off/on or reconnect between polls could retain an old baseline. Existing clipboard text could then be treated as a new update; an asynchronous read could be adopted by a later session. Clipboard sharing and an authenticated peer were prerequisites. | Baselines and pending reads/sends carry control/channel generations and an activation revision. Each new context establishes a baseline and discards obsolete reads. The sender captured before reading is retained throughout the transfer. Tests cover missed off/on, reconnect, reads crossing either change, old-sender refusal and existing credential/echo filters. |

These changes are confined to core policy. Native hooks, raw input, the ordered insertion worker, wire encoding, cryptography, pairing, identity format and dependency versions are unchanged from 0.3.6. Existing identities and pairing remain compatible. The tests use temporary profiles, synthetic clipboard strings and fake channels; they do not access real clipboard contents or input devices.

## Verification and remaining work

The focused policy tests run on the portable unsupported controller and are excluded on Windows so they cannot install native hooks. CI separately builds/tests the Windows crates and isolated native API contracts, tests the browser's synthetic bridge, and builds the native desktop/installer. Those boundaries complement each other; a portable policy test is not a native Windows clipboard or DPAPI test. See [development checks](development.md) for repeatable commands and [release verification](releases.md#verification-status) for build and physical-acceptance scope.

Physical Windows 11 acceptance of 0.3.7 remains pending. The limited 0.3.5 input/file observations do not validate clipboard transitions, persistence failures or every driver/display/network case in a new build. The [manual matrix](manual-testing.md) records the relevant cases.

Dependency review includes the locked graph and repository advisory status. The cross-platform lockfile retains the known Linux GTK `glib` advisory documented in [SECURITY.md](../SECURITY.md#release-review); that dependency is absent from the Windows target graph. Do not interpret a Windows preview as approval to ship the affected Linux desktop graph. JavaScript packages are development tools, rather than shipped settings-interface runtime dependencies.

This assessment did not independently audit cryptographic libraries, Windows, WebView2, mouse/vendor drivers or compiler infrastructure. It did not perform fuzzing, a sustained hostile-network campaign, a live two-PC penetration test or a reproducible-build comparison. Releases remain unsigned; checksums identify bytes and do not establish a publisher signature. Source/asset validation and same-run CI packaging improve traceability without resolving those limits.

Report a suspected vulnerability through the [private reporting process](../SECURITY.md#report-a-vulnerability-privately). Avoid publishing pairing codes, identity files, user clipboard/file content or unredacted diagnostics.
