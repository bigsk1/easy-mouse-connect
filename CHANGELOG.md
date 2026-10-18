# Changelog

## 0.3.6 — maintenance preview

- Upgrade diagnostic ZIP support from `zip` 2.6.1 to 9.0.5, outside the affected ranges in [RUSTSEC-2026-0367](https://rustsec.org/advisories/RUSTSEC-2026-0367.html), [0368](https://rustsec.org/advisories/RUSTSEC-2026-0368.html) and [0369](https://rustsec.org/advisories/RUSTSEC-2026-0369.html). These advisories concern extraction; production uses ZIP writing for diagnostic exports. The existing export test handles the updated filename API.
- Wait for rendered navigation in browser regressions and exercise deferred hash-change delivery. Failed Windows UI checks retain the isolated failure screenshot in Actions artifacts.
- Configure Dependabot for security-update PRs only. Routine dependency migrations remain deliberate maintenance changes.

Input routing, the ordered injection worker, encryption, pairing, persistent identities and clipboard/file policies are unchanged. Crypto/randomness dependencies retain their existing versions. Use the same version on both PCs; hardware observations from 0.3.5 do not establish acceptance for a new build.

## 0.3.5 — initial public preview

Windows x64 source preview with encrypted mouse/keyboard sharing, optional text clipboard and approved file transfers, tray controls and bounded local diagnostics. The full hardware acceptance matrix and independent security review remain incomplete.
