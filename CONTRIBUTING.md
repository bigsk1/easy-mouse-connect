# Contributing

The initial supported desktop is Windows x64. Protocol/core development can happen on other platforms, but user input behavior must be validated on Windows. Keep native OS details in `emc-platform` and keep the settings WebView out of networking, cryptographic state and per-event input forwarding.

## Working locally

Read [README.md](README.md) for prerequisites and commands. Run formatting, Clippy and the appropriate tests before proposing a change. Use `scripts/check.ps1` on Windows; portable crate commands are listed in the README. Record the Windows cases you exercised from [the manual matrix](docs/manual-testing.md), including your mouse model, Windows version, display arrangement and network path.

For interface changes, run the persistent browser checks with `npm run test:ui` after `npm ci` and `npx playwright install chromium`. These are development dependencies; they do not add a browser-test runtime to the installed application. The synthetic bridge suite does not establish native Tauri IPC correctness. See [development troubleshooting](docs/troubleshooting.md) for browser setup and [the protocol](docs/protocol.md) before changing messages or session ownership.

Add meaningful regression tests for protocol, authentication, reconnect, bounded parsing and file-policy changes. Interactive hardware behavior needs a manual reproduction alongside any unit test. Tests should use loopback and temporary directories rather than real device keys or the user's clipboard.

Windows `scripts/check.ps1` explicitly runs six normally ignored native API contracts through `scripts/test-native-input.ps1`; CI does the same in its Windows core job. They exercise owned hidden windows, process-local registration, synthetic wake/message ordering, original deadlines, production source admission and asynchronous insertion ownership/cleanup with a fake sink, with no hooks, real input injection or application profile. `scripts/test-raw-registration.ps1` remains the narrow two-test selector. Keep actual API tests when changing native contracts: portable fixtures and cross-compilation missed the 0.3.1 flag-readback regression. Do not substitute a launch over someone's paired installation for an isolated test.

## Keep changes focused

Keep one behavior change per patch. Preserve the ordered off-hook-thread insertion executor, current ownership/deadline checks and raw edge admission. Keep feature/UI work separate from input routing, geometry and recovery changes, and avoid incidental dependency upgrades or broad refactors in a specific fix.

A changed runtime needs a new version and package destination. Record the affected manual cases actually exercised; when hardware is unavailable, state that boundary explicitly. See [development and verification](docs/development.md) for the current checks and acceptance requirements.

## Boundaries to preserve

- A network peer remains untrusted until the selected handshake authenticates it.
- Never silently replace a trusted device's key or downgrade encryption.
- Never forward secrets, invitations, keyboard events or clipboard text into logs.
- Validate settings and Tauri command inputs in Rust. Rendering network names as HTML is unsafe.
- Keep hook callbacks short; no filesystem access, network waits, or UI work on the hook path.
- Bound queues and messages. File traffic must not wait in the input queue.
- Releasing input must work locally, including during connection failure.
- Keep files inside the receiving directory, create without overwrite and handle partial transfers explicitly.
- New privileges, default-on sharing, remote content in WebViews and service installation need an explicit design/security discussion.

Use narrowly scoped dependencies with a reason for each addition. Check licenses before proposing dependencies. Do not commit identities, pairing invitations, test secrets, machine addresses, clipboard data, downloaded files, build output or local configuration.

Keep local build histories and personal investigation notes in the ignored local-notes/ directory. Review diagnostic exports and screenshots before attaching them publicly; numeric diagnostics can describe a display setup even without user content. Source/package exports must use the allowlisted helper in [packaging](packaging/README.md).

## Pull requests

Describe the user-visible problem, resulting behavior, validation performed and any remaining limitation. Include screenshots for interface changes and reproducible steps for native behavior. Security-sensitive fixes should use the private process in [SECURITY.md](SECURITY.md) before a public pull request exposes an unpatched vulnerability.

This project is licensed under the [MIT License](LICENSE), copyright 2026 bigsk1. Preserve its notice when redistributing source or substantial portions of the software.

