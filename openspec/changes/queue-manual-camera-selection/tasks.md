## 1. Core camera catalog and resolver

- [x] 1.1 Add failing unit tests for queue eligibility based only on non-empty video-job raw brand+model pairs, including empty and mixed queues
- [x] 1.2 Add failing unit tests for active camera_db catalog enumeration, zero-valued readout availability, and disabled models without numeric readout data
- [x] 1.3 Implement a focused core manual-camera catalog/resolver that loads `get_camera_db_path()` and exposes brands, models, availability, and selection validation
- [x] 1.4 Add failing unit tests for per-job crop/unit-pixel-focal-length/camera-matrix resolution, special brand geometry, invalid geometry, and no-lens-index behavior
- [x] 1.5 Implement per-job camera geometry resolution using camera_db model/crop data and existing brand-specific semantics
- [x] 1.6 Add failing unit tests for camera_db readout priority, zero readout, nearest-standard-FPS half-frame fallback, tie-breaking, and direction preservation/defaulting
- [x] 1.7 Implement per-job readout resolution and invisible half-frame fallback

## 2. Persistence and render-queue integration

- [x] 2.1 Add failing tests for long-term global brand/model settings that remain stored while ineligible or invalid
- [x] 2.2 Implement independent persisted manual camera brand/model settings without changing `lens_group_configs_v1` or `lens_group_manual_edit`
- [x] 2.3 Add failing tests proving effective manual metadata never mutates raw detection metadata and cannot self-disable eligibility
- [x] 2.4 Extend render-queue job baseline/effective metadata handling to overlay manual camera geometry and readout only while the whole queue is eligible
- [x] 2.5 Add failing integration tests for per-job resolution/fps/lens-group focal inputs, missing lens_index, and clean removal of stale overrides when eligibility or selection changes
- [x] 2.6 Wire brand/model, queue membership, Lens Group changes, rematching, and active lens-data updates to reapply affected jobs from their clean baselines
- [x] 2.7 Ensure successful matrix/readout results persist in job project state and queue Play consumes the same state without affecting direct raw-video loading

## 3. Missing-data gate behavior

- [x] 3.1 Add failing tests showing valid manual geometry satisfies only the existing sensor condition
- [x] 3.2 Update the batch missing-data gate to accept valid effective manual geometry while retaining focal, lens-index, invalid-selection, and mixed-queue blocks
- [x] 3.3 Add regression tests proving half-frame readout alone cannot satisfy missing focal or sensor geometry

## 4. Lens Group UI

- [x] 4.1 Add failing UI/contract tests for strict non-empty all-video-incomplete visibility and independence from Manual edit
- [x] 4.2 Expose queue eligibility and active camera catalog/selection APIs to QML
- [x] 4.3 Add only the Camera brand and Camera model selectors to `LensGroupConfig.qml`, with dependent model refresh and disabled unavailable models
- [x] 4.4 Add regression tests for hidden-but-preserved selection, restored eligibility, missing saved model, and absence of a visible switch/readout/fallback/status UI

## 5. Verification and documentation

- [x] 5.1 Run focused core and Lens Group/render-queue tests and resolve all failures
- [x] 5.2 Run `just check` plus relevant translation/QML validation required by the repository
- [x] 5.3 Perform one concentrated diff review for raw/effective metadata separation, camera_db rule reuse, stale-state cleanup, and scope containment
- [x] 5.4 Re-run `openspec validate queue-manual-camera-selection --strict` and mark every completed task
