# Development and verification

Easy Mouse Connect is a Windows-first Rust/Tauri application with a static settings UI. Build instructions are in the [README](../README.md); the [architecture](architecture.md), [protocol](protocol.md) and [security policy](../SECURITY.md) describe the contracts changes must preserve.

## Run the checks

The workspace requires Rust **1.99 or newer**. Keep Cargo/npm lockfiles and use locked dependency resolution. On Linux/WSL or another development platform, run from the repository root:

```sh
cargo fmt --all -- --check
cargo test --locked -p emc-protocol -p emc-core -p emc-platform
cargo clippy --locked -p emc-protocol -p emc-core -p emc-platform --all-targets -- -D warnings
node --check ui/app.js
python -m unittest discover -s tests -p 'test_*.py'
```

Portable tests exercise protocol, policy, framing, loopback sessions and synthetic native state. The source-staging tests use temporary fixtures to verify private-data exclusions, optional license/notice inclusion, checksums and refusal to overwrite an existing destination. Release-publisher tests use synthetic repositories and mocked GitHub calls to verify source/asset integrity, exact commit/tag checks and draft-only creation. They do not access a real application profile or publish a release.

Install and run the settings-interface checks:

```sh
npm ci
npx playwright install chromium
npm run test:ui
```

These browser checks cover five-page navigation, responsive/equal-height control layout, defaults for newly added permissions, independent image/text/file toggles, the fixed project opener, scoped firewall arguments and pending/error states through a synthetic command bridge. They do not establish native Tauri IPC, Windows hooks, clipboard locking or physical input behavior. See [troubleshooting](troubleshooting.md) for browser/runtime setup failures.

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

Version 0.3.5 remains the preserved native-input baseline. The published 0.3.7 GitHub build also has [reported two-PC release checks](manual-testing.md#confirmed-037-release-checks) for crossing, typing, rapid scrolling, very fast movement, basic file transfer and remote-to-local text clipboard through context-menu Copy and Ctrl+C. Keep that tested release as a reference for future surgical changes. These results do not establish the remaining policy/failure paths or every mouse, shell surface, display/DPI layout and network configuration. Network RTT does not measure end-to-end input latency; no automated suite substitutes for hardware acceptance or an independent security audit.

## 0.4.1 release evidence and feature acceptance

The locally built 0.4.1 preview has a [maintainer report](manual-testing.md#confirmed-041-local-build-use) of approximately half a day of two-PC use behaving as expected, with one keyboard interruption recovered by reselecting its Bluetooth host. Its cause is unconfirmed and the report does not pass every matrix row. Preserve those local artifacts/source hashes, the published 0.3.7 packages and their [reported checks](manual-testing.md#confirmed-037-release-checks), and the separate [0.4.0 image-copy result](manual-testing.md#confirmed-040-image-copy-checks) with their original scopes.

Current channels use the `EMC02` preamble for appended image and activity messages; 0.3.7 uses `EMC01`. Versions 0.4.0 and 0.4.1 retain the same wire format, but use the same 0.4.1 build on both PCs for the fixes. When moving from 0.3.7, update both together. The version-1 DPAPI store and pinned peer keys remain intact, so a wire update does not require Forget device. Only an explicit outbound `emc1` import uses the old channel-00 header; short-code failure never downgrades. See [protocol](protocol.md).

Image regressions need encoded/decoded/dimension bounds, PNG/bitmap conversion, transparency/orientation, cross-kind baseline transitions, same-image local recopy, received-marker echo suppression, generation/policy revocation and Windows clipboard contention. Single-file imports must independently bound source bytes to 16 MiB, admit only a regular local PNG/JPEG/static WebP, reject network/folder/multiple-file sources and transfer canonical pixels without preserving source names/formats. Native tests use only owned synthetic clipboard fixtures with preservation/restoration where applicable, never production user content. Image cache tests use temporary paths and verify the four-entry bound, generated names, zone marking and referenced-file preservation. The cache holds private images and is excluded from source/export allowlists.

Keep-awake tests distinguish authenticated activity leases from display/sleep requests and Windows screen saver/security locks. Verify both-side opt-in, 30-second local activity eligibility, three-second signaling, 15-second receiver expiry and immediate lifecycle/policy release without synthetic input or a service. Notification tests verify receive-only clipboard events, the 30-second clipboard/five-second network cooldowns, five-second event expiry, disabled-state handling and fixed content-free messages. Firewall tests validate literal endpoints, fixed executable/helper/port authority, Private/Domain peer scope, cancellation and no broad rule changes; actual UAC/rule application remains a separate user-approved native test.

The focused [manual cases](manual-testing.md#041-feature-acceptance-checklist) cover the new features and input regressions alongside them. A native package or a synthetic browser bridge does not pass these cases by itself. Stage an allowlisted source snapshot and matching package hashes into a new destination; do not overwrite the retained 0.3.5/0.3.7/0.4.0/0.4.1 acceptance artifacts. Publication of 0.4.1 is authorized following the local report. CI builds from the release commit, so compare its runtime inputs with the tested snapshot and record its artifact/source hashes separately rather than claiming byte identity. Complete a short downloaded-package smoke check after publication and add the actual results; this follow-up does not convert the broader matrix into a release gate or an assumed pass.

## Source and package hygiene

Dependabot is configured for security-update PRs only. Repository vulnerability alerts and automated security fixes stay enabled; scheduled version-update PRs have an open limit of zero. Routine major upgrades are deliberate maintenance changes, with upstream migration guidance and relevant compatibility tests. Security fixes still require review and passing checks; the bot does not automatically merge them.

The repository's active `main` ruleset requires a reviewed pull request, resolved review conversations and the Ubuntu core, Windows core and Windows desktop/installer checks. It blocks force pushes and branch deletion. Repository administrators have an explicit bypass; other collaborators follow the rules. Keep required check names synchronized if the workflow jobs are renamed. Workflows from external contributors require maintainer approval, and workflow tokens retain read-only default permissions.

Use `scripts/stage-preview.py` as described in [packaging](../packaging/README.md) to create an allowlisted source ZIP and package manifest in a new destination. Review the archive before publication. Never commit runtime identities, invitations, diagnostics, cached clipboard images, user files, environment/signing keys or local tooling configuration. The ignored `local-notes/` directory is for local working records and is excluded from source exports; public documentation must not depend on it.

User-facing documentation describes current behavior. Keep private test chronology and machine-specific investigation notes out of the public tree. Use Git commits and narrowly scoped release notes to explain product changes. Changes to protocol/persistence need compatibility and recovery tests before shipping; never replace trust or delete an identity to hide an upgrade failure.

The repository is [MIT licensed](../LICENSE). Report exploitable security issues through the [private vulnerability-reporting form](https://github.com/bigsk1/easy-mouse-connect/security/advisories/new), and ordinary bugs through [GitHub Issues](https://github.com/bigsk1/easy-mouse-connect/issues).

See [release preparation](releases.md) for the owner-requested CI draft workflow and [the source security review](security-review.md) for review scope and policy fixes.
