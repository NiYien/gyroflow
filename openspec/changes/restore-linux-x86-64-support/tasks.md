## 1. Plugin producer contract (`gyroflow-plugins`)

- [x] 1.1 Add a failing static/package test proving the Linux job builds OpenFX but does not invoke or upload Adobe
- [x] 1.2 Add a failing ZIP-contract test for the Linux x86_64 binary, three Resolve sidecars, license metadata, and explicit version file
- [x] 1.3 Update the root Justfile so Linux deploy runs OpenFX and existing frei0r only while Windows/macOS behavior remains unchanged
- [x] 1.4 Add the explicit Linux version file to `GyroflowNiyien-OpenFX-linux.zip` and make the packaging check fail when any required member is missing
- [x] 1.5 Update the plugin release workflow to upload the Linux OpenFX artifact without a Linux Adobe artifact
- [ ] 1.6 Run the focused plugin contract tests and a Linux container deploy; inspect the produced ZIP and dynamic dependencies

## 2. Linux application build contract (`gyroflow`)

- [x] 2.1 Add failing release-automation/static tests for Linux x86_64-only architecture handling and the complete Linux Just recipe surface
- [x] 2.2 Make `_scripts/linux.just` reject non-x86_64 overrides before dependency installation or compilation
- [x] 2.3 Implement Linux `build`, `build-debug`, `test-core`, `clippy`, `profile`, and `bundle` recipes with root-Justfile-compatible parameter forwarding
- [x] 2.4 Add failing package-producer tests for both exact Linux filenames, non-empty output, required archive members, and executable payload
- [x] 2.5 Add explicit dependency/tool probes and final AppImage/tar validation to the Linux install/deploy recipes
- [x] 2.6 Run Linux recipe listing/static checks and the focused producer tests

## 3. Application CI and release artifacts (`gyroflow`)

- [x] 3.1 Add failing workflow tests for the active Linux matrix row, two tag uploads, two nightly uploads, and two GitHub Release files
- [x] 3.2 Restore the Linux x86_64 matrix job and retain the `just deploy docker` build entry
- [x] 3.3 Add raw tag uploads for `gyroflow-niyien-linux64.AppImage` and `gyroflow-niyien-linux64.tar.gz`
- [x] 3.4 Add independent nightly artifacts `gyroflow-niyien-linux-appimage` and `gyroflow-niyien-linux-tar`
- [x] 3.5 Attach both raw Linux files to GitHub Release and make missing files fail visibly
- [x] 3.6 Add Linux artifact smoke steps for tar/AppImage extraction, packaged `--version`, and unresolved dynamic libraries
- [x] 3.7 Run the focused workflow contract tests

## 4. Publisher and release-center Linux asset model (`gyroflow`)

- [x] 4.1 Add failing Python tests that require both Linux application assets when the matrix is active and require neither when Linux is paused
- [x] 4.2 Add failing artifact-source tests mapping the two Linux short artifact names to their exact raw files with cache reuse
- [x] 4.3 Extend app asset, platform, role, artifact, and remote-name maps with the Linux tar and independent nightly artifacts
- [x] 4.4 Add failing metadata tests for Linux `package_*` AppImage fields and `archive_*` tar fields
- [x] 4.5 Emit both Linux metadata sets, artifact URLs, 123 uploads, pre-warms, and finalize-summary values
- [ ] 4.6 Extend release-center policy normalization, inventory, version details, and manifest preview to preserve/display `archive_*`
- [ ] 4.7 Run publisher, release-center backend, and release-automation Rust wrapper tests

## 5. Template and production manifest (`gyroflow` + `docs`)

- [ ] 5.1 Add failing Node tests in `gyroflow` for Linux `kind=appimage`, AppImage `package_*`, tar `archive_*`, absolute URLs, and legacy-policy fallback
- [ ] 5.2 Extend `gyroflow/api/_distribution.js` normalization and release/artifact/CN URL resolution with optional Linux archive fields and an `appimage` default
- [ ] 5.3 Add the equivalent failing production-manifest tests in `docs`
- [ ] 5.4 Mirror the normalized Linux package/archive behavior in `docs/api/_control-plane.js` without changing other platform routes
- [ ] 5.5 Run both repositories' Node manifest suites and compare normalized Linux output shapes

## 6. Linux application update handoff (`gyroflow`)

- [ ] 6.1 Add failing Rust tests for Linux package kind, default AppImage filename, wrapper extraction name, and selection of AppImage over tar metadata
- [ ] 6.2 Implement Linux-specific defaults in manifest/update selection and remove every Linux-to-DMG fallback
- [ ] 6.3 Add failing Unix permission tests proving preparation adds only the owner executable bit required by the cached AppImage
- [ ] 6.4 Mark the verified Linux AppImage executable after download while preserving existing cache and integrity behavior
- [ ] 6.5 Add failing command-selection tests for `xdg-open`, `gio open` fallback, both-fail absolute-path error, and no automatic quit
- [ ] 6.6 Implement Linux open-containing-directory handoff without self-replacement, root escalation, or application exit
- [ ] 6.7 Add Linux ready-state QML instructions/action text and update all translation catalogs using English source comments/strings
- [ ] 6.8 Run focused app-update Rust tests and translation/static UI checks

## 7. Linux Resolve/OpenFX client integration (`gyroflow`)

- [ ] 7.1 Add failing platform-parameterized tests for Linux OpenFX availability, Linux asset name, standard install path, Resolve detection paths, and hidden Adobe
- [ ] 7.2 Compile the NLE module on Linux and expose only the OpenFX entry through controller and QML platform gates
- [ ] 7.3 Select `GyroflowNiyien-OpenFX-linux.zip`, `/usr/OFX/Plugins/GyroflowNiyien.ofx.bundle`, `/opt/resolve`, and `Contents/Linux-x86-64/GyroflowNiyien.ofx` without macOS fallthrough
- [ ] 7.4 Add failing version-detection tests for a complete Linux bundle and incomplete binary/version-file cases
- [ ] 7.5 Read the explicit Linux bundle version file during detect and return empty for incomplete bundles
- [ ] 7.6 Add failing Linux Resolve Utility path/copy tests for `~/.local/share/DaVinciResolve/Fusion/Scripts/Utility`, exact sidecars, legacy removal, and unrelated-file preservation
- [ ] 7.7 Implement Linux Resolve sidecar installation without privilege escalation
- [ ] 7.8 Add failing privilege-path tests for direct copy, canonical source validation, fixed destination, shell-free `pkexec`, and preserved manual fallback
- [ ] 7.9 Implement Linux direct/pkexec OFX copy with a typed/sentinel manual-fallback result and no untrusted shell command construction
- [ ] 7.10 Add Linux install/update/manual-fallback QML handling and translations while preserving Windows/macOS behavior
- [ ] 7.11 Run focused `nle_plugins` Rust tests, QML static tests, and main-repository plugin asset contract tests

## 8. Documentation and cross-repository verification

- [ ] 8.1 Update deployment/build documentation with Linux x86_64 scope, both application formats, update behavior, Resolve paths, driver-dependent hardware encoding, and explicit exclusions
- [ ] 8.2 Run the full relevant `gyroflow` Rust, Python, Node, release-center, workflow, and formatting checks
- [ ] 8.3 Run the relevant `docs` manifest/API tests and formatting checks
- [ ] 8.4 Run the relevant `gyroflow-plugins` Cargo, packaging, workflow, and formatting checks
- [ ] 8.5 Run `just check`, tests, and `just deploy docker` in Linux x86_64; inspect both application packages and execute the packaged `--version`
- [ ] 8.6 Validate `restore-linux-x86-64-support` with OpenSpec strict validation and run whitespace/diff checks in all three repositories
- [ ] 8.7 On a real Linux x86_64 workstation, launch both package formats, exercise a representative software/GPU export, and confirm updater folder handoff
- [ ] 8.8 On a real Linux x86_64 DaVinci Resolve installation, install/update OpenFX, restart Resolve, verify the effect version, and confirm exactly two Gyroflow NiYien Utility menu entries
