## ADDED Requirements

### Requirement: Linux manifest exposes primary AppImage and alternate tar metadata

For Linux clients, the distribution manifest SHALL expose `app.packages.linux` with `kind = "appimage"`. `package_url`, `package_sha256`, and `package_size` SHALL describe `gyroflow-niyien-linux64.AppImage`. When tar metadata is present in policy, `archive_url`, `archive_sha256`, and `archive_size` SHALL describe `gyroflow-niyien-linux64.tar.gz`.

`app.url` and every Linux manual-version `url` SHALL equal the resolved AppImage `package_url`. All Linux URLs SHALL be absolute for release, artifact, and CN routing. Missing `archive_*` fields in an older policy SHALL NOT prevent manifest generation or AppImage updates.

#### Scenario: Linux release manifest returns both formats
- **WHEN** a Linux client requests a release-mode manifest whose policy contains both Linux assets
- **THEN** `app.packages.linux.kind` SHALL equal `appimage`
- **AND** `package_url` SHALL end in `gyroflow-niyien-linux64.AppImage`
- **AND** `archive_url` SHALL end in `gyroflow-niyien-linux64.tar.gz`
- **AND** both files SHALL include their own SHA-256 and size metadata

#### Scenario: Linux artifact manifest returns independent URLs
- **WHEN** a Linux client requests an artifact-mode manifest
- **THEN** the package URL SHALL resolve from `app_urls.linux.package_url`
- **AND** the archive URL SHALL resolve from `app_urls.linux.archive_url`
- **AND** both SHALL be absolute

#### Scenario: Legacy Linux policy remains usable
- **WHEN** a policy entry contains only Linux `package_*` metadata and no `archive_*` fields
- **THEN** the manifest SHALL return a usable AppImage package
- **AND** SHALL omit or empty the archive fields without error

### Requirement: Linux client selects the AppImage update package

The client SHALL normalize Linux as a first-class update platform, default its package kind to `appimage`, and default its cache filename to `gyroflow-niyien-linux64.AppImage`. It SHALL NOT use the macOS DMG kind or filename as a Linux fallback. The optional tar archive SHALL remain a manual-download alternative and SHALL NOT be selected by the in-app update flow.

#### Scenario: Linux package metadata selects AppImage
- **WHEN** the manifest contains `packages.linux` with AppImage and tar metadata
- **THEN** the update selection SHALL use the AppImage URL, SHA-256, and size
- **AND** SHALL ignore `archive_*` for the prepared in-app update

#### Scenario: Linux wrapper uses AppImage fallback filename
- **WHEN** a Linux AppImage is downloaded through a nightly wrapper URL
- **THEN** the extracted cache file SHALL be named `gyroflow-niyien-linux64.AppImage`
- **AND** SHALL be verified against the raw inner AppImage SHA-256

### Requirement: Linux update handoff opens the containing directory

After the Linux AppImage has downloaded and passed integrity verification, the client SHALL add user executable permission bits to the cached file. When the user invokes the ready action, the client SHALL open the cached file's parent directory with `xdg-open`, falling back to `gio open` when the first command is unavailable or fails. Successful handoff SHALL leave Gyroflow running and SHALL NOT replace the current executable, request root privileges, or delete the prepared update.

#### Scenario: xdg-open handoff succeeds
- **WHEN** a verified Linux AppImage is ready and `xdg-open` successfully opens its parent directory
- **THEN** the client SHALL report handoff success
- **AND** Gyroflow SHALL remain running

#### Scenario: gio fallback succeeds
- **WHEN** `xdg-open` is unavailable or returns failure
- **AND** `gio open` succeeds for the parent directory
- **THEN** the client SHALL report handoff success without launching a second download

#### Scenario: Both openers fail
- **WHEN** both Linux directory openers are unavailable or fail
- **THEN** the update UI SHALL display a readable error containing the absolute cached AppImage path
- **AND** SHALL keep the verified file available for manual access

#### Scenario: Prepared AppImage is executable
- **WHEN** Linux finishes preparing a verified AppImage
- **THEN** the owner executable bit SHALL be set on the cached file
- **AND** existing read/write permission bits SHALL not be broadened beyond what is needed to execute it

### Requirement: Linux update UI explains manual replacement

The ready-state Linux update UI SHALL tell the user that the containing directory will open and that the user must exit Gyroflow, replace the previous AppImage, and start the new file. The action label SHALL describe opening the folder and SHALL NOT claim that Gyroflow will install or replace itself.

#### Scenario: Ready dialog shows Linux instructions
- **WHEN** a Linux AppImage reaches the ready state
- **THEN** the dialog SHALL show localized manual replacement instructions
- **AND** the primary action SHALL be labeled as opening the containing folder
- **AND** the dialog SHALL NOT show the macOS drag-to-Applications text

#### Scenario: Linux handoff does not trigger quit confirmation
- **WHEN** the Linux directory opener succeeds
- **THEN** the client SHALL NOT set the update handoff state that automatically quits the application
