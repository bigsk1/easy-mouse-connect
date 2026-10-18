# Development and verification

Easy Mouse Connect is a Windows-first Rust/Tauri application with a static settings UI. Build instructions are in the [README](../README.md); the [architecture](architecture.md), [protocol](protocol.md) and [security policy](../SECURITY.md) describe the contracts changes must preserve.

## Run the checks

The workspace requires Rust **1.99 or newer**. Keep Cargo/npm lockfiles and use locked dependency resolution. On Linux/WSL or another development platform, run from the repository root:

```sh
cargo fmt --all -- --check
cargo test --locked -p emc-protocol -p emc-core -p emc-platform
cargo clippy --locked -p emc-protocol -p emc-core -p emc-platform --all-targets -- -D warnings
node --check ui/app.js
python -m unittest discover -s tests -p test_stage_preview.py
```

Portable tests exercise protocol, policy, framing, loopback sessions and synthetic native state. The source-staging tests use temporary fixtures to verify private-data exclusions, optional license/notice inclusion, checksums and refusal to overwrite an existing destination. They do not access a real application profile.

Install and run the settings-interface checks:

```sh
npm ci
npx playwright install chromium
npm run test:ui
```

These browser checks cover rendering, responsive layout and a synthetic command bridge. They do not establish native Tauri IPC, Windows hooks, clipboard locking or physical input behavior. See [troubleshooting](troubleshooting.md) for browser/runtime setup failures.

On native Windows, use the documented prerequisites and run:

```powershell
.\scripts\setup-windows.ps1 -InstallDevDependencies
.\scripts\check.ps1
.\scripts\build-windows.ps1
```

`check.ps1` runs six isolated native contracts through `test-native-input.ps1`: raw-registration readback/cleanup, message/wake order, original input deadlines, production source admission and asynchronous insertion ownership/cleanup with a fake backend. These tests use owned windows and process-local registrations; they do not install hooks, inject real input or open an app identity. `test-raw-registration.ps1` selects the two registration contracts separately.

## Validate a change

Keep one behavior change per patch. Add meaningful regression coverage for protocol, policy, concurrency or native-contract changes, and run the applicable existing checks. Keep UI/features separate from input routing changes and avoid unrelated dependency upgrades. Every `SendInput` operation must remain on the bounded ordered worker outside the hook/raw-input thread; acknowledgment follows actual completion and guarded ownership commit.

Use the [Windows acceptance matrix](manual-testing.md) for physical testing. At minimum, an input change needs both crossing directions, keyboard/modifier release, scrolling, idle return, local reclaim and Ctrl+Alt+Escape, plus the affected display/application case. File changes need consent, bounds, interruption, integrity, safe destinations and concurrent input. Report platform/build, mouse connection, relevant display/network configuration, exact revision and which cases actually ran.

Preview 0.3.5 has limited two-PC mouse/keyboard/scrolling and basic Send file confirmation. The 0.3.6 maintenance preview still needs physical acceptance. Those observations do not establish every mouse, shell surface, display/DPI layout, network recovery path or transfer-policy case. Network RTT does not measure end-to-end input latency. Keep untested cases explicit; no automated suite substitutes for hardware acceptance or an independent security audit.

## Source and package hygiene

Dependabot is configured for security-update PRs only. Repository vulnerability alerts and automated security fixes stay enabled; scheduled version-update PRs have an open limit of zero. Routine major upgrades are deliberate maintenance changes, with upstream migration guidance and relevant compatibility tests. Security fixes still require review and passing checks; the bot does not automatically merge them.

The repository's active `main` ruleset requires a reviewed pull request, resolved review conversations and the Ubuntu core, Windows core and Windows desktop/installer checks. It blocks force pushes and branch deletion. Repository administrators have an explicit bypass; other collaborators follow the rules. Keep required check names synchronized if the workflow jobs are renamed. Workflows from external contributors require maintainer approval, and workflow tokens retain read-only default permissions.

Use `scripts/stage-preview.py` as described in [packaging](../packaging/README.md) to create an allowlisted source ZIP and package manifest in a new destination. Review the archive before publication. Never commit runtime identities, invitations, diagnostics, user files, environment/signing keys or local tooling configuration. The ignored `local-notes/` directory is for local working records and is excluded from source exports; public documentation must not depend on it.

User-facing documentation describes current behavior. Keep private test chronology and machine-specific investigation notes out of the public tree. Use Git commits and narrowly scoped release notes to explain product changes. Changes to protocol/persistence need compatibility and recovery tests before shipping; never replace trust or delete an identity to hide an upgrade failure.

The repository is [MIT licensed](../LICENSE). Report exploitable security issues through the [private vulnerability-reporting form](https://github.com/bigsk1/easy-mouse-connect/security/advisories/new), and ordinary bugs through [GitHub Issues](https://github.com/bigsk1/easy-mouse-connect/issues).
