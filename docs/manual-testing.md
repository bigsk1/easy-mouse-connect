# Testing on two Windows PCs

Use this checklist to test a build on your own pair of Windows PCs or to verify a change before release. Record the build and environment, then exercise the features and recovery paths relevant to your setup. Advanced fault-injection cases belong in an isolated development fixture, not an installed paired session.

**Current test status:** the retained local 0.4.1 preview has a limited maintainer report of approximately half a day of two-PC use behaving as expected. One keyboard interruption recovered after reselecting its Bluetooth host; the cause is unconfirmed. This local-build report and the separate CI-produced release installer smoke check have different scopes. The earlier local 0.4.0 has only the limited [Snipping Tool-to-Explorer image-copy confirmation](#confirmed-040-image-copy-checks) below. The published 0.3.7 GitHub preview has reported two-PC confirmation for screen crossing, typing, rapid scrolling, very fast mouse movement, basic app-based **Send file**, and remote-to-local text clipboard sharing. The [confirmed release checks](#confirmed-037-release-checks) describe exactly what is covered. Version 0.3.5 remains the preserved native-input comparison baseline; 0.3.7 has its own reported physical results. The full acceptance matrix remains incomplete.

Portable tests, browser fixtures and compilation complement these physical checks. Record each case actually exercised; a passed result on one setup or revision does not pass a different setup or later implementation. See [development and verification](development.md) for repeatable checks and [architecture](architecture.md) for the current input and session contracts.

Use non-sensitive accounts/content for the initial run. Keep a local keyboard/mouse on each PC. Verify the local emergency shortcut before moving control remotely. Use the same application revision on both PCs and preserve redacted failure details. Versions 0.4.0 and 0.4.1 share the `EMC02` wire format and do not connect to 0.3.7. Use the same 0.4.1 build on both PCs for the clipboard fixes, while retaining existing identities/pairing.

Before updating, fully Quit both processes from their tray menus; the window's X hides it instead of exiting. Install the same update on both PCs and preserve `%LOCALAPPDATA%\EasyMouseConnect\identity.dat`. An existing pairing should reconnect with its original pinned identity, without Forget device. Test fresh short-code pairing using separate clean test profiles when needed; never delete an established identity just to create a test case.

## Supported target and test records

Windows 11 x64 is the desktop target; both PCs need not run the same feature version or cumulative build. Record each PC's Windows version/build from `winver`, app version, WebView2 version, mouse/keyboard connection and relevant display/DPI/network settings. These details help reproduce failures without requiring every monthly build combination to be tested before a preview.

Two PCs are the minimum hardware setup, rather than the entire acceptance matrix. A representative preview check covers both directions, typing/modifiers and emergency release, fast/freewheel scrolling, the actual multi-monitor layout, Pause/Quit, reconnect after a network interruption or sleep/wake, and any enabled clipboard/file flow. Record broader edge cases below as passed, failed or untested. An automated Windows Server CI build is not a Windows 11 desktop acceptance result.

## Confirmed 0.3.7 release checks

Build under test: the published [GitHub v0.3.7 preview](https://github.com/bigsk1/easy-mouse-connect/releases/tag/v0.3.7), installed on both PCs in one Windows 11 x64 setup. These are maintainer-reported interactive results, separate from CI and the older 0.3.5 observations.

| Case | Reported result |
| --- | --- |
| GitHub preview installation | The release installs and runs on both PCs. |
| Screen crossing and return | Remote control and return to local control work, with behavior consistent with the working input baseline. |
| Keyboard input | Typing on the controlled PC works. Ctrl+C also works in the clipboard flows below. |
| Rapid wheel scrolling | Fast scrolling remains usable without an observed routing failure in these checks. |
| Very fast mouse movement | Very fast movement remains usable without an observed routing failure in these checks. No polling rate or end-to-end latency measurement is recorded. |
| App-based file transfer | Sending a file between the PCs completes successfully. Transfer size, both directions, limits and interruption cases are not established by this result. |
| Context-menu clipboard copy | Highlight text on the controlled PC, choose right-click Copy, cross back to the controlling PC and paste: the expected text arrives. |
| Keyboard clipboard copy | Highlight text on the controlled PC, press Ctrl+C, cross back to the controlling PC and paste: the expected text arrives. |

The clipboard results cover the controlled PC to the controlling PC; reverse-direction clipboard sharing is not established by this report. Exact Windows builds, WebView2 versions, per-display DPI, network path and test duration are not recorded for these release checks. Earlier setup details do not substitute for a completed environment record.

The wider matrix below remains available for subsequent checks. Clipboard off/on and reconnect boundaries, persistence failures, transfer bounds/consent/interruption/integrity, emergency release, sleep/wake, other display/driver configurations and routed/Tailscale paths need their own results. Ordinary successful use does not establish those failure-path or compatibility cases. Potential quirks without confirmed reproduction remain follow-up observations, rather than diagnosed regressions.

## Confirmed 0.4.0 image-copy checks

Build under test: the earlier local 0.4.0 preview on the two Windows PCs. The reported case is **copy an image with the Snipping Tool, then paste into File Explorer on the other PC**, repeated successfully in both directions. This is limited confirmation of the screenshot-to-Explorer workflow, not a claim of clipboard reliability over time or of all source/target formats.

Copying PNG/JPEG/WebP image files from Explorer is not established by this result and was reported not working with that build. Repeated transitions between text and images also require the new regression cases below. These observations do not pass 0.4.1, image bounds/cache/privacy/policy cases, original-file preservation or the wider input/feature matrix. The published 0.3.7 build retains its separate confirmed scope above.

## Confirmed 0.4.1 local-build use

Build under test: the retained locally built 0.4.1 preview, installed on the two Windows PCs. The maintainer reports approximately **half a day of use**, with behavior as expected. The report includes one keyboard interruption that recovered after reselecting the keyboard's Bluetooth host. There is no confirmed engine, Bluetooth, driver or network diagnosis for that interruption.

This is normal-use evidence, not a case-by-case pass of the matrix below. It does not separately establish every PNG/JPEG/WebP source/target format, clipboard boundary/revocation/echo test, cache limit/marking failure, keep-awake timer, notification expiry, UAC cancellation, network path or recovery case. Exact Windows builds, per-display DPI, WebView2 versions and measured latency are not recorded by this report. Earlier 0.3.7 and 0.4.0 checks retain their original scopes.

The following retained artifacts identify the local package supplied for that run. Their bytes are preserved and the hashes are verified against the retained files; the installed copies on the two PCs were not independently hashed during this documentation update.

| Retained local 0.4.1 artifact | SHA-256 |
| --- | --- |
| `EasyMouseConnect-0.4.1-preview-setup.exe` | `8f1b4c9a01722b5e194a21f460e4b97bad10ed3ba269b3fdadea55267c6a5a35` |
| `easy-mouse-connect.exe` | `d78cc271a33651715aee7173a9f1f2d8fac30749ea89184a4cef0da3436ece40` |
| `Easy-Mouse-Connect-source-0.4.1-preview.zip` | `ead388c7e71d0126e833661d130634d1c64c0e6507ecbed0593a19d5053e6237` |

### CI release and download smoke check

GitHub release assets are a separate CI build from the release commit, including updated public documentation. Compare runtime source inputs with the retained tested snapshot before release; matching source is not a claim that the new executable/source ZIP hashes match these local artifacts. The release's own `SHA256SUMS.txt` identifies its files. Preserve the local package and record CI artifact provenance separately.

After the public package is available, download its installer, verify its release checksum and perform a short two-PC install/reconnect/input/clipboard smoke check. Record the downloaded installer hash and actual cases exercised here. This post-publication check is follow-up evidence; it is not already passed by the local normal-use report or CI compilation, and it does not require claiming the entire matrix complete before publication.

## 0.4.1 feature acceptance checklist

Install the same 0.4.1 package on both PCs after fully quitting both old tray processes. Preserve the current identities and pairing. Record the exact local or CI-produced package SHA-256 and mark every case below passed, failed or untested. The normal-use report above does not mark these individual cases passed; each earlier build report also retains its own scope.

Start by repeating crossing/return, typing/modifier release, rapid scrolling, very fast movement, basic **Send file** and text-copy flows against the current tested release reference. New optional features start off; enable only the feature under test. After enabling it, copy a fresh non-sensitive value rather than relying on pre-existing clipboard state.

| Case | Procedure | Pass condition |
| --- | --- | --- |
| Both-PC upgrade | Record both public keys, Quit both apps, install the same 0.4.1 package and reconnect | Existing keys/pairing retained; no Forget device or new invitation required |
| Wire compatibility | Use isolated peers/fixtures for 0.3.7 against 0.4.1 and for 0.4.0 against 0.4.1 | 0.3.7 rejected; 0.4.0 uses the same EMC02 format; no fallback or identity replacement; normal acceptance uses the same 0.4.1 build on both PCs |
| Defaults and independent permissions | Upgrade an older test profile and inspect all new toggles; enable images with text/files off, then test the reverse combinations | New image/keep-awake/notification settings default off; text, images and consent-based files remain independently authorized |
| Snipping Tool and copy actions | Copy small screenshots and supported app images using Snipping Tool Copy, context-menu Copy and Ctrl+C; paste in each direction | The new image pastes once with expected pixels/dimensions; record each supported source/target action separately |
| Explorer image-file copy | Copy one nonsensitive local PNG, JPEG/JPG or static WebP file in Explorer using Ctrl+C/context-menu Copy; paste remotely into an image app and Explorer, both directions | Image transfers through image permission as a generated PNG with expected pixels; no extra prompt, original filename/format not preserved |
| Source file bounds and unsupported sources | Use isolated fixtures at/above 16 MiB source length, a corrupt/nonimage file, animated WebP, a folder/multiple-file selection and UNC/mapped-network locations | Only one supported regular local file within bounds is decoded; unsupported/network selections rejected or ignored without general-file authority or input stalls |
| Original file unchanged | Send a JPEG/WebP through explicit Send file with recipient consent | Original bytes/name retained by file transfer; no claim that image clipboard preserves the source file |
| Cross-kind sequence | Copy/paste text A, screenshot A, text B, local image file B and text A again; repeat both directions with relevant independent toggles | Each new permitted copy arrives once; changing kind does not discard its first value or replay a previous kind |
| Recopy and received marker | Copy image A, then text, then image A again; separately receive an image and allow Windows to request/publish extra clipboard formats | A genuine local recopy arrives once; received marker/format changes alone never echo image metadata or generate an idle transfer loop |
| Application bitmap paste | Test opaque and transparent images with orientation/color fixtures in ordinary image-capable applications | Bitmap/PNG formats are usable; no inversion, color-channel swap or unexpected alpha loss |
| Explorer screenshot paste | Copy a screenshot remotely, cross back and paste into a test Explorer directory; repeat opposite direction | A PNG file is pasted, with expected image content and zone 3 metadata; no separate arbitrary-file transfer is authorized |
| Image bounds | Use controlled fixtures below, equal to and above 16 MiB source file/canonical PNG, 64 MiB decoded RGBA and 16,384 pixels per dimension; test corrupt/unsupported formats in isolation | Valid in-bound content accepted; each exceeded/invalid bound rejected without UI/input stall or excessive allocation |
| Text bounds unchanged | With image sharing off, copy ASCII/Unicode fixtures below, equal to and above 1 MiB UTF-8 | Up to 1,048,576 bytes accepted; larger text rejected; no truncation or extra text-size preference |
| Image echo | Alternate text/images locally and remotely, including sources exposing more than one clipboard format; leave both apps idle after paste | One accepted update; no image/file-format reflection loop, repeated notification loop or continued network transfers at idle |
| Activation and revocation | Enable images with old clipboard data present; copy fresh data, then disable on either side and repeat fast off/on, Pause, disconnect and reconnect while a fixture is pending | Baselines do not send old content; old reads/offers/completions cannot apply under revoked/replacement authority; text policy remains independent |
| Cache bounds and referenced files | Receive at least six distinct fixture images and inspect `clipboard-images`, including a currently referenced image | At most four managed final PNGs of at most 16 MiB each; only generated managed entries evicted; current clipboard references preserved or update rejected safely |
| Cache failure/privacy | Use isolated unwritable/cache-full/zone-marking-failure fixtures; inspect sanitized exports/source package | No unmarked published file or reported successful failed application; no image bytes/cache paths in diagnostics or cache files in exports/source ZIP |
| Clipboard contention and input | Hold clipboard open in a test app, copy a large permitted image while typing/scrolling/crossing, then release contention | Failure/retry remains bounded off hooks; local release and ordinary input remain usable; no stale image replay |
| Keep-awake active lease | Enable Keep the other PC awake on both PCs, use short test display-off/idle-sleep timers and remain physically active on the controller | Receiver display-off/idle sleep is inhibited while permitted activity renews the lease; record behavior for both controlling directions |
| Keep-awake expiry and policy | Stop local activity beyond the 30-second activity window and 15-second receiver lease; separately disable on either PC, Pause, disconnect and Quit | Lease expires or clears and normal display/sleep policy resumes; no retained power request in the ended session |
| Screen saver and security lock | Keep awake enabled while exercising a screen saver and ordinary/managed security lock policy | Screen saver/lock remain effective; no synthetic input, service, elevated control or policy override |
| Optional tray notifications | Enable on one PC, copy locally, receive text/images, connect/disconnect and generate a burst; then turn off | Clipboard messages only for received updates, with a 30-second clipboard cooldown; network cooldown five seconds; no contents/previews/addresses/names/paths; disabled means no optional messages |
| Notification expiry and coalescing | In an isolated notification fixture delay dispatch past five seconds, queue text/image bursts and disable/re-enable before drain | Old events discarded without a delayed burst; at most the current eligible generic event per cooldown; disabled-period events are not replayed |
| Notification suppression | Repeat with Windows notification suppression/Focus Assist enabled | Content still transfers normally; missing Windows presentation is recorded separately from transfer success |
| Scoped firewall action | On an authorized test PC, enter a literal peer IPv4/IPv6 or stored endpoint in Troubleshooting and approve Windows UAC; inspect the resulting rule | Fixed current executable, exact peer host, inbound TCP 24859 and Private/Domain only; app process stays unelevated |
| Firewall cancellation and invalid input | Cancel UAC; try DNS, wildcard, subnet and command-shaped input; repeat an identical allowed request | Cancellation/invalid input changes no rule; no command execution; no duplicate broadening, firewall disable or Public-profile exception |
| Firewall during connection/setup | Test before pairing, with an unavailable input engine and while connected; edit address during status polling | Explicit action only; user-edited address retained; at most one pending helper; error shown as text; no new pairing/input authority |
| Settings layout and project link | Inspect Devices and Troubleshooting at supported window sizes; activate Windows Preview by keyboard and mouse | Balanced equal-height control panels when side by side, responsive stacking and fixed project URL only |

Use synthetic fixtures for malformed image/channel/policy races. Native UAC application is a separate deliberate firewall-policy change; a dry-run or mocked command proves no actual rule was installed. Keep private screenshot pixels and clipboard values out of the result record.

## Record the environment

```text
Revision / installer SHA-256:
Tester / date:
PC A Windows edition/build / architecture:
PC B Windows edition/build / architecture:
Mouse A model / connection / polling rate / vendor software:
Mouse B model / connection / polling rate / vendor software:
Wheel modes / ordinary or free-spin / vertical or horizontal / observed acceleration:
Windows pointer precision / inactive-window scrolling / scroll amount on each PC:
Vendor software known installed/configured, or unknown (do not assume):
Keyboard layouts and models:
Displays A resolutions / scaling / positions:
Displays B resolutions / scaling / positions:
Network path / latency / wired or wireless:
Tailscale version / direct, peer-relay or DERP path:
Windows firewall profiles / scoped rule used:
```

## Pairing, authentication and network paths

| Case | Procedure | Pass condition |
| --- | --- | --- |
| First pair | Show a fresh 12-character code on B; type it with B's endpoint on A | Both show the expected trusted device; no input before trust and enabled sharing |
| Wrong code | Change one code character | Pairing fails without creating trusted state; an admitted attempt is consumed |
| Attempt budget | Make five wrong or abandoned attempts against one code, then try again | Further attempts fail until a new code; no trust is installed |
| Expired code | Wait beyond five minutes, then attempt pairing | Code rejected; fresh code succeeds |
| Replaced code | Generate a second code and retry the first, including an in-flight attempt | Earlier code cannot create a new pairing |
| Transcript/channel binding | Run controlled wrong-role/corrupt/transcript/preamble substitutions using isolated test identities | Auth fails; no fallback from PAKE to legacy or plaintext |
| Unknown client | Connect using a clean profile without code | No input, clipboard, file or trusted reconnect authority |
| Identity mismatch | Use a fresh B profile with A still trusting the original key | Connection rejected; no silent key replacement |
| Restart | Quit from the tray, then relaunch both apps | Identities remain stable and trusted reconnect works |
| Existing-pair upgrade | Record public keys from an established test pairing, Quit both, install the new build, relaunch | Same keys/settings and authenticated reconnect, without Forget device; test fresh pairing separately |
| Revoke | Revoke the other PC while connected and reconnect | Existing authority ends and reconnect requires new pairing |
| Same LAN | Use normal local interface addresses | Pair and reconnect without subnet discovery |
| Routed VLAN/subnet | Use different private subnets with an authorized route | Same behavior as LAN once TCP route/firewall permit it |
| Separate locations | Use tailnet IPs with LAN routes absent | Pair and reconnect across Tailscale |
| IPv6 | Use reachable literal IPv6, with brackets for an explicit port | Listener accepts and reconnect behaves correctly |
| No route/firewall deny | Block TCP to the peer and attempt connection | Clear failure; local control remains usable |
| Channel authentication | Attempt a bulk/control socket from an untrusted profile | Neither channel grants authority without peer authentication |

## Input and recovery

| Case | Procedure | Pass condition |
| --- | --- | --- |
| Edge crossing | Move across each configured shared edge in both directions | Predictable handoff and return; no unconfigured-edge switch |
| Mouse standard | Left/right/middle, back/forward, vertical/horizontal scrolling | Events delivered once with correct direction and button state |
| Logitech | Repeat with wired/Unifying/Bolt/Bluetooth devices available | Standard events work; record model/transport and unsupported custom gestures |
| High rate | Move a 1,000 Hz+ mouse continuously while transferring a file | UI responsive, no loss of local release, no unbounded lag buildup |
| Keyboard | Type ASCII/Unicode via real layouts; modifiers, arrows, function keys, numpad | No stuck keys; layout behavior recorded and reproducible |
| Idle then type | Cross to the peer, stop moving for at least 2 seconds, type into a non-sensitive editor, repeat both directions | Keyboard stays with the active receiver; no false raw-capture reset just because mouse motion stopped |
| Bluetooth host switching | Switch mouse/keyboard hosts, including delayed key-up after reconnect; repeat with both hardware switches on one controller | Fresh keydown can reclaim, cleanup keyup alone does not end receiving; device removal releases safely |
| Local shortcuts | With routing Local control, exercise ordinary Ctrl/Alt shortcuts before/after pause; with Sending, check delivery on peer | Ordinary shortcuts pass through locally or route during Sending; Ctrl+Alt+Escape remains the reserved local release gesture, including while paused |
| Protected holds after reclaim | Hold Ctrl/Shift or a button whose down was suppressed; reclaim manually, then approach edge before release | Guard retains the physical hold; no asynchronous sample falsely rearms handoff; observed up/device reset clears it |
| Resize shapes and hotspots | Test Explorer edges/corners, text insertion and links, with/without native cursor | Correct resize/I-beam/hand artwork follows actual input hotspot; test shell surfaces separately |
| Slow native processing | In a synthetic sink test delay native completion while exchanging heartbeat/opposite input | Independent network progress; stale local work releases instead of replaying; numeric latency/age counters agree |
| Held keys/buttons | Hold Shift and a mouse button, disconnect/release | Remote state releases; subsequent local use is normal |
| Emergency release | During remote control, press Ctrl+Alt+Escape on controller | Local control returns without the network or UI responding |
| Network loss | Unplug network or stop Tailscale during active use | Control recovers locally; both sides clear held input |
| Reconnection | Restore network after a failure; repeat sleep/wake and app restart | Predictable trusted reconnect, no duplicate sessions or input loops |
| Multiple monitors | Test displays left/above primary including negative coordinates | Pointer remains within valid desktop bounds and configured edge behavior is clear |
| Monitor gaps and rows | Use staggered displays whose bounding-box center or row spans a gap; cross internal seams and exposed peer edges | No center warp, pointer placed on an actual monitor; only an exposed configured edge hands off |
| Mixed DPI | Test 100%, 150% and 200% scaling across displays | No unexpected jumps, traps or unusable boundary regions |
| Bluetooth with multiple monitors | Test Bluetooth input with stacked or offset monitors in both directions, including click-drag and diagonal movement | Visible receiver pointer and continuous accumulated motion; record physical evidence rather than inferring a pass from RTT |
| Receiver without mouse | Test with no physical receiver mouse, and with Windows reporting a hidden cursor | Receiving-only fallback visible when needed; correct hotspot; any absent-mouse overlap stays aligned with the same native shape |
| Overlay reclaim | Move/click locally, disconnect, panic, change displays/input desktop while receiving | Fallback disappears, clicks/focus remain local; global mouse/registry settings remain unchanged |
| Raw motion ordering | Use short final movements, drag after movement, wheel and keyboard boundaries | Final tail arrives; movement precedes the related button/wheel/key and does not jump after it |
| Raw delivery loss | In an isolated native test, suppress raw progress after a physical callback, then test low-rate/sparse input too | Conservative routing reset, held input released; fresh relative motion required before reentry; measure actual recovery instead of assuming a hard OS deadline |
| Unsupported absolute device | Attempt control from an absolute raw-input device in a test profile | Control releases visibly; absolute coordinates are not interpreted as relative deltas |
| Display change | Disconnect/reconnect a monitor and change resolution | App recovers or fails visibly with local control retained |
| Lock/UAC | Lock B and trigger a UAC prompt from B's local controls | No bypass of secure desktop or elevated input boundary |
| App crash | Terminate sender while keys/buttons are held | Receiver clears held state after session-loss detection |
| Simultaneous control | Attempt remote activation on both PCs at once | No feedback loop, stuck capture or conflicting ownership |
| Stale queued input | Generate continuous input, pause/disconnect and establish a new session | Events queued under the old native epoch do not appear in the new session |

## Registration, handoff and responsiveness

Run the same build on both PCs, using the published 0.3.7 build as the current tested release reference and retaining 0.3.5 as the original native-input baseline. Keep the current pairing and quit both old tray processes before installing. Isolated native contracts do not replace the following physical tests. Fault injection belongs in an isolated native test account/build; do not modify live hook state, registration or renderer resources on a paired session just to collect a pass.

| Case | Procedure | Pass condition |
| --- | --- | --- |
| Registration baseline | On each PC, unpause and move its physically connected mouse for at least 15 seconds, including settings/tray focus changes; inspect Diagnostics | Raw input remains registered; query/mismatch/failed-repair counts do not continually rise; no once-per-second routing reset |
| Current raw edge entry | Cross midway along each configured side, slowly and diagonally; stop at the edge for over a second, then continue outward; repeat both directions | Four outward X hardware units at the same eligible span request entry without a hook/50 ms/dominance dependency; zero/tangent packets and idle do not erase valid pressure; inward/invalid/held/policy boundaries clear it; no timed dwell/batch |
| Stack/portrait seams and corners | Move up/down through stacked and offset portrait/landscape seams; slide along corners and offset shoulders | Local monitor transitions stay local; 16-pixel corner/shoulder margin blocks accidental cross-PC entry/return; aligned continuous side edges remain usable |
| Entry settlement and idle return | Cross, stop moving, then move inward and deliberately return; also move a window that warps the pointer in an isolated fixture | Old direction or pre-entry position cannot cause timer-only return; settled fresh outward intent is required; an inward/outward nudge can restore deliberate return after skipped samples |
| Source pointer | Observe the sending display during remote input | Record the current visible parked pointer as a known limitation; do not mark source hiding passed |
| Native deadline and failure stages | In isolated synthetic tests, hold command work past the original receipt deadline and cancel/revoke between enqueue and commit | No renewed enqueue budget, known stale work cannot start insertion, entry rolls back, one stale stage and total are counted, and accepted late holds are cleaned before replacement input |
| Both crossing directions | With local input available, test A controlling B, then B controlling A over the existing LAN connection | Captured/injected counts rise on the correct peers, no re-pairing required, pointer remains controllable after a movement pause |
| Bulk reconnect overlap | From the original dialing side, request reconnect while the periodic retry is due; repeat clipboard/file feature off/on using nonsensitive fixtures | One bulk installation per generation; an old closing slot can retire without an immediate duplicate failure; active transfers are never evicted or resumed through a replacement sender |
| Known local pause | Pause one PC and let the dialing peer try reconnect | Paused receiver records typed local policy admission refusal; remote EOF is not presented as evidence of the peer's exact policy |
| Receiver jitter | While typing remotely, introduce tiny receiver-mouse jitter, then deliberately move at least the reclaim threshold; separately click or scroll locally | Tiny movement retains ownership; deliberate movement reclaims; button/wheel/key actions and emergency release stay immediate |
| Unrelated removal | During source control, remove a known unused secondary mouse/keyboard; separately remove a contributing or held device | Unused known device does not end routing; contributing/held/unknown devices fail open; another device's held modifier/button remains guarded |
| Two devices holding one control | Hold the same modifier/button on two devices, release or remove one, then approach an edge | The remaining physical hold still blocks crossing; final observed release clears it without a fabricated release |
| Hook delivery recovery | In a native fault-injection build, remove one hook while raw input continues; also test sparse activity after long idle | Suspected sustained delivery loss releases locally before replacement; partial install never captures; idle gaps alone cause no false recovery; last-installed flags are not treated as liveness proof |
| Raw registration ownership | In an isolated build, replace the raw target/flags after engine setup | Audit releases active routing, repairs/verifies ownership and requires fresh readiness; normal Tauri focus/tray activity does not steal registration |
| Renderer recovery | Inject a one-shot window/paint failure and a terminated worker; repeat while idle/native-visible and while fallback is required | Current receiving lifecycle ends safely; bounded idle retry recovers resources without app restart; later Enter succeeds only when required fallback is healthy; no stale window reappears |
| Optional fallback | With a successfully sampled visible native cursor and present mouse, make optional renderer unavailable | Normal receiving is not rejected solely because the optional renderer failed; unavailable sampling cannot grant bypass |
| Lock-return guidance | Hold a modifier before changing input desktop, return locally and attempt crossing | Retained guards do not silently clear; banner/tray guidance appears in local mode; explicit local release restores crossing |
| Busy and background | Repeat movement/typing/resize with settings hidden in tray and with CPU load; record processing/recovery counters | No unbounded backlog; safety release works; observed input latency is measured separately from network RTT |
| Address refresh | Change the local network while status polling is open and input is active | Input remains responsive during route lookup; display-only address hints update on the 30-second refresh |
| Intentional hidden cursor | Use a video player or hide-pointer-while-typing setting while receiving | Record the existing conservative fallback behavior as a limitation; no global cursor change or claimed intent detection |

Current `Leave` has no exit-height metadata, and third-party injected macros remain filtered. These are documented limitations, not passing cases. See [architecture](architecture.md) for the current routing boundaries.

## Scrolling and asynchronous input

Record the mouse model, wheel mode, Windows pointer/scroll preferences and any known vendor software before testing. Exercise each available wheel mode, horizontal input, action barrier and emergency-release case separately. Keep existing preferences during the baseline run and test both controlling directions. Record synthetic/native contract results separately using [the development checks](development.md).

| Case | Procedure | Pass condition |
| --- | --- | --- |
| Slow/normal/fast wheel | Scroll non-sensitive long documents by slow notches, ordinary speed and repeated rapid strokes; repeat both directions | Scroll direction/magnitude stays usable; no unexpected local return, native deadline disconnect or stuck button/modifier |
| Accelerating/free-spin wheel | Use the mouse's available wheel modes and accelerate/decelerate without moving across an edge; test vertical and horizontal input where supported | No packet normalization to ±120, input remains ordered, local release stays available; record actual mode rather than assuming Logi Options+ settings |
| Focused and inactive targets | Scroll over a focused ordinary window, then an unfocused one with the current inactive-window-scroll preference | Behavior follows the recorded receiver setting; the utility does not steal focus or change that preference to obtain scrolling |
| Small signed deltas | In a synthetic sink fixture feed ±1, ±15, ±60, ±120 and larger deltas, both wheel axes | Exact signed values/axis survive each stage; no truncation to whole notches or merging through a key/button barrier |
| Move/wheel/action barriers | Move, scroll, click/drag, press/release modifiers and keys, then scroll again in controlled sequence | FIFO action order and final releases remain correct, with no stale motion replay after a click/key/wheel |
| Pending Enter reclaim | Hold insertion in a fake executor, deliver genuine local reclaim/reset before completion, including raw button/wheel fallback without its low-level callback, then release the fake operation | Hooks/raw/maintenance keep progressing; pending Enter never restores receiving after its local ownership token was revoked |
| Late accepted down | In a synthetic executor accept a down as cancellation/expiry occurs, then request replacement ownership | Accepted old-owner hold is released before new-owner input; abandoned completion does not lose the cleanup ledger or report successful stale work |
| Emergency while pending | During held synthetic insertion and normal fast scrolling, exercise Ctrl+Alt+Escape and local button/key reclaim | Local suppression ends without waiting on the executor; OS cleanup follows when any in-progress call returns; emergency intent is never filtered as jitter |
| Optional return warp | Queue a return warp in a fixture, revoke its owner, then establish replacement input | Stale optional warp is skipped, never moving a replacement owner's pointer |
| Shell and background | Repeat scroll/drag/key sequences with settings hidden and over Explorer, browser, Start/Search and available full-screen surfaces | Routing safety and ordering hold; record shell cursor visibility separately rather than declaring the ordinary overlay limitation fixed |
| New measurements | Compare each run's wheel counts, insertion-backend/wheel-call peaks, asynchronous native interval and four stale stages | Counts/durations are numeric only; no wheel delta/key/coordinate history; no inference that low network RTT rules out native stalls |

From a Windows development terminal, `scripts/test-native-input.ps1` selects six named isolated registration, pump/order, deadline, source-admission and executor-cancellation contracts serially. The executor contract is `windows::tests::native_injection_executor_pumps_owner_messages_and_releases_cancelled_holds`. It uses a fake insertion backend; it must not start the paired app, install hooks, inject input or access identities. Its completion is not a hardware acceptance pass. The two-contract registration helper remains available separately.

## Diagnostic collection sequence

After reproducing, export the ZIP while the first app is running, Quit it, switch Bluetooth hardware if needed, export from the second app and Quit it. Record that sequence. The first app's Quit can produce an expected terminal disconnect on the second; a post-export event is absent from the first ZIP. Treat both exports as separate snapshots, interpret current counters separately from retained history and do not count collection transitions as proof of the original failure. Confirm numeric edge counters and original-version/current-logger metadata are present in current exports. No input content or coordinates should be included.

## Clipboard and file transfer cases

Basic app-based **Send file** and controlled-to-controlling-PC text clipboard have the limited [0.3.7 results above](#confirmed-037-release-checks); these results do not independently pass 0.4.1 cases. The separate limited 0.4.0 screenshot-to-Explorer result does not pass its follow-up changes, and the 0.4.1 normal-use report is not a detailed transfer-policy result. The default destination is `%USERPROFILE%\Downloads\EasyMouseConnect\<transfer-id>\<filename>`; each transfer uses a generated child folder. File-size boundaries, integrity comparisons, consent/rejection, policy changes and interrupted/concurrent transfers still need their own results. Reverse clipboard and the remaining bounds/policy/fault cases need separate results.

| Case | Procedure | Pass condition |
| --- | --- | --- |
| Defaults | Fresh profiles, copy local text/images | Text/image clipboard and file sharing remain off until independently enabled |
| Clipboard both ways | Enable on both PCs; copy plain/Unicode text each way | Exactly one application per update; no endless echo |
| Clipboard policy | Disable on either side during a session | No further forbidden send or receive applications |
| Clipboard bounds | Try just under, equal to and above 1 MiB UTF-8 text | Behavior matches byte limit; app responsive on rejection |
| Clipboard contention | Hold clipboard open in another app | Retry/failure does not freeze hooks or UI |
| Drop into app | Drop a small file onto A's transfer area | B displays offer; no content accepted without explicit approval |
| Decline | Decline an offer on B | No completed received file |
| Exact file limit | Offer 99,999,999, 100,000,000 and 100,000,001-byte files | First allowed; exact limit and above rejected |
| Integrity | Transfer binary content and compare Get-FileHash on both PCs | SHA-256 values match |
| Safe destination | Offer a name with traversal, ADS/device names or path separators | Unsafe metadata rejected; no write outside receive directory |
| Duplicate name | Receive identical filenames in two transfers | Existing content never overwritten |
| Interrupted transfer | Disconnect during a file | Incomplete content is not reported as a successful file |
| Session replacement | Interrupt an approved transfer, reconnect and offer another file | Old outgoing bytes do not migrate to the new sender; stale incoming state does not block or join the new transfer |
| Revoke near commit | Pause/forget the peer while a large accepted file completes | Commit respects current session/policy; rejected work leaves no completed unmarked file |
| Receiver confirmation | Complete a transfer, then test a lost completion acknowledgment | Success appears only after `Received`; lost acknowledgment is reported as failure even if receiver already saved |
| Internet-zone marking | Inspect Zone.Identifier on a received test file; repeat with a filesystem that cannot store it | Mark present before final success; marking failure does not leave an unmarked final file |
| Concurrent input | Transfer near-limit file while moving and typing remotely | Input remains usable and release continues to work |
| No auto-open | Receive executable/HTML/document test files | Saved as content; nothing launches automatically |
| Settings persistence | Toggle sharing and restart app | Saved preferences match displayed state and policy |

## Packaging and storage

| Case | Procedure | Pass condition |
| --- | --- | --- |
| Fresh install | Install unsigned development NSIS on clean test account | Correct app name/path, launches with WebView2 available |
| Desktop shortcut | Exercise the installer's shortcut choice; launch from the chosen shortcut | Correct executable/icon; choice respected |
| Tray Close/Open | Close X, then single-/double-left-click tray and use Open | Engine remains active while hidden; same settings window reappears |
| Single instance | Launch the shortcut/executable repeatedly while hidden and visible | Existing instance focuses; no second listener or input engine |
| Tray Pause/Stop/Quit | Exercise Pause/Resume, Stop sharing and Quit while remote input is held | Settings/tooltip agree; pause/stop release input; Quit exits the process and stops sharing |
| Reinstall/update | Install newer build over existing test install | Identity preserved or migration clearly handled; no silent trust reset |
| User isolation | Try copied DPAPI data from a different Windows account | Identity cannot simply decrypt as the other user |
| Privacy | Inspect logs, crash output and git status | No invitation/private key/clipboard/file content in logs or repository |
| Log bounds | Generate typed test events through multiple rotations | Only emc.log/emc.1.log/emc.2.log; each ≤2,097,152 bytes and total ≤6,291,456 bytes |
| Log metadata | Exercise pairing, input failure, transfer and reconnect failures with synthetic sensitive fixtures | Only timestamp/version, typed events/categories and aggregate counts; no raw input/content, user file names/paths, IPs, device names or credentials |
| Explicit export | Inspect before clicking, then click Export diagnostic ZIP | No automatic ZIP/upload; allowlisted existing records plus boolean/numeric summary only; no identity/settings/directory fields |
| Open exported ZIP | After export use Open export folder; copy that ZIP | Known export directory only, no frontend-selected path; ZIP includes numeric summary unlike standalone .log |
| Latest export | Export twice; copy the first elsewhere before the second if comparison is needed | One app-managed diagnostic-exports/emc-diagnostics.zip, replaced on later export |
| Startup failure diagnostics | Use an isolated profile with a controlled identity/native-startup failure | Actual error shown; local diagnostic metadata/export/known-folder open still available; no false connection/input state |
| Uninstall | Uninstall after receiving files | Behavior documented; user data is not unexpectedly erased |
| Firewall script | Apply helper with -WhatIf and an exact peer; review rule | Only requested executable/IP/TCP port/Domain+Private profiles appear; no change occurs in dry-run |
| Firewall UI | Use Troubleshooting with a literal peer and deliberate Windows approval; inspect rule, then exercise cancellation | Current executable, fixed TCP 24859, exact peer and Domain/Private only; app stays unelevated; cancel changes nothing |

Report failed cases with revision, minimal steps, observed behavior and redacted diagnostics. Do not mark a row passed solely because a unit test covers a related function.

