## Context

The main repository still contains a Linux build recipe, AppImage recipe, renderer backends, Linux SDK packaging, and dormant CI build step, but the active release matrix and asset uploads explicitly pause Linux. The release publisher already recognizes one Linux AppImage in release mode, while artifact-mode mapping, the tar archive, production manifest metadata, and client handoff are incomplete. The client also compiles and displays its NLE installer only on Windows and macOS even though the plugin repository already builds a Linux x86_64 OpenFX ZIP and DaVinci Resolve officially supports Linux.

The change crosses three repositories with different responsibilities:

- `gyroflow` owns application build/packaging, GitHub Actions, 123 publishing, release-center policy, manifest templates, the Rust client, and QML.
- `docs` owns the production `api/_control-plane.js` manifest implementation deployed by Vercel.
- `gyroflow-plugins` owns the Linux OpenFX binary, ZIP layout, and plugin release workflow.

The user approved Linux x86_64 as the only supported architecture, AppImage plus tar.gz as required release assets, AppImage as the in-app update package, an open-containing-directory update handoff, Linux Resolve/OpenFX without Adobe, no NeuFlow change, and no Linux GPU zero-copy work.

## Goals / Non-Goals

**Goals:**

- Restore a tested Linux x86_64 build and release path that produces both required application assets.
- Make release, nightly, GitHub Release, artifact-source, 123, release-center, template manifest, and production manifest agree on the same Linux asset contract.
- Preserve backward compatibility while adding optional tar archive metadata to Linux package records.
- Complete Linux AppImage download, verification, executable-permission preparation, and open-directory handoff without privileged self-replacement.
- Complete Linux DaVinci Resolve/OpenFX build, publication, installation, script placement, detection, update, and error reporting.
- Fail early and visibly when a Linux deliverable or required bundle member is absent.

**Non-Goals:**

- Linux ARM64 or any architecture other than x86_64.
- NeuFlow/AI Sync enablement or model/runtime distribution changes.
- VAAPI, Vulkan, or DMA-BUF zero-copy stabilization work.
- Adobe, Final Cut Pro, or other unavailable Linux host integrations.
- Replacing the current Linux compatibility baseline with a new container distribution or packaging framework.
- Automatic replacement of the running AppImage or package-manager integration.

## Decisions

### D1: Treat Linux x86_64 as the only supported Linux build contract

`_scripts/linux.just` will reject `FORCE_ARCH=aarch64` and other non-x86_64 requests before dependency installation or compilation. The root command surface will be completed for Linux, but no ARM64 artifact name, manifest entry, CI matrix row, or feature claim will be introduced.

The alternative was to repair the dormant cross-compilation branch. It was rejected because the current recipe installs a Rust target without building with that target, the bundled BRAW/RED SDK path is x86_64-only, upstream does not publish Linux ARM64 binaries, and no ARM64 validation host is in scope.

### D2: Publish AppImage and tar.gz as independent required assets

The fixed raw filenames are:

- `gyroflow-niyien-linux64.AppImage`
- `gyroflow-niyien-linux64.tar.gz`

Tag releases upload both as raw `archive:false` artifacts and attach both to GitHub Release. Manual/nightly runs use two single-file V4 artifacts named `gyroflow-niyien-linux-appimage` and `gyroflow-niyien-linux-tar`. The publisher maps each short artifact to one raw filename, preserving its existing single-deliverable extraction and cache model.

The alternative was one wrapper containing both files. It was rejected because independent SHA-256, size, inventory, URL, cache, and failure reporting are required, and the current artifact resolver maps one artifact name to one raw file.

### D3: Keep AppImage primary and add an optional archive channel

`packages.linux.package_*` remains the primary update channel and describes the AppImage. Linux adds `archive_url`, `archive_sha256`, and `archive_size` for the tar.gz. Artifact policy adds the corresponding `app_urls.linux.archive_url`. Older policy entries, clients, and consumers can ignore or omit these fields without changing AppImage behavior.

The alternative was a generic array of package variants. It was rejected because it would require a larger schema migration across every platform and consumer for a two-file Linux-specific need.

### D4: Coordinate the three repositories as one vertical release contract

Implementation and verification order is:

1. `gyroflow-plugins` produces the Linux OpenFX ZIP contract.
2. `gyroflow` accepts/publishes that plugin asset and produces both application assets.
3. `docs` consumes the extended policy and emits production Linux manifest fields.
4. Cross-repository contract tests compare filenames and manifest behavior before any release is promoted.

No production deployment, tag creation, or policy promotion is performed by this change implementation. Those external state changes remain explicit release operations after the code is reviewed.

### D5: Open the verified AppImage's containing directory instead of replacing it

After download and SHA-256 verification, Linux sets user executable bits on the cached AppImage. Handoff opens the parent directory using `xdg-open`, falling back to `gio open`, and leaves Gyroflow running. QML instructs the user to exit, replace the old AppImage, and restart. Failure of both openers returns the absolute cached path in a readable error.

Automatic self-replacement was rejected because the running binary may come from a tar directory, a read-only AppImage mount, or a system-managed location and would require privilege, rollback, and install-origin detection.

### D6: Install Linux OpenFX system-wide with constrained privilege escalation

The standard destination is `/usr/OFX/Plugins/GyroflowNiyien.ofx.bundle`. The client first attempts an ordinary directory copy. On permission failure it uses `pkexec` with fixed executable arguments and a fixed destination; it never constructs a shell command from network or UI text. The extracted source is canonicalized and validated to contain the exact Linux x86_64 bundle before escalation. If PolicyKit is unavailable or declined, the temporary directory is preserved and the UI shows a manual installation command.

Resolve Utility scripts are copied without elevation to `~/.local/share/DaVinciResolve/Fusion/Scripts/Utility`. Linux detects Resolve under `/opt/resolve`, the installed bundle under `/usr/OFX/Plugins`, the binary under `Contents/Linux-x86-64`, and the version from an explicit package version file.

A user-only OFX location was rejected because the OpenFX Linux standard guarantees `OFX_PLUGIN_PATH` and `/usr/OFX/Plugins`, not a universal per-user directory scanned by every host.

### D7: Use tests and package inspection as release gates

Behavioral helpers will be made platform-parameterized where practical so Windows development can test Linux filename/path selection. Linux CI remains the authoritative compile/package check. The Linux job will run checks/tests, build both packages, inspect tar and AppImage contents, run the packaged binary with `--version`, and report unresolved dynamic libraries. Plugin CI will inspect the ZIP contract and Linux plugin dependencies.

Configuration and workflow edits will receive static contract tests first. Runtime code will follow red-green TDD. A live Resolve install/restart/menu check remains a final manual acceptance item and cannot be marked complete without a real Linux Resolve environment.

## Risks / Trade-offs

- **[R1] The existing Buster/Python/AppImageBuilder baseline is old** -> Keep the compatibility baseline for this change, pin/probe every invoked tool and asset, and fail at the producing step; migrate the packaging stack separately.
- **[R2] GitHub artifact semantics differ between tag and nightly paths** -> Use one raw file per named artifact and test both mapping tables and wrapper extraction.
- **[R3] Template and production manifest implementations can drift** -> Add equivalent Linux contract tests in both repositories and compare normalized package output shapes.
- **[R4] PolicyKit is not installed or the user cancels authentication** -> Preserve the extracted ZIP, return a typed/sentinel failure, and provide a fixed manual command.
- **[R5] A plugin producer/client rollout mismatch causes 404s** -> Land and verify the plugin asset producer before enabling the main-client Linux asset contract.
- **[R6] Linux distributions and GPU drivers vary** -> Gate release on package extraction, `--version`, and dynamic-library checks; document that hardware encode availability remains driver-dependent.
- **[R7] Live Resolve validation may not be available in the implementation environment** -> Keep the acceptance task pending and report the exact unverified step rather than claiming full live validation.

## Migration Plan

1. Add failing contract tests in all affected repositories.
2. Repair and validate the plugin producer contract, then retain its exact asset name in the main publisher and client.
3. Repair the Linux build recipe and restore the two application assets in CI.
4. Extend publisher, release center, policy, template manifest, and production manifest metadata.
5. Implement Linux client update handoff and NLE installation paths.
6. Run repository-local tests, Linux container builds, artifact smoke checks, and OpenSpec strict validation.
7. On a Linux x86_64 workstation, install the AppImage/tar build and the OpenFX ZIP, restart Resolve, and verify the plugin and two Utility menu entries.

Rollback is performed by removing the Linux matrix row. Required asset derivation then stops requiring Linux assets, while optional manifest fields remain backward compatible. No automatic updater or plugin action overwrites the running application or Resolve itself.

## Open Questions

None. Architecture, package formats, updater handoff, NLE scope, cross-repository scope, AI Sync scope, and zero-copy scope were explicitly approved before artifact creation.
