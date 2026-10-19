# Windows preview releases

Published Windows x64 previews appear in [GitHub Releases](https://github.com/bigsk1/easy-mouse-connect/releases). Each release contains the existing **Easy Mouse Connect Preview** per-user installer, the portable executable, instructions, a sanitized source snapshot and checksums. Preview installers are unsigned and require Microsoft Edge WebView2 Runtime. Use the same app version on both PCs.

| Asset | Purpose |
| --- | --- |
| `EasyMouseConnect-<version>-preview-setup.exe` | Per-user preview installer with shortcuts and an uninstaller |
| `easy-mouse-connect.exe` | Portable executable; each PC creates its own local identity |
| `START-HERE.txt` | Pairing, upgrade, uninstall and troubleshooting instructions |
| `Easy-Mouse-Connect-source-<version>-preview.zip` | Sanitized source snapshot from the packaged revision |
| `SHA256SUMS.txt` | SHA-256 hashes of the four payload files |

Download assets from one release. Compare a downloaded file with its entry in `SHA256SUMS.txt` using PowerShell:

```powershell
Get-FileHash .\EasyMouseConnect-0.3.7-preview-setup.exe -Algorithm SHA256
```

Checksums identify matching bytes; they are not a publisher signature or a reproducible-build claim. GitHub's automatic source archives are separate from the explicitly sanitized source ZIP. Before upgrading, Quit both tray apps and retain protected identity/settings data. Existing `Easy Mouse Connect Preview` installations use this same preview installer format. If Windows lists a separate **Easy Mouse Connect** installation from the normal Tauri installer, Quit and uninstall that installation before installing the preview; uninstall preserves identity/settings and received files.

## Build and create a draft

The **Verify and package** workflow retains the required `Core (ubuntu-latest)`, `Core (windows-latest)` and `Windows desktop and unsigned installer` checks. Pushes, pull requests and ordinary manual runs keep read-only repository permissions. The Windows job completes the normal Tauri NSIS build, then packages that same executable with `scripts/package-preview.py` and stages the five preview assets with `scripts/stage-preview.py`. The normal Tauri installer remains a build check; the downloadable package uses the established preview registration and install directory.

Git checkout uses a process-scoped `core.autocrlf=false` on all runners so the Windows source snapshot and Linux publisher can be compared byte for byte. Official actions are pinned to verified commit SHAs.

The packaging step uses `makensis.exe` from Tauri's `%LOCALAPPDATA%\tauri\NSIS` cache, or an available `makensis.exe` on `PATH`. It requires the accompanying NSIS include files and fails if no complete compiler is available. It does not install or run the application. Every successful Windows run uploads the staged package as `easy-mouse-connect-windows-x64-unsigned-<commit>` in Actions artifacts.

To prepare a release, update the checked-in version manifests and `packaging/RELEASE-NOTES.md` together, then merge the change through the required checks. The notes start with `# Easy Mouse Connect <version> preview`; describe that version's changes, verification evidence and known limits. Documentation/publication changes do not change the runtime version. Preserve accepted local packages and stage any new package in a fresh destination.

1. Open **Actions → Verify and package → Run workflow** and select `main`.
2. Set **Create an unpublished preview release draft** to true, then run the workflow.
3. After the required build succeeds, the publisher downloads that run's staged artifact, validates its exact files and checksums, verifies matching manifests and the checked-out source commit, and creates a **draft prerelease** named `Easy Mouse Connect <version> preview` with tag `v<version>`.
4. Review the draft's notes, target commit and all five assets. Check relevant Windows and two-PC acceptance evidence, security/dependency review and unsigned-publisher status. Publish the prerelease manually only after that review.

The publisher runs only for a manual dispatch by the repository owner from `main`, with the draft input enabled and a successful Windows job. Only that job has `contents: write`, using the workflow's GitHub token. It checks out the exact run commit with credentials disabled and uploads the assets from the same run. An existing release is an error; the helper does not replace published assets. A matching existing tag must identify the exact commit. Drafts are not public downloads, and this workflow never publishes them or installs an automatic updater.

## Verification status

The 0.3.5 baseline has limited two-PC Windows 11 mouse/keyboard/scrolling and basic file-send observations. The 0.3.6 dependency-maintenance build preserves the input-routing implementation; the source review uses that revision as its baseline. Version 0.3.7 applies surgical core-policy fixes from that review and still awaits physical acceptance. A successful CI/package build does not inherit hardware acceptance from 0.3.5.

The [AI-assisted source review](security-review.md) is additional review evidence, not an independent professional security audit. It does not establish the absence of vulnerabilities or replace dependency review, adversarial runtime tests, installer review or the [hardware acceptance matrix](manual-testing.md). Record the exact revision and cases actually exercised. See [development](development.md), [SECURITY.md](../SECURITY.md) and the [threat model](threat-model.md) for the relevant boundaries.
