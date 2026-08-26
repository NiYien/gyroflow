## ADDED Requirements

### Requirement: Linux desktop build supports x86_64 only

The Linux desktop build SHALL accept x86_64 as its only supported architecture and SHALL reject ARM64 or any other architecture before downloading dependencies or compiling application code. The build scripts, CI matrix, package names, and documentation SHALL NOT advertise a Linux architecture for which this change does not produce and validate a release artifact.

#### Scenario: Default Linux build selects x86_64
- **WHEN** a developer or CI job invokes a Linux build without an architecture override
- **THEN** the build SHALL target Linux x86_64
- **AND** all generated package names SHALL use the `linux64` suffix

#### Scenario: ARM64 override fails before build
- **WHEN** `FORCE_ARCH=aarch64` or an equivalent unsupported Linux architecture is requested
- **THEN** the Linux recipe SHALL exit non-zero with an explicit unsupported-architecture message
- **AND** SHALL NOT produce an ARM64-named package

### Requirement: Linux implements the shared desktop Just command surface

Every root Justfile command that dispatches to the platform file for desktop development SHALL resolve to a Linux recipe with matching argument forwarding. At minimum Linux SHALL implement `run`, `test`, `test-core`, `check`, `build`, `build-debug`, `debug`, `profile`, `clippy`, `install-deps`, `deploy`, and `bundle` where those commands are exposed by the root Justfile.

#### Scenario: Root build commands resolve on Linux
- **WHEN** a Linux developer invokes `just build`, `just build-debug`, `just test-core`, `just clippy`, or `just profile`
- **THEN** Just SHALL find the corresponding recipe in `_scripts/linux.just`
- **AND** SHALL forward caller parameters to the underlying Cargo command

#### Scenario: Bundle command uses Linux package outputs
- **WHEN** a Linux developer invokes `just bundle`
- **THEN** the recipe SHALL create or validate the Linux distribution bundle rather than failing with an unknown recipe

### Requirement: Linux deploy produces two validated application artifacts

A successful Linux x86_64 deploy SHALL create both `gyroflow-niyien-linux64.AppImage` and `gyroflow-niyien-linux64.tar.gz` in `_deployment/_binaries/`. The deploy recipe SHALL validate both files before returning success, including non-zero size, expected archive members, executable application payload, and required bundled runtime libraries and data.

#### Scenario: Successful deploy creates both artifacts
- **WHEN** `just deploy docker` completes successfully on Linux x86_64
- **THEN** `_deployment/_binaries/gyroflow-niyien-linux64.AppImage` SHALL exist and be non-empty
- **AND** `_deployment/_binaries/gyroflow-niyien-linux64.tar.gz` SHALL exist and be non-empty

#### Scenario: Missing package fails at producer
- **WHEN** the packaging step does not create either required Linux artifact
- **THEN** `just deploy` SHALL exit non-zero before the workflow upload step
- **AND** the error SHALL name the missing artifact

#### Scenario: Packaged program reports its version
- **WHEN** CI extracts the tar archive or AppImage and invokes the packaged binary with `--version`
- **THEN** the command SHALL exit successfully
- **AND** SHALL print the Gyroflow(NiYien) version

### Requirement: Linux packaging probes its compatibility toolchain

The existing Linux compatibility baseline MAY remain on the current container and AppImage recipe during this change, but every externally invoked build tool and every downloaded Qt, FFmpeg, SDK, and AppImage input consumed by packaging SHALL be checked at the point of use. Missing or incomplete inputs SHALL fail with a diagnostic that identifies the expected file or tool.

#### Scenario: Incomplete dependency fails immediately
- **WHEN** a cached Qt, FFmpeg, SDK, or AppImage tool directory exists but a file consumed by deploy is absent
- **THEN** the dependency or deploy step SHALL exit non-zero at its explicit probe
- **AND** SHALL NOT continue to an unrelated copy or upload failure

#### Scenario: Dynamic library smoke check finds no unresolved bundled dependency
- **WHEN** CI inspects the extracted Linux application and OpenFX binaries with the configured runtime library path
- **THEN** the dynamic dependency report SHALL contain no required library marked `not found`
