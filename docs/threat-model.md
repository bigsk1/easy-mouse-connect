# Threat model

Easy Mouse Connect grants an explicitly paired PC control of the signed-in user's ordinary desktop. This document describes the current trust boundaries and intended protections. It is not an independent audit or a claim that all attacks have been excluded. See [SECURITY.md](../SECURITY.md) for vulnerability reporting and [manual testing](manual-testing.md) for acceptance cases.

## Assets and trust

The protected assets are device private identities, pairing secrets, user input, clipboard/file content and the receiving filesystem. A paired peer is trusted to generate input while sharing is enabled. Clipboard and file permissions are separate from mouse/keyboard sharing and default off.

Trust boundaries separate:

- Unauthenticated sockets from PAKE/Noise sessions and pinned identities.
- Authenticated peers from current per-feature policy and session ownership.
- Peer-provided file metadata from the local filesystem.
- The local WebView from Rust commands and native resources.
- The ordinary desktop from higher-integrity applications, UAC and secure desktops.

The OS, current account and native dependencies are part of the trusted computing base. Transport encryption cannot protect against a compromised trusted peer, a malicious same-account process or an administrator controlling an endpoint.

## Attacker model

| Attacker | Intended protection | Remaining exposure |
| --- | --- | --- |
| Passive network observer | Noise encrypts application payloads | IPs, timing, traffic sizes and connection duration remain visible |
| Active LAN/VPN intermediary | Short-code PAKE, transcript-bound Noise pairing and pinned identities on later channels | A stolen valid code can authorize pairing; an attacker can exhaust attempts or deny service |
| Unpaired network client | Authentication before input/clipboard/file authority; bounded frames and handshake timeouts | Listener exposure and connection churn require resource limits and adversarial testing |
| Trusted peer sending malicious metadata | Feature consent, byte limits, safe filenames and isolated receive directories | Accepted files can contain malware; contents are not inspected |
| Trusted peer sending unwanted input | Explicit enabling, pause/disconnect, revocation and emergency release | An enabled peer has substantial user-session authority |
| Compromised settings frontend | Local assets, CSP, narrow commands and Rust validation | A frontend compromise may invoke exposed commands; capabilities remain security-sensitive |
| Compromised local account | Current-user DPAPI protects stored data at rest | Same-account code can request decryption or control the session |

## Pairing and independent channel authentication

Pairing codes contain 12 unbiased base32 characters, or 60 random bits, displayed in three groups of four. A window lasts five minutes and admits at most five attempts. Failed/abandoned exchanges consume attempts before cryptographic work. Replacing a code invalidates the previous window. Reachable attackers can consume this budget, so regeneration is a local recovery action rather than permission to bypass authentication. Possession of a valid code grants initial pairing authority; keep codes out of logs, screenshots and reports.

Channel 3 uses RustCrypto SPAKE2 0.4/Ed25519Group, then HKDF-SHA256 over its shared secret with a domain/preamble/role/message transcript digest as salt. The resulting 32-byte PSK feeds Noise XXpsk3; the same transcript enters its prologue before static identities and application data are accepted. Wrong codes, role confusion and channel/transcript substitution fail closed. This is a PAKE bootstrap, not a direct hash of the code; a 32-byte derived key does not make the displayed credential 256-bit. Upstream [security/timing caveats](https://docs.rs/spake2/0.4.0/spake2/#security-warning) apply, and neither the dependency nor integration has an independent audit claim.

Every reconnecting control and bulk channel independently authenticates pinned keys with Noise IK. Addresses, names and an already-open socket do not establish authority for another channel. Installation rechecks identity, generation, enabled/shutdown state and the still-valid pairing window before persisting trust. Revocation must terminate active authority as well as future reconnects. A confirmation failure can leave asymmetric local pairing state, which needs deliberate recovery rather than automatic trust replacement.

Version-1 DPAPI identity/settings data and existing IK peers are preserved through 0.3.5. Upgrading does not require resetting identity or forgetting a trusted peer. Incoming legacy channel-0 pairing is rejected and no new legacy invitation is generated. An explicit outbound `emc1` import branch is compatibility code, never a downgrade after PAKE failure. See [protocol](protocol.md) for exact framing and binding.

## Session generations and bulk ownership

Control and bulk channels have separate generations. Installation and commit boundaries recheck both relevant generations, current identity, policy and shutdown state. Incoming transfers retain their owning bulk generation. Outgoing transfers capture a sender and generations at offer creation, so reconnect/re-pair cannot adopt their remaining bytes.

Outgoing bulk dialing is serialized through committed installation even if the caller is cancelled. A pre-handshake bulk generation cannot install after policy revocation while the control generation remains unchanged. An authenticated incoming attempt can wait at most two seconds for an old slot to retire naturally; it never evicts a live channel, holds no transfer mutex while waiting and cannot follow a replacement session. Readiness notification is local coordination rather than a new unauthenticated authority.

Input/control and bulk streams are separate, so file content cannot fill input queues. Feature policy is enforced in Rust on send and receive. An unexplained remote EOF cannot identify the remote policy or prove an authentication failure.

## Native capture and local release

Raw relative input supplies motion and current-position edge admission; low-level hooks suppress physical input only after fresh relative readiness. Injected echoes are excluded. Unsupported absolute input, queue failure, desktop/display changes and stalled raw delivery release routing locally. Raw/hook ordering and packet identity are not guaranteed by Windows, so the source guard observes progress rather than exact timestamp matches: recent raw-first delivery has a 16 ms allowance, otherwise the first suppressed observation starts a 250 ms missing-progress deadline that subsequent hooks cannot renew. A recent coalesced packet cannot prove every individual event arrived. Receiver pass-through input cannot satisfy this source-only guard.

Geometry uses actual occupied monitor rectangles in physical pixels, including negative origins and gaps. Edge pressure changes interaction policy, not remote authorization. One raw-path cursor sample, four outward X units at the same eligible continuous span, held-input checks, native readiness, current side and original epoch remain required through Enter/commit. Corner margins and the receiver's fresh-return gate reduce accidental handoff. Source-pointer hiding is unimplemented; no input-owning popup, global cursor replacement or registry workaround is used.

Physical holds and injected holds are distinct. Reconciliation outside hooks clears only stale local pass-through state; suppressed-origin holds require an observed release or device reset. Anonymous device tracking stays bounded and process-local. Relevant/unknown/ambiguous removals release routing; only attributable holds clear, preserving another device's same-key/button hold. Raw-held inputs also block handoff if a low-level DOWN was missed. Cleanup keyup alone cannot reclaim remote ownership.

Receiver movement reclaim uses eight relative hardware units of net displacement within 200 ms to filter sensor noise. Buttons/wheels, keydown and the emergency **Ctrl+Alt+Escape** remain immediate. Raw button/wheel observations cancel pending Enter when a hook is missed, without treating tagged injected echoes as local intent.

## Queue, deadline and insertion boundaries

Authenticated reading, encrypted writing and ordered native dispatch run independently. Three control queues are bounded to 128 entries each. Only adjacent motion coalesces, using checked sums and the oldest timestamp; action/control barriers cannot drop silently. EOF, overload or stale work releases routing, clears backlog and revokes the native epoch. Liveness refreshes on authenticated receipt, never buffered dequeue. The 200 ms local processing/send budget is neither a network-latency guarantee nor a hard OS recovery bound.

Native command wakes coalesce and processing runs in count/time-bounded slices so raw input and maintenance can run during sustained traffic. Original receipt deadlines survive queueing and are checked before insertion and at commit; a ready asynchronous result does not extend them if a timer is polled late.

All `SendInput` calls share one bounded ordered executor outside the hook/raw thread. At most one incoming apply is prepared, inserted and committed at a time; acknowledgment follows actual insertion and guarded commit rather than enqueue. A local ownership token complements the connection epoch because reclaim/reset can end receiving while the connection survives. Pending Enter cannot restore revoked ownership.

The worker tracks actually accepted key/button downs, including late acceptance after cancellation, and cleans up before replacement input. Old-owner cleanup cannot release another live owner's new holds. Idle maintenance retires revoked holds, but valid held input does not expire merely because its original insertion deadline passed. Cleanup requests coalesce and obsolete optional return warps are skipped. A running OS call cannot be preempted or its accepted input withdrawn; local authority revokes promptly and cleanup follows when it returns. No parallel executor is started beside an unfinished worker.

## Native recovery, renderer and tray

Sustained raw activity without low-level delivery may suggest hook failure; idle gaps and installation flags do not prove liveness. Routing releases before replacing hooks/registration, partial installation cannot capture, and fresh motion is required afterward. Registration readback may omit a notification flag while remaining valid. Correct target/classes and compatible background delivery remain mandatory. Unavailable inspection suspends admission without blind repair; confirmed mismatch permits bounded/backed-off repair.

The receiving fallback renderer has its own native UI thread, cached owned artwork/hotspots and lifecycle invalidation. It is click-through and nonactivating, appears only while authorized receiving needs a cursor, and changes no global settings. Hidden valid cursor shapes are preserved. It may be occluded by shell, Start/Search or full-screen surfaces and cannot operate on a secure desktop. Physical acceptance remains necessary for visibility, shape and device behavior.

Renderer failure invalidates ownership. Resource retry is bounded, dead workers restart only after finishing, and hidden recovery cannot restore an old receiving lifecycle. Required fallback checks sampling/resources and actual renderer progress; window-message traffic alone cannot refresh health. A visible native pointer can bypass an optional failed renderer. Hooks never wait for GDI or create recovery workers.

Close hides the settings window while sharing continues. Pause/Stop sharing ends active routing; explicit Quit stops the engine and releases input. Native single-instance handling prevents a second listener/input engine and restores the existing window. An upgrade requires both processes fully quit while retaining protected identity data.

## Clipboard and filesystem boundary

Clipboard sharing defaults off, requires policy on both peers and limits text to 1 MiB of UTF-8. Existing content is not sent immediately on activation. Credential-shaped invitation/code text is conservatively excluded. Encryption does not make an enabled peer safe to receive secrets copied during sharing.

Files default off and require recipient consent. Declared/actual sizes must stay **strictly below 100,000,000 bytes**. Peer names are bounded safe basenames, excluding traversal, alternate data streams, control characters and reserved Windows names. Each transfer uses a generated child under `Downloads/EasyMouseConnect`; existing directories/files cannot be overwritten.

Completion requires exact byte count and SHA-256. Windows internet-zone metadata is applied to the temporary file before final rename under current generation/policy. Marking failure, revocation, truncation and mismatch cannot leave a successful unmarked final file. Only committed application/save earns `Received`, which the sender needs before reporting success. That acknowledgment is the authenticated peer's assertion, not proof of endpoint honesty. Accepted files can contain malware and are never auto-opened/executed. No resumable transfer authority crosses reconnect.

## Diagnostics and frontend boundary

Persistent diagnostics accept typed operational events and allowlisted failure categories without free-text messages. A bounded nonblocking queue feeds a separate writer and three files (`emc.log`, `emc.1.log`, `emc.2.log`), each at most 2 MiB: 6 MiB total. Timing, categories, flags and aggregate counters reveal operational state. Raw input, key identities, content, credentials, user file names/paths, IPs and device names are excluded. Anonymous handles remain in memory only.

Export is an explicit local action. It reparses/allowlists bounded records and numeric/boolean summaries, excluding identities, configuration, arbitrary edited bytes and directory metadata. Only the latest `diagnostic-exports/emc-diagnostics.zip` is retained by the app, with no automatic upload. Startup-failure export reports `engine_available=false` and cannot construct another engine/writer or false live state. Folder openers accept no frontend path or general shell authority.

Exports preserve valid original numeric versions, distinguishing current logger-open/live counters from retained engine starts. Versions and clocks are untrusted operational metadata. Monitor count/virtual dimensions can appear as numbers; review archives and screenshots before public attachment. Processing and insertion-backend durations cannot reconstruct content or identify every exact syscall/target-app delay. Local UI error messages are separate and may need redaction.

The WebView loads local resources, displays peer-provided values as text and exposes narrow Rust commands. Sharing authorization, content limits and file commit checks remain in Rust even if frontend checks are bypassed.

## Scope and remaining assurance work

The app does not cross Windows integrity boundaries, bypass UAC, control the lock screen, resist endpoint administrators or determine whether an accepted file is malware. Continuous connectivity, zero latency and every vendor-specific mouse feature are not guaranteed.

Security/release assurance includes dependency advisories and licenses, malformed-message/adversarial session testing, resource-exhaustion and reconnect races, the Windows hardware matrix, package signing/provenance and an update policy. Use [SECURITY.md](../SECURITY.md) for the private reporting channel and [development](development.md) for the distinction between automated checks and physical acceptance.
