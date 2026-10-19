# Architecture

Easy Mouse Connect runs native input, networking and application state in Rust. Tauri 2 hosts the local settings interface in WebView2. The frontend uses HTML, CSS and vanilla JavaScript without a framework or bundler; WebView2 and the Rust dependency graph remain part of the application.

```text
Local settings WebView
    | narrow Tauri commands / state snapshots
Rust desktop application
    |-- emc-core: transport, settings, trust, sessions, clipboard/file policy
    |-- emc-protocol: messages, bounds, metadata validation
    `-- emc-platform: Windows capture, ordered insertion, clipboard
            |
    TCP input/control channel + separate TCP bulk channel
            |
    Authenticated paired PC
```

The WebView configures sharing and displays state. Mouse movement never crosses JavaScript IPC. Hook callbacks return promptly without waiting for networking, filesystem access, rendering or insertion. File bytes use a separate authenticated connection so a transfer cannot fill the input queue. TCP retains ordered delivery and retransmission delays.

## Components

| Path | Responsibility |
| --- | --- |
| `crates/emc-protocol` | Typed wire messages, framing limits and metadata validation |
| `crates/emc-core` | PAKE/Noise/TCP transport, settings, protected identity/trust persistence, sessions, sharing policy, transfers and diagnostics |
| `crates/emc-platform` | Windows capture, injection, cursor fallback, clipboard and portable policy helpers |
| `apps/desktop` | Tauri initialization, tray/single-instance lifecycle and narrow Rust commands for the local interface |
| `ui` | Settings, pairing, peer layout, transfer consent and diagnostics |

Display-only address hints are discovered on blocking workers and refreshed every 30 seconds, with missed ticks skipped. Status reads cached hints before taking the session mutex. Route discovery never runs on the input path, and an address hint establishes neither reachability nor identity.

## Identity and transport

Pairing starts with a random 12-character base32 code displayed as `XXXX-XXXX-XXXX`: 60 random bits, a five-minute lifetime and at most five admitted attempts. Failed or abandoned exchanges consume the budget before expensive cryptographic work. The code can be typed on the other PC without an existing clipboard connection.

RustCrypto SPAKE2 0.4 uses the code as its password on channel 3. Transcript-bound HKDF-SHA256 derives a fresh 256-bit PSK, then `Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s` confirms it and exchanges device identities. The transcript is also bound to the Noise prologue. This is not a direct code-to-PSK hash; neither the dependency nor the integration has an independent audit claim.

Subsequent control and bulk connections each authenticate pinned identities with `Noise_IK_25519_ChaChaPoly_BLAKE2s`. Addresses locate peers; they do not identify them. A changed key needs a new pairing decision. Records are bounded before decoding, and there is no plaintext fallback.

Versions 0.2.0 through 0.4.1 preserve version-1 DPAPI identity/settings storage and established IK peers. Upgrading does not require deleting identities or forgetting a trusted peer. Versions 0.4.0/0.4.1 use EMC02; 0.3.7 uses EMC01 and does not connect to them. New pairing requires compatible short-code receivers. Rust retains an explicit outbound legacy `emc1` import branch, but generates no legacy tokens, rejects incoming channel-0 pairing and never falls back after short-code failure.

Connections use TCP port 24859 with `TCP_NODELAY` over an existing route. There is no application relay, rendezvous server, subnet scan, UPnP or cloud account. The code-entering PC retains the dialing role, including manual reconnect and automatic attempts every three seconds; the other PC listens. See [protocol](protocol.md) for framing and authentication and [networking](networking.md) for route/firewall requirements.

## Session ownership and queues

Control sessions and bulk channels have separate generations. Installation and operation commits recheck identity, expected generation, policy and shutdown state. Incoming transfers belong to their original bulk generation. Outgoing transfers retain their original sender and generations; a replacement channel cannot inherit remaining bytes.

Captured events carry a native connection epoch. Pause, disconnect or replacement invalidates old queued events. The authenticated reader, encrypted writer and ordered native dispatcher run independently, so native completion cannot stop heartbeat receipt or opposite-direction writes.

Outgoing control, authenticated dispatch and native-input queues each hold at most 128 entries. Only adjacent relative moves merge, preserving checked sums and the oldest timestamp. Enter, Leave, ReleaseAll, keys, buttons, wheels and other control messages are barriers. A 200 ms local capture/receipt-to-processing/send budget rejects stale work and releases rather than replaying backlog. This is a local processing policy, not a network-latency guarantee. EOF/cancellation revokes native authority before old workers can feed a replacement session.

Liveness is checked once per second, with release after observing four seconds without newly received authenticated traffic. At most one ping is outstanding, so a delayed matching pong remains valid. Liveness refreshes at authenticated receipt, never buffered dequeue. These policy timers cannot bound recovery under every scheduler stall or blocked OS call.

Bulk dialing serializes through committed installation; its guard follows a spawned installer if the caller is cancelled. Commit checks the pre-handshake bulk generation as well as control generation, pin and policy. An authenticated incoming connection may wait up to two seconds for an old slot to retire naturally, without holding the transfer mutex or evicting a live channel. It cannot follow a replacement session.

## Windows capture and geometry

A hidden native window receives background raw mouse/keyboard input. Physical relative `WM_INPUT` supplies motion, buttons, wheels and source-edge admission. Low-level hooks handle suppression, keyboard scan codes and emergency release. Capture requires observed nonzero relative motion and healthy registration. Unsupported absolute input, stalled delivery, queue failure or desktop/display changes release routing locally.

Each raw movement packet flushes immediately without a deliberate source wait or timer tail. Bounded downstream queues may combine adjacent motion while preserving action barriers. The source never clips or recenters per movement. Relative receiver insertion lets Windows accumulate deltas and apply receiver pointer settings. Signed wheel deltas preserve magnitude and axis, including values smaller than 120.

Geometry comes from Windows' occupied monitor rectangles in physical pixels, including negative origins, portrait displays, offset rows and gaps. Entry/return positions project onto an actual monitor instead of treating the virtual bounding box as occupied. The hook thread uses per-monitor-v2 DPI awareness. Runtime geometry is not tied to a particular resolution or arrangement.

Source entry samples the current cursor once from an eligible physical relative raw packet. Four hardware units of outward X pressure on the same continuous exposed edge span request entry. Diagonal movement contributes outward X; zero/tangent packets and idle preserve partial pressure only at the same valid site. Inward motion, invalid geometry, side/span changes, held controls or lifecycle/policy boundaries clear pressure. Corner/offset-shoulder margins are 16 physical pixels; aligned stacked side edges stay continuous. Original epoch, side, readiness and policy remain checked through Enter and commit. Admission needs no hook ordering, fresh hook timestamp, horizontal dominance or timed dwell.

Receiver return requires entry/inward observation followed by fresh outward intent. Continued crossing movement therefore points inward on the receiver. The sending pointer remains at its local edge while forwarding; source-pointer hiding is not implemented.

## Ordered insertion and cancellation

All `SendInput` operations use one ordered executor with an eight-entry queue outside the hook/raw thread, including input, entry/return warps and held-key/button cleanup. At most one incoming apply is prepared at a time. The hook thread keeps servicing messages while insertion runs, then performs a guarded ownership commit. Completion acknowledges actual insertion and commit; enqueue alone is not success.

A separate native ownership token complements the connection epoch. Local reclaim/reset can revoke receiving while TCP survives; old pending Enter cannot restore that owner. Original receipt deadlines survive queueing and are checked before execution and final commit. Core also checks elapsed time after a ready asynchronous result, since timer polling can be delayed.

The executor owns the ledger of actually accepted key/button downs. A late accepted down is cleaned up before replacement-owner input. Idle maintenance retires revoked holds; legitimate held keys do not expire with their insertion deadline. Release requests coalesce and obsolete optional return warps are skipped. Old-owner cleanup cannot release a different live owner's holds. Hooks and maintenance never wait for the executor, and a blocked executor is never replaced by a parallel one.

An OS insertion cannot be interrupted or its accepted events withdrawn. Local routing can revoke during a blocked call, with late-hold cleanup after it returns. Aggregate measurements distinguish queue residence, asynchronous execution and insertion-backend intervals. Backend intervals include thin input/error wrappers and warp-coordinate work; they do not measure an isolated syscall or target-app rendering latency.

## Native health and reclaim

Raw and low-level callbacks have no one-to-one timestamp contract. Source delivery tolerates accepted raw-first progress within 16 ms. Otherwise the first suppressed observation starts a 250 ms missing-progress limit that later hooks cannot extend. Accepted physical relative progress clears suspicion. Routing/repair needs fresh relative motion. Receiver pass-through input, injected echoes, keyboard packets and unsupported absolute motion cannot satisfy this guard. Recent coalesced activity cannot prove delivery of every individual movement.

Native commands use coalesced wakes and slices bounded to 32 commands or 2 ms of completed work. Raw/device messages run between slices, already-retrieved messages retain order, and maintenance runs during sustained traffic. A slice bound cannot interrupt one blocked OS call.

Sustained raw activity without low-level delivery can suspect hook failure; idle gaps are not failure evidence. Routing releases before replacement, and partial installation cannot capture. Tao device events are disabled before backend registration. Audits require mouse and keyboard entries with the backend target, `RIDEV_INPUTSINK` and compatible flags; readback may omit `RIDEV_DEVNOTIFY`. An unavailable query suspends admission without blind re-registration. Confirmed mismatches permit repair, with failed attempts backing off from one to 30 seconds and fresh readiness required afterward.

Physical and injected holds are separate. Outside callbacks, asynchronous samples prune only stale local pass-through holds; suppressed-origin holds need an observed release or device reset. Up to 64 anonymous process-local handles distinguish contributing/held devices from known unused devices. Relevant, unknown or ambiguous removal releases routing, and only attributable holds clear so another device's hold stays protected.

Receiver movement reclaims after eight relative hardware units of net displacement within 200 ms. Physical buttons/wheels, keydown and **Ctrl+Alt+Escape** remain immediate. Raw button/wheel observations cancel pending Enter if a hook was missed; cleanup keyup alone does not reclaim. Protected holds stay visible through local-release guidance. `physical_mouse_reclaims` includes movement/button/wheel in 0.3.5; its 0.3.4 definition counted movement only.

## Receiving cursor fallback

A dedicated native UI thread renders a transparent, click-through, no-activate layered window when Windows reports an absent/hidden/suppressed cursor or no physical mouse. Cached owned artwork keeps native dimensions/hotspots and valid resize, text and hand shapes. Null/invalid/blank artwork falls back to an arrow. The input thread updates an atomic lifecycle without painting.

Leave, reclaim, emergency release, disconnect and desktop/display changes hide the fallback and invalidate old frames. Injected movement posts a coalesced renderer wake; its timer is only a fallback, with no fixed rendering-rate guarantee. Resources retry with bounded backoff. A dead worker restarts only after finishing. Required fallback checks sampling, resources and recent actual renderer progress; a visible native pointer with a present mouse can bypass an optional failed renderer. Recovery never reactivates ended ownership.

The overlay remains an ordinary application window that shell, Start/Search, taskbar or full-screen surfaces may cover. Intentional application cursor hiding cannot always be distinguished from other hidden states. No MouseKeys, registry or global cursor settings change. Windows integrity/UIPI restrictions remain: the app neither elevates nor installs a service, and UAC/sign-in desktops are outside its scope.

## Clipboard and files

Text clipboard sharing defaults off and is limited to 1 MiB of UTF-8. Both peers enforce policy. Activation establishes a baseline rather than sending existing clipboard content immediately. The baseline and asynchronous read/send retain their original control generation, bulk generation and clipboard policy revision; an off/on transition invalidates them even if polling never observes the inactive interval. Transport encryption does not prevent an enabled trusted peer from receiving passwords/tokens present in shared content.

Image clipboard sharing has an independent off-by-default permission on both peers. PNG/DIB/DIBV5 pixels and one explicitly copied local PNG/JPEG/static WebP file are decoded into bounded RGBA pixels, stripped of source metadata and sent as canonical PNG on the authenticated bulk channel. Source file bytes are also limited to 16 MiB; neither its path nor its original name is sent. Encoded images are at most 16 MiB, decoded RGBA at most 64 MiB, and either dimension at most 16,384 pixels. Length, SHA-256, current session/channel generations and image permission are checked before publication; success requires the receiver's acknowledgment. A content revision rejects reads sampled before a remote clipboard commit, independently of activation and connection revisions, so a pending old read cannot overwrite or echo newer received content. Content-only changes preserve established activation baselines: receiving an image must not swallow the next text copy, or vice versa. Publication attaches a private received-image digest marker; metadata-only sequence changes retain that provenance while pixels still match. A normal new Copy clears the marker, and changed pixels remain eligible. A third-party clipboard restore that strips provenance is indistinguishable from a fresh copy, so this is not a general clipboard-manager intent detector.

Windows publishes PNG and bitmap formats for app paste, plus a copy-only file-drop format referring to a generated PNG under the local profile's `clipboard-images` cache for Explorer. Peer paths are never opened. Explorer import accepts exactly one supported image on a local drive; it rejects folders, multi-file lists, UNC/device paths, alternate streams and mapped network sources before content transfer. The opened file must remain regular, bounded and on a local volume. This permission does not enable general clipboard file transfer. Cache files have generated digest names, receive internet-zone marking before final rename and remain private local clipboard data. Cache retention is bounded to four images of at most 16 MiB each; cleanup excludes the currently referenced managed clipboard file and unrelated files. Conversion, allocation, directory inspection, hash checks, eviction and metadata flushes run on a blocking worker outside the session mutex. The short final commit rechecks authority and only promotes the marked file and publishes preallocated clipboard formats. Cancelled preparation removes its temporary file.

Files are offered through the app with explicit recipient approval. Each file must be **smaller than 100,000,000 bytes**; exactly that size is rejected. Reception uses `Downloads/EasyMouseConnect/<transfer-id>/<safe-name>`, a generated child directory, no overwrites and no automatic opening. Direct cross-PC Explorer drag and drop is not included.

Chunks are at most 48 KiB. Completion requires exact length and matching SHA-256. Windows internet-zone metadata is applied to the temporary file before rename under current generation/policy. Failed marking or revocation cannot leave a completed unmarked file. Sender success waits up to 15 seconds for authenticated `Received`; this is the peer's statement, not proof of a compromised endpoint's honesty. Approval waits up to 65 seconds; incoming lifetime is an absolute 120 seconds from offer creation, including approval. Disconnect/replacement cancels rather than resumes transfers.

## Storage, diagnostics and lifecycle

Windows identity/configuration lives under `%LOCALAPPDATA%\EasyMouseConnect` with current-user DPAPI. Same-account code and a compromised OS remain outside that protection. Restrictive-permission Unix storage supports core development, not Linux desktop input.

The protected version-1 identity file keeps its original nested settings keys and pinned identities. New preferences are saved in an optional top-level extension; a 0.3.7 reader ignores that extension and can still load its identity, peer and dialing role. Loading a missing extension defaults new permissions off. Saving with an older version can discard these new preferences; it does not require deleting identity or trust.

A bounded nonblocking queue feeds typed diagnostics to a separate writer. `logs/emc.log`, `emc.1.log` and `emc.2.log` are capped at 2 MiB each, for 6 MiB total. Raw input, key identities, content, credentials, file names/paths, IPs and device names are excluded. Timing, typed categories and numeric/boolean aggregates remain.

Explicit export reparses allowlisted records and a numeric/boolean summary into one replaceable `diagnostic-exports/emc-diagnostics.zip`. It excludes identities, configuration and directory metadata and never uploads automatically. Folder-opening commands take no frontend path. Startup-failure export works without another engine/writer and reports `engine_available=false`; unavailable input controls and the startup error remain visible.

Close hides settings while sharing continues. The tray provides Open, Pause/Resume, Stop sharing and Quit. Single-instance handling focuses the existing window on another launch. Explicit Quit releases input and stops the engine. Upgrade with both peers fully quit and retain identity/settings data.

See [development](development.md) for checks, [manual testing](manual-testing.md) for real Windows acceptance, and the [threat model](threat-model.md) for remaining trust boundaries. Portable tests and browser mocks do not establish hardware compatibility, end-to-end latency or secure-desktop control. A Linux desktop backend requires separate compositor-approved capture/emulation and permission work.

Settings-save failure preserves requested Stop/Pause and file/clipboard reductions for the current run, without applying unsaved increases or unrelated layout changes. Channel retirement happens under the policy lock; native disable/disconnect runs before returning an explicit persistence/restart error.

## Optional desktop preferences and support actions

Keep-awake is opt-in on both PCs. A slow task samples existing local activity evidence outside hooks and sends a small authenticated `PeerActivity` control message at most every three seconds while recent local activity remains within 30 seconds. The optional enqueue leaves input queue headroom and can be omitted under load; an expired activity item is discarded before starting a write. A write already in progress retains the normal ordered transport failure rules. Receiving/injected activity cannot reflect a keep-awake loop.

The recipient scopes each request to its current control generation and preference activation. A dedicated power thread requests display/system availability for at most 15 seconds from the original authenticated receipt. Permission changes, pause, disconnect, replacement and shutdown revoke the scope; maintenance checks at 250 ms intervals, with OS request updates off the input thread. No input is synthesized, no saved setting changes, and Windows screen savers, security locking and manual Sleep remain in effect.

Optional notifications consume typed, content-free clipboard and connection snapshots in the desktop's slow status task. Clipboard notices are receive-only; send acknowledgments cannot replace a pending received notice. Activation establishes a baseline, including fast off/on transitions. Clipboard notices have a 30-second cooldown, connection notices a separate five-second cooldown, and suppressed events are consumed without a backlog. Original clipboard completion times and session generations reject notices at least five seconds old or from a retired session. Native posting preserves that deadline through registration/retry, removes the prior owned entry, and retires expired requests through the existing poll. Windows shell policy and scheduling determine actual banner display; an exact banner lifetime is not guaranteed. No text, image thumbnail, filename, peer identity or address is included.

The project opener accepts no frontend URL and opens only the fixed repository address. The firewall action accepts one literal IP or stored endpoint and extracts a single normalized unicast peer IP. Only an explicit click starts the system Windows PowerShell executable with `runas`; the desktop/input engine stays unelevated. The helper is compiled into the app, isolates its process-local module lookup to OS modules, and imports NetSecurity from its fixed system path. Its inbound allow rule is limited to the current executable, TCP 24859, that peer and Domain/Private profiles. Existing rules must match exactly; the helper never broadens a conflicting rule. An owned admission guard follows the elevated process through actual exit even if its UI request is cancelled. This action creates no service or automatic firewall exception.
