# Protocol and session behavior

Easy Mouse Connect uses an authenticated encrypted protocol over TCP, with separate connections for pairing, input/control and bulk transfer. This document describes the 0.4.1 Windows preview. Run the same application revision on both PCs: there is no stable wire-compatibility promise or version-negotiation mechanism. The authoritative definitions are `crates/emc-protocol/src/lib.rs`, `crates/emc-core/src/pairing.rs`, `crates/emc-core/src/transport.rs` and `crates/emc-core/src/engine.rs`.

## TCP endpoints and preamble

The default listener port is **TCP 24859**. The app uses separate TCP connections for pairing, input/control and bulk transfer. Each connection begins with a cleartext six-byte preamble written by the initiator:

```text
Offset   Size   Value
0        5      ASCII EMC02, bytes 45 4d 43 30 32 (hex)
5        1      Channel: 01 input/control, 02 bulk, 03 short-code pairing
```

Current incoming connections require `EMC02`; 0.3.7 uses `EMC01` and cannot connect to 0.4.0 or 0.4.1. Version 0.4.1 retains 0.4.0 wire compatibility, including `BulkKind::Image` and `PeerActivity`; use the same 0.4.1 build on both PCs for its clipboard fixes. Update without deleting identities or re-pairing. The explicit outbound legacy import path described below alone writes `EMC01` with channel 00. Current receivers reject that incoming legacy path as well as unknown magic/version and unknown channel values. It allows five seconds to receive the preamble. The exact six preamble bytes are bound into the Noise prologue, binding version and channel selection to the handshake. Channel 3 additionally binds the PAKE transcript as described below. Changing that selection in transit does not create a valid session on another channel.

The preamble and record lengths are visible to a network observer. Names, input, clipboard and file data are application messages inside authenticated encryption. Traffic sizes, timing, endpoints and connection duration remain observable.

## Frames and encoding

Noise handshake records and subsequent encrypted records share this framing. The preceding channel-3 PAKE records are the fixed-size exception described below:

```text
2 bytes       N bytes
u16 big-endian length N | Noise handshake message or encrypted record
```

The length excludes its own two-byte prefix. `N` must be between 16 and 65,535 inclusive and is checked before allocating the record buffer. There is no unbounded length field or plaintext-message fallback. Each write has a five-second timeout. The complete Noise handshake has an eight-second timeout, in addition to the receiver's preamble timeout.

After the handshake, each decrypted record contains one Postcard-serialized `Message`. The maximum encoded plaintext is **65,519 bytes** so its 16-byte authentication tag fits the u16 ciphertext length. Decoding rejects oversized input and trailing bytes. Larger application content is split into bounded transfer chunks, not placed into a larger frame.

Postcard serializes the Rust enum and its fields, so enum variant order and field changes affect interoperability. Before incompatible public releases, change/version the protocol deliberately and define mixed-version behavior. Existing peers reject unsupported preamble versions; there is no downgrade or feature-negotiation exchange. `EMC02` alone does not promise compatibility between arbitrary development revisions.

## Short-code pairing

New pairing uses channel **03**. The receiver generates 12 unbiased characters from `0123456789ABCDEFGHJKMNPQRSTVWXYZ`, giving **60 random bits**, displayed as `XXXX-XXXX-XXXX`. Parsing ignores hyphens/ASCII whitespace, accepts lowercase, and maps `O` to `0` and `I`/`L` to `1`. The code is valid for **300 seconds** with at most **five admitted attempts**. The receiver reserves an attempt before the PAKE exchange; failed and abandoned attempts count. Generating another code replaces the previous window. Success installs trust and consumes the code.

The code is the password for **RustCrypto `spake2` 0.4 / Ed25519Group**. It is not transmitted as plaintext or hashed directly into a Noise PSK. After the six-byte `EMC02` + channel-03 preamble, the initiator sends one fixed **33-byte** SPAKE2 message and the responder sends one fixed **33-byte** message. These are raw fixed-size messages, without a peer-selected length or u16 frame prefix. The PAKE exchange has a ten-second timeout. Its role identities are fixed project/domain strings, not device names.

`pairing.rs` computes a SHA-256 transcript over its domain separator, six-byte preamble, role identity strings, initiator message and responder message in that order. It uses this digest as the salt for HKDF-SHA256 over the SPAKE2 shared secret, expanding with a separate Noise-PSK label to 32 bytes. Each fresh exchange therefore derives a session-specific PSK. The derived output length is not a claim that the displayed code has 256 bits of entropy.

On the same TCP connection, `Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s` then confirms the derived key and exchanges static device identities. Its prologue is the six-byte header, the `EasyMouseConnect/Pairing/v2/Noise-XXpsk3/transcript` separator and the 32-byte transcript digest. Both the selected channel and this particular PAKE exchange are bound to Noise. Wrong codes, wrong roles, transcript substitution and malformed exchanges must fail rather than select another authentication mode. The eight-second Noise timeout and framed-record bounds still apply after PAKE.

After authentication, the initiator sends encrypted `Hello`; the responder returns encrypted `Hello`. Each greeting contains a nonempty device name of at most 128 UTF-8 bytes without control characters. Greeting reads have a five-second timeout. `Ready` messages confirm the exchange around persistent trust installation. Installation rechecks generation, enabled/shutdown state, and the same still-unexpired code window before persisting peer keys. Replaced or expired codes cannot authorize a stale in-flight exchange.

The project and its SPAKE2 integration are not independently audited. The upstream [RustCrypto 0.4 security notice](https://docs.rs/spake2/0.4.0/spake2/#security-warning) also states that the crate has not received an independent audit. Keep code generation, attempt bounds, transcript binding and implementation review in scope; use of a PAKE library alone does not establish the application protocol's security.

A failure around persistent confirmation can leave the PCs with different local pairing states. Inspect both. If one genuinely has an incomplete pairing, deliberately forget that incomplete state and generate a fresh code. Do not replace identities automatically, and do not forget an established peer merely to upgrade.

## Existing peers and legacy import

Versions 0.2.0 through 0.4.1 preserve the version-1 DPAPI identity/settings format and existing pinned peer keys. The current control/bulk IK paths use the new `EMC02` preamble. Established peers can reconnect without a new code or Forget device. This storage/authentication preservation does not promise interoperability between arbitrary development revisions; use the same updated build on both PCs.

The Rust pairing API retains an explicit outbound import branch only when its supplied credential begins with `emc1.`. Such an older token contains the receiver public key and a 32-byte secret, encoded as URL-safe Base64 without padding, and uses the older `EMC01` channel-00 XXpsk3 path with destination pinning. This version **does not generate** those tokens and **rejects incoming channel-00 pairing**. Normal settings pairing accepts the new short code. A failed channel-03 exchange never falls back to channel 00 or to plaintext. Code-shaped and legacy-token-shaped clipboard text is conservatively excluded from clipboard synchronization.

## Trusted reconnect and role

Input/control and bulk reconnects each use `Noise_IK_25519_ChaChaPoly_BLAKE2s`. The initiating device knows the receiver's stored public key, and the receiver verifies the initiator against its stored peer key. Both channels authenticate independently, even when one is already connected. An address is only a network locator.

The device that originally entered the pairing code (or the earlier invitation for an existing pairing) retains the dialing role. While sharing is enabled and a peer exists, its reconnect loop runs every **three seconds**. It opens control first and then the independent bulk connection. Manual **Reconnect** also belongs on that device. The code-generating device listens and does not automatically reverse roles after a disconnect.

Each installed control session has a generation. A bulk connection also has its own generation. Installation rechecks the authenticated key, expected generation, sharing state and shutdown state. Pausing, disconnecting, revoking or replacing a session invalidates its authority. Old socket tasks must not clear the state of a newer session when they finish.

Native events are queued with a connection epoch and discarded if that epoch becomes stale. Incoming transfers carry their owning bulk generation. Outgoing transfers capture their sender and generations when offered; they cannot continue through a replacement sender or newly paired peer. Reconnecting re-establishes sessions, rather than resuming a partially sent file.

## Message channels

| Message | Channel | Meaning |
| --- | --- | --- |
| `Hello { name }`, `Ready` | Pairing | Exchange names and confirm persistent pairing |
| `Input(event)` | Input/control | Mouse movement/buttons/wheels, keyboard scan codes, edge enter/leave and release |
| `Ping(value)`, `Pong(value)` | Input/control | Liveness and round-trip timing |
| `Decision { id, accept }` | Input/control | Recipient accepts or rejects a bulk offer |
| `Received { id }` | Input/control | Recipient confirms successful completion/application |
| `PeerActivity` | Input/control | Authenticated activity indication for the optional receiver keep-awake lease; no input content |
| `Offer { id, name, size, kind }` | Bulk | Propose text, image or file content with a declared byte count |
| `Chunk { id, data }` | Bulk | Bounded bytes for the accepted transfer |
| `Complete { id, sha256 }` | Bulk | Declare completion with a SHA-256 digest |
| `Cancel { id }` | Bulk | Cancel matching transfer state |

Unexpected messages fail the receiving channel. Transfer IDs are 32 hexadecimal characters. A file name is a bounded Windows-safe basename, never a peer-selected destination path.

Input uses relative movement, standard five-button/wheel events and keyboard scan-code/virtual-key metadata. Edge entry carries direction and normalized vertical position in the 0–65,535 range. These are desktop input semantics, not hardware-level Logitech configuration or a cross-platform keyboard-layout abstraction.

## Liveness and local release

The control loop ticks every **one second**. It sends a ping when there is no outstanding ping and records at most one outstanding value. A matching delayed pong clears that value and updates displayed round-trip timing. The peer echoes the received value; an unexpected pong is rejected.

When a liveness check observes **four seconds without a freshly received authenticated message**, it tears down the control session and releases native input. Local emergency release and queue failures also return control. These are application policy timers, not a guaranteed maximum elapsed recovery time under blocked OS calls, scheduler stalls or network write timeouts.

The authenticated reader, encrypted writer and ordered native dispatcher run independently. Outgoing, authenticated-dispatch and native-input queues each hold at most 128 entries; only adjacent relative moves merge with checked sums and oldest timestamps. Enter, key/button/wheel, Leave/ReleaseAll and other control messages are barriers. A 200 ms local capture/receipt-to-processing/send budget releases the session on stale work or overload. This budget is not a network transit-time claim. EOF/cancellation clears pending jobs and revokes the native epoch before old workers can feed a replacement session. Liveness is updated in the authenticated reader, and RTT diagnostics include sample age.

Bulk establishment has a bounded control-readiness notification before its generation snapshot, followed by independent IK authentication and final pin/generation checks. Readiness adds no plaintext application message or new trust authority. Each length prefix and encrypted payload is issued as one bounded TCP write.

Outgoing bulk establishment is serialized through a local committed-installation acknowledgment. Concurrent attempts can coalesce only into the exact next bulk generation, and cancellation leaves the guard with an already-spawned installer. A pre-handshake bulk generation is rechecked at commit as well as the control generation, pin and policy. An authenticated incoming attempt waits up to two seconds for an old bulk slot to close naturally, while retaining its original generations/key and holding no transfer mutex; an occupied slot is rejected rather than evicted. This local coordination adds no remote acknowledgment or authority. A locally known pause has a typed diagnostic; an unexplained remote EOF cannot reveal peer policy.

Failed injection, stale work and overload disconnect and release input. The protocol has no ownership-recovery acknowledgment that would make a receiver-only reset equivalent to a coordinated release. `Leave` carries no return-height coordinate. A receiver failure cannot justify continuing to suppress the sender's local input without acknowledgment.

Receipt deadlines continue through native queueing and commit instead of restarting at enqueue. Windows insertion and cleanup share one bounded ordered executor outside the hook/raw thread. Prepare, actual insertion and guarded hook-state commit remain ordered under the original 200 ms deadline. A local ownership token revokes pending work after reclaim/reset even if the connection survives; it is not a wire identifier. Completion acknowledges actual insertion/commit, never enqueue alone. Late accepted holds are cleaned up before replacement input, and obsolete optional return warps cannot acquire replacement ownership. Signed wheel deltas and all action barriers retain their order. [Architecture](architecture.md) describes native scheduling, edge admission and recovery; aggregate diagnostics are local metadata rather than additional wire messages.

The transport sets `TCP_NODELAY` for small messages and keeps file bytes off the input stream. TCP still retransmits and delivers records in order. This implementation does not promise datagram behavior, zero latency or measured performance on relay paths.

## Clipboard and file transfer

Text and image clipboard sync are independent optional permissions and default off on each PC. Text is limited to **1,048,576 UTF-8 bytes**. Images use `BulkKind::Image` and canonical PNG, with at most **16,777,216 encoded bytes**, **67,108,864 decoded RGBA bytes**, and **16,384 pixels on either axis**. The receiver validates format, dimensions and decoded size rather than trusting the declared encoded count. Clipboard transfers can be accepted automatically only when the corresponding clipboard permission is enabled on both ends; file transfer permission is separate and files require an explicit recipient decision.

Initial activation establishes a clipboard baseline rather than sending its existing content immediately. Clipboard baselines, reads, content hashes and pending sends remain bound to their activation revision and original control/bulk generations. Track the active clipboard kind when baselining so a text/image transition and later recopy of an earlier local value remain new activity. Image identity uses decoded pixel content for echo suppression, together with a received-image clipboard marker; publishing additional formats or file metadata must not reflect a received image back to its origin.

File offers require a regular source file and a declared size **strictly below 100,000,000 bytes**. Exactly that many bytes is rejected. On approval, the receiver creates a dedicated transfer directory beneath `Downloads/EasyMouseConnect` and a new `receiving.part` file. Unsafe names, reused directories and unexpected/unaccepted chunks are rejected.

The sender waits up to **65 seconds** for offer approval. It serializes outgoing transfers and sends chunks of at most **49,152 bytes (48 KiB)**. The receiver enforces accepted ID, ownership, current policy and declared total length while updating a SHA-256 digest. An incoming transfer has an absolute **120-second lifetime from offer creation**, including its approval wait; this is not extended by progress.

On `Complete`, the receiver requires the exact declared byte count and matching digest. For text, it validates UTF-8 and invitation exclusion before applying the clipboard under current policy. For images, it validates and canonicalizes PNG before publishing Windows bitmap formats and a managed local PNG file representation for Explorer. Image preparation and final application recheck the owning session and image policy; a cancelled old transfer cannot populate a replacement session. For files, it flushes/synchronizes temporary content, rechecks session ownership and sharing policy at commit, applies Windows internet-zone metadata to the temporary file, then renames it to the safe final name. The app does not open received files. Failed transfers clean up partial state rather than reporting success.

Only after successful application/save does the receiver send `Received { id }` on the authenticated control channel. The sender waits up to **15 seconds** for that acknowledgment. An absent acknowledgment is a failure, even if the file may already have been committed before a connection failed. Check the receiver before retrying. An authenticated acknowledgment is the peer's statement; it does not establish honesty of a compromised trusted endpoint.

Disabling a feature, revoking trust or changing session ownership cancels the affected transfer. These operations must never move its remaining bytes into a replacement session.


## Windows image application and cache

The receiver supports image paste through Windows bitmap clipboard representations and a generated PNG file representation. The latter lets Explorer paste a screenshot as a file without sending an arbitrary peer-selected path. It is image clipboard authority, independent of consent-based app file transfers. Unsupported source clipboard formats are not parsed as arbitrary files.

For a source clipboard containing exactly one local image-file path (`CF_HDROP`), 0.4.1 can decode a regular local PNG, JPEG or static WebP file of at most 16 MiB under the existing decoded/dimension bounds. Folders, multiple paths, UNC locations, mapped network files and unsupported/animated formats are outside this import. The path is local source data, never a peer-selected destination or command. Only validated pixel content is encoded into canonical PNG; the original filename, file format and arbitrary file bytes do not become transfer authority. Enabling image clipboard authorizes this conversion without a new recipient decision, while original-file preservation still uses explicit app file transfer and consent. This adds no wire message or permission.

Managed image files live under `%LOCALAPPDATA%\EasyMouseConnect\clipboard-images`, with at most four retained managed PNG files of at most 16 MiB each. Filenames are generated from content, receive-path traversal is unavailable, and Windows `Zone.Identifier` zone 3 is applied before publishing a file representation. Cache preparation and cleanup must not delete a currently referenced clipboard file or publish an unmarked file. Rejection or cache exhaustion is a failure, rather than silently exceeding the bound. PNG cache contents are private user data, not logs or DPAPI-encrypted identities; diagnostics and source packages exclude them. A temporary image being prepared is separate from the retained-cache count.

## Activity lease and notifications

Both PCs must enable `keep_peer_awake`. Recent genuine local input is eligible for **30 seconds**; while eligible, the controller can send an authenticated content-free `PeerActivity` at most every **three seconds**. A current permitted receipt renews a **15-second** receiver lease. Session replacement, disconnection and policy revocation clear authority; an absent renewal lets the lease expire. The Windows user-session power request prevents display-off and idle sleep only. It does not send synthetic input, alter screen-saver/security-lock settings, bypass a secure desktop or require a service/elevated app.

Optional clipboard/network system tray notifications are local presentation, not new wire authority. Clipboard notices are receive-only and share a 30-second cooldown; network notices have a five-second cooldown. Events older than five seconds are discarded before display. Messages contain no clipboard data, image preview, peer name, address or path. Windows notification policy can suppress their display.
