# UI regression checks

These development-only checks run Chromium against the static interface. They start a temporary preview subprocess on a loopback address and close both the server and browser when finished. The installed application gains no browser-testing runtime dependency.

From the repository root, install the locked development dependencies and the Playwright browser once:

```sh
npm ci
npx playwright install chromium
npm run test:ui
```

On Linux hosts that need Chromium system libraries, use `npx playwright install --with-deps chromium` in an environment where installing those packages is appropriate. The tests also run on Windows with the normal Playwright Chromium installation. No live peer, real invitation, local clipboard, or sample user file is needed.

The runner reports three separate suites:

- **Browser preview:** honest engine-unavailable state, disabled native controls, four-page navigation with deliberately deferred hash-change delivery, pairing dialog tabs, Escape/focus restoration, and layouts at 390, 760, 1024, and 1280 pixels without horizontal overflow. Navigation waits for the rendered destination and verifies its active link before inspecting page content; an immediate visibility query after a click can race the hash-change handler.
- **Synthetic startup failure:** a real synthetic startup error is displayed while local log metadata, explicit export and the known-folder command remain usable; connection/input controls remain unavailable.
- **Synthetic native bridge:** synthetic Tauri command responses exercise explicit pairing-code generation/copying, pairing arguments, typed inputs preserved across status polling, settings and arrangement changes, literal rendering of untrusted names/filenames/diagnostic text, native drop followed by an explicit Send, recipient consent, IPv6 diagnostic arguments, local log retention metadata and explicit export/folder commands, emergency stop, and unavailable-engine recovery.

Synthetic responses exist only in the test browser context. The fixtures contain synthetic pairing codes and invalid keys and fictional paths; they never authenticate or transfer real content. These tests cannot validate Rust/Tauri IPC permissions or serialization, cryptography, the byte limits enforced by Rust, Windows hooks/clipboard/DPAPI, or actual two-PC behavior. Continue running the portable Rust tests and [Windows acceptance matrix](../../docs/manual-testing.md).

Screenshots are optional on successful runs. Set `UI_TEST_ARTIFACTS_DIR` to a temporary directory to capture the desktop/narrow preview and pairing dialog. On failure, the runner saves the current page to that directory, or to a fresh temporary directory if the variable is absent. No test artifacts are written to the source tree by default.

The Windows Actions job sets this output directory and uploads `failure.png` for seven days when the UI step fails. The screenshot contains only the isolated browser preview or synthetic fixture; the workflow does not collect desktop screenshots, application profiles or unrelated temporary files.

If Chromium already exists on the test machine, select it explicitly:

```sh
PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/path/to/chromium \
UI_TEST_ARTIFACTS_DIR=/tmp/emc-ui-artifacts \
npm run test:ui
```

PowerShell equivalent:

```powershell
$env:PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH = 'C:\path\to\chrome.exe'
$env:UI_TEST_ARTIFACTS_DIR = Join-Path $env:TEMP 'emc-ui-artifacts'
npm run test:ui
```

The direct runner command is `node tests/ui/run.mjs`. A missing browser can be installed with `npx playwright install chromium`. Browser launch failures in restricted execution environments may require an approved local process/network permission; the runner does not change sandbox, firewall, or system security settings.
