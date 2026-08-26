## Why

Linux desktop support already exists in the codebase, but its release job is paused and the remaining build, distribution, update, and NLE integration paths are incomplete or incorrectly fall back to macOS behavior. Restoring Linux as a supported release target now requires one coordinated x86_64 contract across the main application, the production manifest, and the plugin producer so users never receive a build that cannot be published, updated, or paired with DaVinci Resolve.

## What Changes

- Restore Linux x86_64 to the application release matrix and publish both `gyroflow-niyien-linux64.AppImage` and `gyroflow-niyien-linux64.tar.gz` for tag and nightly builds.
- Make the Linux build surface match the root Justfile, reject unsupported ARM64 requests explicitly, and fail packaging before upload when required files or archive contents are missing.
- Extend release-mode, artifact-mode, GitHub Release, 123, release-center inventory, and production manifest handling so both Linux assets are independently discoverable, verified, and published.
- Keep the AppImage as the primary in-app update package and expose the tar archive as optional `archive_*` metadata for manual downloads without breaking older policies or clients.
- Implement the Linux update handoff by marking the verified AppImage executable and opening its containing directory with `xdg-open` or `gio open`; do not replace the running application or request root privileges.
- Enable the NLE section on Linux for DaVinci Resolve/OpenFX only, including Linux asset selection, standard OFX installation, `pkexec` privilege escalation, manual fallback guidance, version detection, and Resolve Utility script installation.
- Update `gyroflow-plugins` so Linux publishes a versioned `GyroflowNiyien-OpenFX-linux.zip` contract and does not build or advertise an Adobe Linux artifact.
- Update the deployed `docs` manifest implementation together with the main-repository template and verify that their Linux package behavior remains aligned.
- Keep NeuFlow/AI Sync, Linux GPU zero-copy optimization, Linux ARM64, Adobe-on-Linux, and Final Cut Pro outside this change.

## Capabilities

### New Capabilities

- `linux-desktop-build`: Defines the supported Linux x86_64 development commands, deterministic package outputs, archive integrity checks, and explicit rejection of unsupported architectures.

### Modified Capabilities

- `release-automation`: Restores Linux in CI and requires independent AppImage and tar.gz assets across release, nightly, 123, release-center, and artifact-source flows.
- `app-update-installation`: Adds Linux AppImage manifest semantics, optional tar archive metadata, executable permission preparation, and open-containing-directory handoff.
- `nle-plugin-distribution`: Extends the plugin asset and installation contract with Linux x86_64 OpenFX/DaVinci Resolve while keeping Adobe hidden and unsupported on Linux.

## Impact

- Main repository: `.github/workflows/release.yml`, `Justfile`, `_scripts/linux.just`, `_scripts/publish_pan123_release.py`, release-center backend/UI, `api/_distribution.js`, `src/distribution.rs`, `src/nle_plugins.rs`, controller/QML platform gates, translations, tests, and distribution documentation.
- Production web repository: `C:/Users/Jhe/Desktop/github/docs/api/_control-plane.js` and its manifest tests.
- Plugin repository: `C:/Users/Jhe/Desktop/github/gyroflow-plugins/Justfile`, OpenFX packaging, plugin release workflow, and packaging tests.
- Public contract additions: Linux tar asset, Linux nightly artifact names, `packages.linux.archive_url`, `archive_sha256`, and `archive_size`, plus the Linux OpenFX release asset.
- Runtime dependencies: Linux desktop openers (`xdg-open` with `gio open` fallback) and optional PolicyKit `pkexec` for system-wide OFX installation.
