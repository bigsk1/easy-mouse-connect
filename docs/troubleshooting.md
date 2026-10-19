# Troubleshooting

Start with the symptom below. Record the app version, affected direction and first actionable error. [Network setup](networking.md) covers routes, Tailscale and scoped firewall rules; [development checks](development.md) covers building and testing.

## Recover local control

Press **Ctrl+Alt+Escape** on the controlling PC. Physical input on the receiving PC also reclaims control: keydown, buttons and wheel are immediate, while mouse movement has a small jitter threshold. **Pause** or **Stop sharing** releases input and disables sharing until re-enabled.

To exit completely, choose tray **Quit Easy Mouse Connect**. The window's X only hides it. A second launch focuses the existing instance. Quit both tray apps before replacing executables or installing an update, then run the same version on both PCs. Keep each PC's own `%LOCALAPPDATA%\EasyMouseConnect\identity.dat`; upgrading a working pairing does not require **Forget device**.

## Settings cannot be saved

Version 0.3.7 keeps requested Stop/Pause and file/clipboard permission reductions active for the current run even when saving fails. The error explains that the last saved permissions may return after restart. Permission increases and unrelated layout changes are not applied on an unsuccessful save. Use tray Quit if sharing must remain stopped, then check available storage and the current user's write access to the app profile. Preserve `identity.dat`; deleting identity data is not a settings-write repair.

## The PCs cannot connect

1. Confirm that the receiving app is running, sharing is enabled, and the entered IP is the intended peer's reachable address. The listener uses TCP **24859**.
2. Run the TCP check in [network setup](networking.md). A successful TCP connection proves reachability, not pairing or input authority. Ping failure alone is inconclusive.
3. Check routing and host/network policy. Different LAN subnets need a route. Unrelated private networks need an existing VPN or other authorized route; entering a LAN IP does not create one. For Tailscale, both peers must be online, authorized and allowed by tailnet/host policy.
4. Check the Windows network profile and any peer-scoped firewall rule. The app does not open firewall rules automatically. Do not disable Windows Firewall or expose the listener through internet port forwarding to make a test pass.

The PC that originally entered the pairing code initiates automatic reconnect attempts every three seconds while enabled. Use manual **Reconnect** on that PC. The code-generating PC listens; it does not take over dialing after an outage. Displayed LAN/Tailscale addresses are address hints, not proof of a usable route, and can lag a network change by up to the normal 30-second refresh.

## Pairing fails or trust differs

On the destination, use **Devices → Pair a device → Show a pairing code → Generate code**. Type its 12 characters and reachable IP on the initiating PC. Hyphens/capitalization are handled for you. A code lasts five minutes and admits at most five attempts; failed or abandoned attempts consume that budget. Generate a fresh code after either limit. Update both PCs before creating a new pairing.

A changed device key must fail verification. Confirm that the correct per-user profile is running; do not delete identity data or bypass the mismatch. Versions 0.2.0 through 0.3.7 retain established paired identities/settings. If a new pairing was interrupted and only one side remembers the other, explicitly forget the incomplete pairing and pair again with a fresh code. Do not use that procedure for a working pair merely to upgrade.

An old `emc1` invitation is not a current short code. Current receivers do not accept new legacy-token pairing, and failed short-code pairing does not fall back to it or to plaintext. An established connection closing is also not proof of a failed handshake: route loss, local pause and native input failures can end an authenticated session. A locally known pause has its own policy category; remote EOF alone does not identify the peer's reason.

## Connected, but the pointer will not cross

Set **Peer on left/right** to the other PC's position. The two settings should be opposite. For example, the right-hand PC selects **Peer on left** and the left-hand PC selects **Peer on right**. This setting does not rearrange monitors inside Windows.

Enable **Desk sharing** and **Move across screen edges**. Release local keys and mouse buttons, move the physically connected mouse, then push outward midway along an exposed configured side edge. Crossing uses real Windows display rectangles, including stacked displays, negative origins and gaps. Internal seams between a PC's own monitors stay local. A 16-pixel corner/offset-shoulder margin blocks accidental handoff; vertical movement alone does not switch PCs. Slow/diagonal pushes and a pause at the edge are supported without a timed gesture.

If Windows skipped both the receiver's entry sample and inward progress, move inward before deliberately returning through the shared edge. A visible pointer parked on the sending PC is a known limitation; confirm **Sending/Receiving/Local control** before treating it as a routing failure.

Use **Diagnostics** to locate the stage:

| Measurement | What to check |
| --- | --- |
| Raw input registered / Raw motion packets | Registration should remain available and physical relative-motion counts should rise on the controller. Historical packets cannot override a currently wrong raw target. Unsupported absolute devices release control. |
| Raw ownership query failures | Registration inspection was unavailable; admission is suspended without blind re-registration. |
| Raw ownership mismatches | An inspected target, device class or delivery mode was incompatible. |
| Raw registration failures / repairs | Failed repair or verification versus successfully verified recovery. Repeated failures back off rather than retrying every second. |
| Source edge checks / Eligible edge samples | Eligible samples passed native-readiness, held-input and geometry checks. |
| Outward gestures ready / Source entries committed | Local pressure reached the threshold versus completed local source preparation. Manual activation also counts as a source entry. |
| Remote entries committed / Returns through edge | Successful guarded receiver entry versus deliberate receiver edge return. A general capture reset is not necessarily an edge return or a fault. |
| Forwarded moves / Injected moves | Activity is captured on the controller and inserted on the receiver. A local commit alone does not prove peer acceptance. |

Counters are cumulative for the process, can differ across PCs, and reset at restart. Export both PCs' ZIPs after a recurrence rather than interpreting a single counter as a complete event history.

## Keyboard stays local or a held control blocks crossing

Keep the mouse and keyboard hardware connected to the intended controller. Hardware-switching them to the receiver is separate from the app's routing. Compare **Local keyboard events**, **Forwarded key events** and the peer's **Injected key events**, along with the routing state and local-reclaim counters.

The app protects physically held keys/buttons. After a device switch or secure-desktop transition, the window/tray may ask for a local release. Release the actual control; if its UP was missed, press and release the affected modifier or mouse button locally. Another device's same-key hold must remain protected. A cleanup keyup alone does not reclaim remote control; a genuine physical keydown does.

Contributing, held, unknown or ambiguous device removal can release routing. A known unused secondary device may be ignored. The mere presence of a laptop keyboard is not input. Physical receiver mouse movement reclaims after eight relative hardware units of net displacement within 200 ms; these units are not screen pixels. Physical buttons/wheel and keydown still reclaim immediately.

Inspect hook-delivery suspicions, recoveries and failures. Hook-installed flags describe the last installation result, not proof of current delivery. Raw-registration repair counts describe a separate failure path. For another application's shortcut problem, **Quit** is the stronger isolation test: pausing sharing leaves hooks installed. Exit other input-sharing utilities during an isolated test so competing capture paths do not obscure the result.

## Scrolling, stuttering or unexpected returns

Compare **Forwarded wheel events** and **Injected wheel events** with capture stalls, local-reclaim counters, input queue pressure and native completion failures. The app preserves small, negative and larger wheel deltas and horizontal/vertical distinctions. Do not infer a driver fault or change Windows/vendor scroll settings from one counter. Record the current mouse model, transport, available wheel mode and known vendor software.

Network RTT measures a round trip, not hook scheduling, insertion, painting or end-to-end responsiveness. A stale RTT sample is hidden. Establish a wired LAN comparison, then check Tailscale's direct/relay path and packet loss when relevant.

Each control queue is bounded to 128 entries. Adjacent moves can merge, but wheel/button/key/entry/release actions remain ordering barriers. Stale or overloaded work disconnects and releases input rather than replaying a backlog; the local processing/send budget is **200 ms**.

| Diagnostic | Interpretation |
| --- | --- |
| Source queue expired | Input was too old before source transmission. |
| Send deadline exceeded | The source exceeded its write deadline. |
| Receive queue expired | Input was already old before native application. |
| Native completion timed out | Native completion was missing or late. These four counts sum to Stale input releases. |
| Longest native apply / queue wait / execution | Different intervals and cumulative maxima; they cannot be added to reconstruct one event. |
| `native_sendinput_*` / `native_wheel_peak_us` | Insertion-backend time, narrower than the whole asynchronous native interval. It includes thin wrappers and applicable positioning work, not necessarily one isolated syscall or target-app response. |
| `input_entry_failed` with a native precondition/cursor/queue category | Operational failure before receiving could commit. Zero actual insertion failures does not exclude an earlier precondition failure. |

All Windows insertion and cleanup run on one ordered executor outside the hook/raw thread. A blocked Windows call can still time out; it cannot be forcibly withdrawn once accepted. Local ownership is revoked, with held-input cleanup following when the call returns. Do not increase timeouts, discard wheel events or start a parallel insertion worker to mask a repeatable stall. Use the normal local-release controls and collect diagnostics.

A source-only raw-progress guard can release when suppressed physical input stops reaching raw capture. Capture-stall counts describe that safety path; general capture resets can also reflect normal leave/reclaim. After routing or registration repair, fresh physical relative movement is needed before admission. Individual packet loss inside recent coalesced activity cannot be reconstructed from aggregate counters.

## Pointer visibility, shapes and shell surfaces

The receiver uses a click-through, non-activating cursor fallback when its OS pointer is absent or hidden. Check its status in **Diagnostics** and compare with a physical mouse attached to the receiver. The fallback should disappear on local reclaim/disconnect and must not capture clicks or focus. The app does not enable MouseKeys or change global cursor/registry settings.

Some shell, taskbar or fullscreen surfaces can cover an ordinary topmost fallback window. Native pointer availability and surface behavior vary; visibility on one surface is not a universal guarantee. The conservative fallback can also override an application's intentional hidden cursor. Source-pointer hiding is not implemented.

Cursor-resource failures invalidate receiving ownership and recover with bounded retries; recovery does not resurrect an old session. If sampling confirms a visible native pointer and present mouse, an optional fallback failure need not block entry. Return to the ordinary desktop after lock/UAC, inspect cursor failure/recovery counters and try crossing again with local input available.

UAC, the lock screen, secure desktops and elevated application control are outside the supported scope. Keep UAC enabled. Administrator mode does not fix queue, capture or rendering bugs and does not grant UIAccess or guarantee shell visibility. [Microsoft input-integrity rules](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput) and [UIAccess requirements](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-securityoverview) explain those boundaries.

## Clipboard is not syncing

Enable **Share plain-text clipboard** on both PCs and confirm the authenticated bulk channel is ready. Copy new non-sensitive text after connecting: enabling sharing does not immediately send the old clipboard. Only plain text up to **1 MiB of UTF-8** is supported; images and larger text are not. Standalone pairing-code-shaped text and legacy invitations are filtered.

If another app holds the clipboard open, retry after releasing it. Record failure counters and the exact local error without including clipboard contents. Do not dump the clipboard into a bug report or log.

## File transfer fails or the received file is hard to find

Enable **Allow file transfers** on both PCs, drop a regular file into the app and select **Send file**. The recipient must approve the offer. Files must be **smaller than 100,000,000 bytes**; the exact limit and larger files are rejected. Direct Explorer-to-Explorer dragging across PCs is not supported.

The default Windows destination is `%USERPROFILE%\Downloads\EasyMouseConnect\<transfer-id>\<filename>` on the receiver. Check the receiving directory displayed in **Sharing**. Each transfer creates its own generated child directory and never overwrites an existing file. This default uses the user-profile Downloads path; a redirected Windows Downloads folder may be elsewhere.

Approve within **65 seconds**. An incoming transfer expires **120 seconds after the offer was created**, so a very slow route or long approval delay can exceed its lifetime. Disconnects cancel partial transfers; they do not resume on a later connection.

Success requires exact length, SHA-256 verification, Windows internet-zone marking while the file is temporary, and a matching receiver acknowledgment. If acknowledgment times out, check the receiver's transfer directory before retrying: saving may have completed before the acknowledgment was lost. Compare hashes using non-sensitive fixtures. The app does not automatically open or execute received content.

A filesystem without the required Windows zone-metadata support can cause reception to fail. Check its capabilities and the local error; do not bypass marking to force success. Feature/pairing revocation or a changed connection also cancels transfer authority and must not be bypassed.

## Export logs for a report

Use **Diagnostics → Export diagnostic ZIP**, then **Open export folder**. Collect a ZIP from each PC soon after the problem. The output is `%LOCALAPPDATA%\EasyMouseConnect\diagnostic-exports\emc-diagnostics.zip`; every export replaces that one archive, so copy older captures elsewhere first if needed. A standalone `.log` lacks the aggregate counters and status summary. Nothing uploads automatically.

Sequential collection is valid: **export A while running → Quit A → switch Bluetooth hardware if needed → export B → Quit B**. Mention the sequence. The exports are separate snapshots; the first Quit can cause an expected disconnect/reconnect timeout on the other PC. Post-export events are absent from the already-created ZIP. Device changes and capture resets near collection do not, by themselves, prove the reported failure. Cross-PC clocks do not establish exact causality.

Include the app/Windows versions, affected direction, minimal steps, mouse/keyboard connection type and relevant monitor arrangement/scaling. Review ZIPs and screenshots before sharing. Do not attach pairing credentials, identity/settings files, clipboard contents or received user files. Use [GitHub issues](https://github.com/bigsk1/easy-mouse-connect/issues) for ordinary bugs and [private vulnerability reporting](https://github.com/bigsk1/easy-mouse-connect/security/advisories/new) for security issues, following [SECURITY.md](../SECURITY.md).

### Retention and interpretation

Logs live in `%LOCALAPPDATA%\EasyMouseConnect\logs` as `emc.log`, `emc.1.log` and `emc.2.log`. Each is at most **2,097,152 bytes**, totaling at most **6,291,456 bytes (6 MiB)**. The export ZIP is separate from that rolling-log cap. A bounded nonblocking queue feeds the writer; dropped-event/write-failure counts mean the history may be incomplete.

Records contain typed events, allowlisted categories and timing/version metadata. They exclude raw keys/input, typed text, clipboard/file content, credentials, user file names/paths, peer IPs and device names. Exports reparse and allowlist records and add numeric/boolean status, including aggregate counters, monitor count and virtual dimensions. They omit identity/settings files and local directory metadata. Operational metadata can still describe aspects of a setup, so review before sharing.

The summary's `app_version` identifies the exporter. Historical records retain valid original versions; missing/invalid versions stay absent. `logger_opened_at_unix_ms` identifies the current logger, while `latest_retained_engine_start` can belong to an older retained run. Current counters reset at process start. Versions and timestamps are operational metadata, not authenticated evidence.

If identity/native startup fails, Diagnostics can still export existing sanitized logs and reports `engine_available=false`; connection/input controls remain unavailable. Check the actual startup error. For export failure, inspect disk space, directory permissions and write-failure counts before retrying; do not add raw error strings or content to logs.

## Known issues in older versions

Use the same current version on both PCs and preserve a working pairing when upgrading.

| Affected version | Symptom and action |
| --- | --- |
| 0.3.1 | Green connection with rising raw-registration failures and no crossing. Upgrade both PCs; the registration check could reject valid Windows readback. Re-pairing or disabling Tailscale does not fix that predicate. |
| 0.3.3 | Healthy registration but almost no captured movement. Upgrade both PCs; the source gate could discard valid motion at a clamped edge. |
| 0.3.4 | Remote scrolling can trigger native-completion releases. Upgrade both PCs; 0.3.5 moves insertion off the hook thread. The older local-mouse reclaim counter covered movement only, so zero does not exclude button/wheel reclaim. |
| Exporters before 0.3.4 | Retained log lines can be mislabeled with the exporter's version. Their labels cannot prove which run produced each record. Use current-process counters and the recorded reproduction interval. |

## Build, WebView and browser-test failures

Use a native Windows MSVC toolchain, Rust **1.99 or newer**, Visual Studio **Desktop development with C++** plus a Windows SDK, Node development tooling and WebView2. A missing `link.exe`, SDK header or native library belongs to that prerequisite layer. The setup helper checks prerequisites; it does not install them or change machine policy. Follow [Tauri's Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows).

If the settings window cannot create its WebView, check WebView2 and the first native error. The browser preview does not establish native desktop health. Build/package helpers need network access on their first dependency/tool download. See [development checks](development.md).

For the static UI checks, run `npm ci`, `npx playwright install chromium`, then `npm run test:ui` from the root. Install Chromium in the same OS/user/environment used for testing; a Windows download is not a Linux/WSL browser. Linux Chromium may need host libraries; use Playwright's [browser/dependency guidance](https://playwright.dev/docs/browsers), including `npx playwright install --with-deps chromium` where appropriate for the development host. These are development dependencies, not shipped app runtime packages.

Run `npm run preview` for the settings-only preview. It binds to loopback and exposes no native engine. If port 5173 is occupied, use `npm run preview -- --port 5174`. Tests can use a compatible installed Chromium through `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH`; `UI_TEST_ARTIFACTS_DIR` selects temporary output. See [UI-test guidance](../tests/ui/README.md). Browser suites use synthetic native responses and do not prove Tauri IPC, hooks, clipboard or two-PC operation.

From a Windows development terminal, `scripts/test-native-input.ps1` selects six isolated API contracts serially; `scripts/test-raw-registration.ps1` selects two registration contracts. They use owned windows/registrations and synthetic inputs, install no hooks, inject no real input and access no app profile. Keep native contract tests separate from interactive acceptance in [the manual matrix](manual-testing.md).
