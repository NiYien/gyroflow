## ADDED Requirements

### Requirement: Release workflow publishes both Linux x86_64 application packages

When Linux is active in the release matrix, the workflow SHALL build exactly one Linux x86_64 target and publish both `gyroflow-niyien-linux64.AppImage` and `gyroflow-niyien-linux64.tar.gz`. Tag builds SHALL upload raw assets and attach both to GitHub Release. Workflow-dispatch builds SHALL publish two independent single-file V4 artifacts named `gyroflow-niyien-linux-appimage` and `gyroflow-niyien-linux-tar`.

#### Scenario: Linux tag release contains both raw files
- **WHEN** a tag-triggered workflow completes its Linux matrix job
- **THEN** release artifacts SHALL contain the raw AppImage and tar.gz files
- **AND** the GitHub Release files list SHALL include both exact filenames

#### Scenario: Linux nightly uses independent artifacts
- **WHEN** a workflow-dispatch Linux build succeeds
- **THEN** artifact `gyroflow-niyien-linux-appimage` SHALL wrap only `gyroflow-niyien-linux64.AppImage`
- **AND** artifact `gyroflow-niyien-linux-tar` SHALL wrap only `gyroflow-niyien-linux64.tar.gz`

### Requirement: Linux publish metadata records primary and archive packages

For every publish containing Linux, the publisher SHALL compute SHA-256 and size for both Linux files. `packages.linux.package_*` SHALL describe the AppImage and `packages.linux.archive_*` SHALL describe the tar.gz. Both files SHALL be uploaded, inventoried, and pre-warmed through the existing 123 release flow.

#### Scenario: Release summary records both Linux files
- **WHEN** app publishing completes with both Linux assets
- **THEN** `packages.linux.kind` SHALL equal `appimage`
- **AND** `package_filename`, `package_sha256`, and `package_size` SHALL describe the AppImage
- **AND** `archive_filename`, `archive_sha256`, and `archive_size` SHALL describe the tar.gz

#### Scenario: Missing tar blocks Linux publish
- **WHEN** Linux is active and the AppImage exists but the tar.gz is missing
- **THEN** release and artifact source modes SHALL reject the candidate as incomplete
- **AND** publish-and-push SHALL NOT promote the version

## MODIFIED Requirements

### Requirement: Artifact app URLs use a per-platform packages structure

Artifact-mode app URL output SHALL accommodate platforms that ship multiple assets. `build_global_artifact_app_urls` and the policy entry's `app_urls` field SHALL output a per-platform object rather than a single URL per platform.

The structure SHALL support:

- `app_urls.<platform>.installer_url` for a separate installer such as Windows setup.
- `app_urls.<platform>.package_url` for the primary application package.
- `app_urls.<platform>.archive_url` for an optional alternate archive such as the Linux tar.gz.

Each workflow artifact name SHALL map to one raw deliverable. V4 nightly wrappers SHALL be downloaded and unwrapped to their mapped raw filenames before hash, size, upload, or cache validation. Tag-release `archive:false` artifacts SHALL remain raw bytes. Consumers in the docs manifest and release center SHALL accept the object form and SHALL continue accepting a legacy single-URL value as `package_url` with no installer or archive URL.

#### Scenario: Windows artifact URLs include installer and package
- **WHEN** artifact mode resolves a Windows-capable run
- **THEN** `app_urls.windows.installer_url` SHALL resolve to the Windows setup
- **AND** `app_urls.windows.package_url` SHALL resolve to the Windows zip

#### Scenario: Linux artifact URLs include AppImage and tar archive
- **WHEN** artifact mode resolves a Linux-capable run with both independent nightly artifacts
- **THEN** `app_urls.linux.package_url` SHALL resolve to the raw AppImage under the app download route
- **AND** `app_urls.linux.archive_url` SHALL resolve to the raw tar.gz under the app download route
- **AND** both stored URLs SHALL expand to absolute manifest URLs

#### Scenario: Legacy single URL remains compatible
- **WHEN** a consumer reads `app_urls.<platform>` as a string
- **THEN** it SHALL treat the string as `package_url`
- **AND** SHALL leave installer and archive URLs empty

### Requirement: Required app assets are a named subset of all known assets

`_scripts/publish_pan123_release.py` SHALL distinguish two concepts:

- `APP_ASSET_NAMES` is the superset of all known app asset filenames across all supported platforms.
- `REQUIRED_APP_ASSET_NAMES` is the subset that SHALL be present for a publish to be complete and SHALL be derived from the active `release.yml` matrix or an equivalent explicit config.

When Linux is active, both `gyroflow-niyien-linux64.AppImage` and `gyroflow-niyien-linux64.tar.gz` SHALL enter the required subset. The release-center 123 inventory SHALL derive its expected remote names from that subset, report each missing file, and prevent an incomplete version from being promoted. When Linux is removed from the matrix, neither Linux file SHALL remain required; either file MAY still be reported as optional/extra.

#### Scenario: Active Linux requires both assets
- **WHEN** the workflow matrix contains `type: linux`
- **THEN** `REQUIRED_APP_ASSET_NAMES` SHALL include the AppImage and tar.gz
- **AND** the release center SHALL require their corresponding 123 remote filenames

#### Scenario: One of two Linux assets is missing
- **WHEN** Linux is active and inventory contains only one Linux application asset
- **THEN** inventory SHALL mark the version incomplete
- **AND** SHALL name the other Linux asset as missing

#### Scenario: Pausing Linux removes both requirements
- **WHEN** the Linux matrix row is removed
- **THEN** neither Linux asset SHALL block Windows, macOS, or Android publication
