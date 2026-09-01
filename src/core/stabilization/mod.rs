// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc;

use crate::GyroflowCoreError;

use super::StabilizationManager;
use super::gpu::*;
use drawing::DrawCanvas;

mod compute_params;
mod cpu_undistort;
mod frame_transform;
mod pixel_formats;
// mod interpolation;
pub mod distortion_models;
pub use compute_params::ComputeParams;
pub use cpu_undistort::*;
pub use frame_transform::FrameTransform;
pub use pixel_formats::*;

#[derive(Default, Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum Interpolation {
    #[default]
    Bilinear = 2,
    Bicubic = 4,
    Lanczos4 = 8,
    RobidouxSharp = 10,
    Robidoux = 11,
    Mitchell = 12,
    CatmullRom = 13,
}
impl From<&str> for Interpolation {
    fn from(s: &str) -> Self {
        match s {
            "Bilinear" => Interpolation::Bilinear,
            "Bicubic" => Interpolation::Bicubic,
            "Lanczos4" => Interpolation::Lanczos4,
            "EWA: RobidouxSharp" => Interpolation::RobidouxSharp,
            "EWA: Robidoux" => Interpolation::Robidoux,
            "EWA: Mitchell" => Interpolation::Mitchell,
            "EWA: Catmull-Rom" => Interpolation::CatmullRom,
            _ => Interpolation::Lanczos4,
        }
    }
}

type WgpuCache = lru::LruCache<u32, wgpu::WgpuWrapper>;

lazy_static::lazy_static! {
    // Keep GPU destruction off Qt-owned render threads without creating a
    // Rust thread from a TLS destructor. `std::thread::spawn` is not valid
    // after Rust's own thread-local `Thread` has already been destroyed.
    static ref WGPU_DROP_QUEUE: mpsc::Sender<WgpuCache> = {
        let (tx, rx) = mpsc::channel::<WgpuCache>();
        std::thread::Builder::new()
            .name("gyroflow-wgpu-drop".into())
            .spawn(move || {
                while let Ok(cache) = rx.recv() {
                    drop(cache);
                }
            })
            .expect("failed to start wgpu drop worker");
        tx
    };
}

fn queue_wgpu_drop(cache: WgpuCache) {
    if let Err(err) = WGPU_DROP_QUEUE.send(cache) {
        // Never fall back to destroying Vulkan objects on the calling thread.
        std::mem::forget(err.0);
    }
}

struct ThreadLocalWgpuCache(RefCell<WgpuCache>);
impl ThreadLocalWgpuCache {
    fn new() -> Self {
        // Initialize the worker while Rust's thread metadata is still valid,
        // rather than on first access from this type's TLS destructor.
        lazy_static::initialize(&WGPU_DROP_QUEUE);
        Self(RefCell::new(WgpuCache::new(
            std::num::NonZeroUsize::new(15).unwrap(),
        )))
    }
}
impl Drop for ThreadLocalWgpuCache {
    fn drop(&mut self) {
        // Workaround for a Vulkan hang on device destroy (https://github.com/gfx-rs/wgpu/issues/4973)
        let inner = self
            .0
            .replace(WgpuCache::new(std::num::NonZeroUsize::new(1).unwrap()));
        queue_wgpu_drop(inner);
    }
}

lazy_static::lazy_static! {
    pub static ref GPU_LIST: parking_lot::RwLock<Vec<String>> = parking_lot::RwLock::new(Vec::new());
}
thread_local! {
    static CACHED_WGPU: ThreadLocalWgpuCache = ThreadLocalWgpuCache::new();
    #[cfg(feature = "use-opencl")]
    static CACHED_OPENCL: RefCell<lru::LruCache<u32, opencl::OclWrapper>> = RefCell::new(lru::LruCache::new(std::num::NonZeroUsize::new(15).unwrap()));
}

// §8b cross-thread cache clear: thread_local!s can only be touched from the
// thread that owns them, so the controller's onProcessTexture closure must
// call this function (from within the render thread) when it observes a
// gpu_epoch change.
pub fn clear_caller_thread_gpu_caches() {
    #[cfg(feature = "use-opencl")]
    CACHED_OPENCL.with(|x| x.borrow_mut().clear());
    CACHED_WGPU.with(|x| {
        let inner = x
            .0
            .replace(WgpuCache::new(std::num::NonZeroUsize::new(15).unwrap()));
        queue_wgpu_drop(inner);
    });
}

bitflags::bitflags! {
    #[derive(Default, Clone)]
    pub struct KernelParamsFlags: i32 {
        const FIX_COLOR_RANGE      = 1 << 0; // 1
        const HAS_DIGITAL_LENS     = 1 << 1; // 2
        const FILL_WITH_BACKGROUND = 1 << 2; // 4
        const DRAWING_ENABLED      = 1 << 3; // 8
        const HORIZONTAL_RS        = 1 << 4; // 16, right-to-left or left-to-right rolling shutter
        const HAS_SOURCE_RECT      = 1 << 5; // 32
        const HAS_OUTPUT_RECT      = 1 << 6; // 64
        const FRAMEBUFFER_INVERTED = 1 << 7; // 128
        const HAS_IBIS_DATA        = 1 << 8; // 256
        const HAS_MESH_DATA        = 1 << 9; // 512
        const HAS_FPD_DATA         = 1 << 10; // 1024
        const ANY_UNDERWATER       = 1 << 11; // 2048
        // openfx-output-adjust-flip: per-pass horizontal/vertical mirror toggles, applied
        // at the very top of `undistort_coord` before any other transform. Identity (both
        // bits clear) is byte-equivalent to the prior kernel.
        const FLIP_H               = 1 << 12; // 4096
        const FLIP_V               = 1 << 13; // 8192
    }
}

// Each parameter must be aligned to 4 bytes and whole struct to 16 bytes
// Must be kept in sync with: opencl_undistort.cl, wgpu_undistort.wgsl and qt_gpu/undistort.frag
#[repr(C, packed(4))]
#[derive(Copy, Clone)]
pub struct KernelParams {
    pub width: i32,                                         // 4
    pub height: i32,                                        // 8
    pub stride: i32,                                        // 12
    pub output_width: i32,                                  // 16
    pub output_height: i32,                                 // 4
    pub output_stride: i32,                                 // 8
    pub matrix_count: i32, // 12 - for rolling shutter correction. 1 = no correction, only main matrix
    pub interpolation: i32, // 16
    pub background_mode: i32, // 4
    pub flags: i32,        // 8
    pub bytes_per_pixel: i32, // 12
    pub pix_element_count: i32, // 16
    pub background: [f32; 4], // 16
    pub f: [f32; 2],       // 8  - focal length in pixels
    pub c: [f32; 2],       // 16 - lens center
    pub k: [f32; 12],      // 16,16,16 - distortion coefficients
    pub fov: f32,          // 4
    pub r_limit: f32,      // 8
    pub lens_correction_amount: f32, // 12
    pub input_vertical_stretch: f32, // 16
    pub input_horizontal_stretch: f32, // 4
    pub background_margin: f32, // 8
    pub background_margin_feather: f32, // 12
    pub canvas_scale: f32, // 16
    pub input_rotation: f32, // 4
    pub output_rotation: f32, // 8
    pub translation2d: [f32; 2], // 16
    pub translation3d: [f32; 4], // 16
    pub source_rect: [i32; 4], // 16 - x, y, w, h
    pub output_rect: [i32; 4], // 16 - x, y, w, h
    pub digital_lens_params: [f32; 4], // 16
    pub safe_area_rect: [f32; 4], // 16
    pub max_pixel_value: f32, // 4
    pub distortion_model: stabilize_spirv::DistortionModel, // 8
    pub digital_lens: stabilize_spirv::DistortionModel, // 12
    pub pixel_value_limit: f32, // 16
    pub light_refraction_coefficient: f32, // 4
    pub plane_index: i32,  // 8
    pub reserved1: f32,    // 12
    pub reserved2: f32,    // 16
    pub ewa_coeffs_p: [f32; 4], // 16
    pub ewa_coeffs_q: [f32; 4], // 16
    // openfx-output-adjust-affine: appended at end to preserve packed(4) offsets
    // of all prior fields across the four backend struct definitions.
    pub post_rotation: f32, // 4
    pub post_zoom: f32,     // 8
    pub post_offset: [f32; 2], // 16
}
unsafe impl bytemuck::Zeroable for KernelParams {}
unsafe impl bytemuck::Pod for KernelParams {}
impl Default for KernelParams {
    fn default() -> Self {
        // Zeroed for all prior fields preserves previous derived-Default behavior;
        // post_zoom must be 1.0 so the openfx-output-adjust-affine shader block
        // is identity when no PostAffine is supplied.
        let mut p: Self = bytemuck::Zeroable::zeroed();
        p.post_zoom = 1.0;
        p
    }
}

#[derive(Default, Debug)]
pub enum BackendType {
    #[default]
    None,
    OpenCL(u32),
    Wgpu(u32),
    Cpu(u32),
}
impl BackendType {
    pub fn get_hash(&self) -> u32 {
        match self {
            BackendType::Cpu(x) => *x,
            BackendType::OpenCL(x) => *x,
            BackendType::Wgpu(x) => *x,
            _ => 0,
        }
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
    pub fn is_wgpu(&self) -> bool {
        matches!(self, Self::Wgpu(_))
    }
    // Stable `&'static str` label for telemetry keys; matches the strings in
    // `stab.timing` log lines so a `grep backend=opencl` works end-to-end.
    pub fn name(&self) -> &'static str {
        match self {
            BackendType::None      => "none",
            BackendType::OpenCL(_) => "opencl",
            BackendType::Wgpu(_)   => "wgpu",
            BackendType::Cpu(_)    => "cpu",
        }
    }
}

// Per-backend accumulator of render timings flushed at most once per
// `GYROFLOW_STAB_TIMING_MS` (default 1 s) by `add_and_maybe_emit`. The
// `stab_data_ms` / `gpu_ms` / `backend_init_ms` axes mirror the three cost
// centers identified in the optimize-stab-load-pipeline change. All adds use
// saturating arithmetic so a long-running session can't overflow `u64`.
#[derive(Default, Copy, Clone)]
pub struct StabTimingAccumulator {
    pub frames: u64,
    pub stab_data_ms: u64,
    pub gpu_ms: u64,
    pub backend_init_ms: u64,
}

fn timing_accumulators() -> &'static parking_lot::Mutex<std::collections::HashMap<&'static str, StabTimingAccumulator>> {
    static MAP: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<&'static str, StabTimingAccumulator>>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// Add the timings to the per-backend accumulator and emit one `stab.timing`
/// line if the throttle allows. Caller is responsible for measuring each
/// component; pass `0` for any axis that does not apply on this call.
pub(crate) fn add_and_maybe_emit(
    backend: &'static str,
    stab_data_ms: u64,
    gpu_ms: u64,
    backend_init_ms: u64,
) {
    let snapshot = {
        let mut map = timing_accumulators().lock();
        let entry = map.entry(backend).or_default();
        entry.frames = entry.frames.saturating_add(1);
        entry.stab_data_ms = entry.stab_data_ms.saturating_add(stab_data_ms);
        entry.gpu_ms = entry.gpu_ms.saturating_add(gpu_ms);
        entry.backend_init_ms = entry.backend_init_ms.saturating_add(backend_init_ms);
        let interval_ms = crate::log_throttle::min_interval_ms_from_env(1000);
        if crate::log_throttle::try_emit(("stab.timing", backend), interval_ms) {
            let out = *entry;
            *entry = StabTimingAccumulator::default();
            Some(out)
        } else {
            None
        }
    };
    if let Some(s) = snapshot {
        log::info!(
            target: "stab.timing",
            "backend={backend} frames={} stab_data_ms={} gpu_ms={} backend_init_ms={}",
            s.frames, s.stab_data_ms, s.gpu_ms, s.backend_init_ms,
        );
    }
}

#[cfg(test)]
pub(crate) fn reset_timing_accumulators_for_test() {
    timing_accumulators().lock().clear();
    crate::log_throttle::reset_for_test();
}

#[derive(Default)]
pub struct Stabilization {
    pub stab_data: BTreeMap<i64, FrameTransform>,

    pub size: (usize, usize),        // width, height
    pub output_size: (usize, usize), // width, height

    pub interpolation: Interpolation,
    pub kernel_flags: KernelParamsFlags,

    #[cfg(feature = "use-opencl")]
    cl: Option<opencl::OclWrapper>,

    pub wgpu: Option<wgpu::WgpuWrapper>,

    pub initialized_backend: BackendType,

    compute_params: ComputeParams,

    pub drawing: DrawCanvas,
    pub pending_device_change: Option<isize>,

    pub share_wgpu_instances: bool,
    pub cache_frame_transform: bool,
    next_backend: Option<&'static str>,

    // GPU wrappers moved out by `init_size()` (which can run on the QML/main
    // thread) and deferred for Drop on the device-owning render thread inside
    // `update_device`. Releasing a D3D11-shared OpenCL `Mem` re-enters the
    // D3D11 device critical section; doing that off the render thread races
    // the Qt scene-graph render thread and hard-deadlocks the driver (dump
    // 2026-06-04). Draining here keeps the teardown serialized with rendering.
    pending_gpu_drop: Vec<TakenGpuBindings>,

    // Segmented snapshot of the backend cache key from the last (re-)init,
    // kept only for the `backend_rebuild` diagnostic in `init_backends`.
    // Segments decompose everything `get_current_key` hashes so a rebuild
    // log can name the exact component that changed (rect values vs thread
    // vs sizes ...) instead of an opaque checksum mismatch.
    last_backend_key_diag: Option<Vec<(&'static str, String)>>,
}

#[derive(Debug)]
pub struct ProcessedInfo {
    pub fov: f64,
    pub minimal_fov: f64,
    pub focal_length: Option<f64>,
    pub backend: &'static str,
}

/// Moved-out GPU wrappers from a Stabilization invalidation. Held briefly
/// by a background worker (see `StabilizationManager::request_gpu_invalidation`)
/// while the render-thread grace period elapses, then dropped — so the
/// OpenCL / wgpu runtime tear-down runs OFF the QML thread and AFTER any
/// in-flight render-thread dispatch has had time to observe the new
/// `gpu_epoch` and bail out via `clear_caller_thread_gpu_caches`.
///
/// Sending these to a worker thread mirrors the pre-existing
/// `ThreadLocalWgpuCache::Drop` workaround for Vulkan device-destroy hangs.
//
// Fields are only consumed via Drop (the struct's sole purpose is deferred
// release), so the borrow-checker can't see them being "read" — silence
// the dead_code lint without disabling it for the whole module.
#[allow(dead_code)]
pub(crate) struct TakenGpuBindings {
    #[cfg(feature = "use-opencl")]
    cl: Option<opencl::OclWrapper>,
    wgpu: Option<wgpu::WgpuWrapper>,
}

impl Stabilization {
    pub fn set_compute_params(&mut self, params: ComputeParams) {
        self.stab_data.clear();
        self.compute_params = params;
    }

    // §8a Layer A — move GPU resource wrappers out of this struct so the
    // caller can hand them to a background worker for deferred Drop.
    // Render threads (which own their own thread_local CACHED_* LRUs)
    // self-clear via the `gpu_epoch` counter on `StabilizationManager`
    // once `gpu_epoch` ticks. Crate-private: external callers must go
    // through `StabilizationManager::request_gpu_invalidation` so the
    // coalescing policy and the deferred-Drop grace window are both
    // applied uniformly.

    /// Move out the OpenCL / wgpu wrappers so the caller can defer their
    /// Drop to a background thread. Non-GPU state (`initialized_backend`,
    /// `next_backend`, `stab_data`) is reset synchronously since it holds
    /// no GPU resources.
    ///
    /// Returns `None` when nothing needed taking (both wrappers were
    /// already `None`), so the caller can skip spawning a worker.
    pub(crate) fn take_gpu_bindings(&mut self) -> Option<TakenGpuBindings> {
        #[cfg(feature = "use-opencl")]
        let cl = self.cl.take();
        let wgpu = self.wgpu.take();
        self.initialized_backend = BackendType::None;
        self.next_backend = None;
        self.stab_data.clear();

        #[cfg(feature = "use-opencl")]
        let cl_taken = cl.is_some();
        #[cfg(not(feature = "use-opencl"))]
        let cl_taken = false;
        let wgpu_taken = wgpu.is_some();

        if cl_taken || wgpu_taken {
            log::info!(
                target: "lifecycle",
                "Stabilization GPU bindings invalidated (cl_taken={} wgpu_taken={})",
                cl_taken,
                wgpu_taken,
            );
            Some(TakenGpuBindings {
                #[cfg(feature = "use-opencl")]
                cl,
                wgpu,
            })
        } else {
            None
        }
    }

    fn get_rect(desc: &BufferDescription) -> [i32; 4] {
        let mut ret = [0i32; 4];
        if let Some(r) = desc.rect {
            ret[0] = r.0 as i32;
            ret[1] = r.1 as i32;
            ret[2] = r.2 as i32;
            ret[3] = r.3 as i32;
        } else {
            // Stretch to the buffer by default
            ret[0] = 0;
            ret[1] = 0;
            ret[2] = desc.size.0 as i32;
            ret[3] = desc.size.1 as i32;
        }
        ret
    }

    pub fn get_kernel_flags(&self, frame: usize, buffers: &Buffers) -> KernelParamsFlags {
        let mut kernel_flags = self.kernel_flags.clone();
        kernel_flags.set(
            KernelParamsFlags::HAS_DIGITAL_LENS,
            self.compute_params.digital_lens.is_some(),
        );
        kernel_flags.set(
            KernelParamsFlags::HORIZONTAL_RS,
            self.compute_params.frame_readout_direction.is_horizontal(),
        );
        kernel_flags.set(
            KernelParamsFlags::HAS_SOURCE_RECT,
            buffers.input.rect.is_some()
                || self.size.0 != buffers.input.size.0
                || self.size.1 != buffers.input.size.1,
        );
        kernel_flags.set(
            KernelParamsFlags::HAS_OUTPUT_RECT,
            buffers.output.rect.is_some()
                || self.output_size.0 != buffers.output.size.0
                || self.output_size.1 != buffers.output.size.1,
        );
        kernel_flags.set(
            KernelParamsFlags::FRAMEBUFFER_INVERTED,
            self.compute_params.framebuffer_inverted,
        );
        kernel_flags.set(
            KernelParamsFlags::ANY_UNDERWATER,
            (self.compute_params.light_refraction_coefficient != 1.0
                && self.compute_params.light_refraction_coefficient > 0.0)
                || self
                    .compute_params
                    .keyframes
                    .is_keyframed(&crate::KeyframeType::LightRefractionCoeff),
        );
        // openfx-output-adjust-flip: drive mirror toggles from the output buffer description.
        kernel_flags.set(KernelParamsFlags::FLIP_H, buffers.output.flip_h);
        kernel_flags.set(KernelParamsFlags::FLIP_V, buffers.output.flip_v);

        {
            let gyro = self.compute_params.gyro.read();
            let file_metadata = gyro.file_metadata.read();
            if let Some(mc) = file_metadata.mesh_correction.get(frame) {
                if mc.1[0] > 10.0 {
                    kernel_flags.set(KernelParamsFlags::HAS_MESH_DATA, true);
                }
                if mc.1[0] > 0.0 && mc.1[mc.1[0] as usize] > 0.0 {
                    kernel_flags.set(KernelParamsFlags::HAS_FPD_DATA, true);
                }
            }
            if file_metadata.camera_stab_data.len() > frame {
                kernel_flags.set(KernelParamsFlags::HAS_IBIS_DATA, true);
            }
        }
        kernel_flags
    }

    pub fn get_frame_transform_at<T: PixelType>(
        &self,
        timestamp_us: i64,
        frame: Option<usize>,
        buffers: &Buffers,
    ) -> FrameTransform {
        let timestamp_ms = (timestamp_us as f64) / 1000.0;
        let frame = frame.unwrap_or_else(|| {
            crate::frame_at_timestamp(timestamp_ms, self.compute_params.scaled_fps) as usize
        });

        let mut transform = FrameTransform::at_timestamp(&self.compute_params, timestamp_ms, frame);
        transform.kernel_params.pixel_value_limit = T::default_max_value().unwrap_or(f32::MAX);
        transform.kernel_params.max_pixel_value = T::default_max_value().unwrap_or(1.0);
        // If the pixel format gets converted to normalized 0-1 float in shader
        if self.initialized_backend.is_wgpu() && T::wgpu_format().map(|x| x.2).unwrap_or_default() {
            transform.kernel_params.pixel_value_limit = 1.0;
            transform.kernel_params.max_pixel_value = 1.0;
        }
        transform.kernel_params.interpolation = self.interpolation as i32;
        transform.kernel_params.width = self.size.0 as i32;
        transform.kernel_params.height = self.size.1 as i32;
        transform.kernel_params.output_width = self.output_size.0 as i32;
        transform.kernel_params.output_height = self.output_size.1 as i32;
        transform.kernel_params.background = [
            self.compute_params.background[0],
            self.compute_params.background[1],
            self.compute_params.background[2],
            self.compute_params.background[3],
        ];
        transform.kernel_params.bytes_per_pixel = (T::COUNT * T::SCALAR_BYTES) as i32;
        transform.kernel_params.pix_element_count = T::COUNT as i32;
        transform.kernel_params.canvas_scale = self.drawing.scale as f32;
        transform.kernel_params.flags = self.get_kernel_flags(frame, buffers).bits();

        transform.kernel_params.stride = buffers.input.size.2 as i32;
        transform.kernel_params.output_stride = buffers.output.size.2 as i32;

        if transform.kernel_params.interpolation > 8 {
            let (b, c) = match self.interpolation {
                Interpolation::RobidouxSharp => (0.2620145, 0.3689927),
                Interpolation::Robidoux => (0.3782157, 0.3108921),
                Interpolation::Mitchell => (0.3333333, 0.3333333),
                Interpolation::CatmullRom => (0.0000000, 0.5000000),
                _ => (0.0, 0.0),
            };
            transform.kernel_params.ewa_coeffs_p[0] = (6.0 - 2.0 * b) / 6.0;
            transform.kernel_params.ewa_coeffs_p[1] = 0.0;
            transform.kernel_params.ewa_coeffs_p[2] = (-18.0 + 12.0 * b + 6.0 * c) / 6.0;
            transform.kernel_params.ewa_coeffs_p[3] = (12.0 - 9.0 * b - 6.0 * c) / 6.0;
            transform.kernel_params.ewa_coeffs_q[0] = (8.0 * b + 24.0 * c) / 6.0;
            transform.kernel_params.ewa_coeffs_q[1] = (-12.0 * b - 48.0 * c) / 6.0;
            transform.kernel_params.ewa_coeffs_q[2] = (6.0 * b + 30.0 * c) / 6.0;
            transform.kernel_params.ewa_coeffs_q[3] = (-1.0 * b - 6.0 * c) / 6.0;
        }

        let sa_fov = if self.compute_params.show_safe_area || self.compute_params.fov_overview {
            let fov = self
                .compute_params
                .keyframes
                .value_at_video_timestamp(&crate::keyframes::KeyframeType::Fov, timestamp_ms)
                .unwrap_or(self.compute_params.fov_scale) as f32;
            if self.compute_params.fov_overview {
                (if self.compute_params.adaptive_zoom_window == 0.0 {
                    1.0
                } else {
                    1.0 / fov
                }) + 1.0
            } else {
                fov / (if self.compute_params.adaptive_zoom_window == 0.0 {
                    transform.minimal_fov as f32
                } else {
                    1.0
                })
            }
        } else {
            1.0
        };
        let pos_x = (transform.kernel_params.output_width as f32
            - (transform.kernel_params.output_width as f32 / sa_fov))
            / 2.0;
        let pos_y = (transform.kernel_params.output_height as f32
            - (transform.kernel_params.output_height as f32 / sa_fov))
            / 2.0;
        transform.kernel_params.safe_area_rect[0] = pos_x;
        transform.kernel_params.safe_area_rect[1] = pos_y;
        transform.kernel_params.safe_area_rect[2] =
            transform.kernel_params.output_width as f32 - pos_x;
        transform.kernel_params.safe_area_rect[3] =
            transform.kernel_params.output_height as f32 - pos_y;

        if let Some(r) = buffers.input.rotation {
            transform.kernel_params.input_rotation = r;
        }
        if let Some(r) = buffers.output.rotation {
            transform.kernel_params.output_rotation = r;
        }
        if let Some(pa) = buffers.output.post_affine {
            transform.kernel_params.post_rotation = pa.rotation_deg;
            transform.kernel_params.post_zoom = pa.zoom;
            transform.kernel_params.post_offset = pa.offset_norm;
        }

        transform.kernel_params.source_rect = Self::get_rect(&buffers.input);
        transform.kernel_params.output_rect = Self::get_rect(&buffers.output);

        transform
    }

    pub fn ensure_stab_data_at_timestamp<T: PixelType>(
        &mut self,
        timestamp_us: i64,
        frame: Option<usize>,
        buffers: &mut Buffers,
        is_pixel_normalized: bool,
    ) {
        self.ensure_stab_data_at_timestamp_timed::<T>(timestamp_us, frame, buffers, is_pixel_normalized).0
    }
    // Variant exposed for the `stab.timing` instrumentation: returns
    // (was_inserted, elapsed_ms_of_insert). When `was_inserted` is false the
    // elapsed value is 0 (we skipped the expensive `get_frame_transform_at` /
    // map insert path).
    pub fn ensure_stab_data_at_timestamp_timed<T: PixelType>(
        &mut self,
        timestamp_us: i64,
        frame: Option<usize>,
        buffers: &mut Buffers,
        is_pixel_normalized: bool,
    ) -> ((), u64) {
        let mut insert = true;
        if let Some(itm) = self.stab_data.get(&timestamp_us) {
            insert = false;
            if itm.kernel_params.stride != buffers.input.size.2 as i32
                || itm.kernel_params.output_stride != buffers.output.size.2 as i32
            {
                log::warn!(
                    "Stride mismatch ({} != {} || {} != {})",
                    itm.kernel_params.stride,
                    buffers.input.size.2,
                    itm.kernel_params.output_stride,
                    buffers.output.size.2
                );
                insert = true;
            }
            let pa = buffers.output.post_affine.unwrap_or_default();
            if itm.kernel_params.input_rotation != buffers.input.rotation.unwrap_or(0.0)
                || itm.kernel_params.output_rotation != buffers.output.rotation.unwrap_or(0.0)
                || itm.kernel_params.source_rect != Self::get_rect(&buffers.input)
                || itm.kernel_params.output_rect != Self::get_rect(&buffers.output)
                || itm.kernel_params.post_rotation != pa.rotation_deg
                || itm.kernel_params.post_zoom != pa.zoom
                || itm.kernel_params.post_offset != pa.offset_norm
            {
                log::warn!("Updating stab params at {timestamp_us}");
                insert = true;
            }
        }
        let mut stab_data_ms: u64 = 0;
        if insert {
            let t0 = std::time::Instant::now();
            let mut transform = self.get_frame_transform_at::<T>(timestamp_us, frame, buffers);
            if is_pixel_normalized {
                transform.kernel_params.max_pixel_value = 1.0;
                transform.kernel_params.pixel_value_limit = 1.0;
            }
            self.stab_data.insert(timestamp_us, transform);
            stab_data_ms = t0.elapsed().as_millis() as u64;
        }
        ((), stab_data_ms)
    }

    pub fn get_current_key(&self, buffers: &Buffers) -> String {
        let mut flags = self.get_kernel_flags(0, buffers);
        flags.set(KernelParamsFlags::FILL_WITH_BACKGROUND, false);
        format!(
            "{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}",
            buffers.get_checksum(),
            self.compute_params.distortion_model.id(),
            self.compute_params
                .digital_lens
                .as_ref()
                .map(|x| x.id())
                .unwrap_or_default(),
            self.interpolation as u32,
            flags.bits(),
            self.size,
            self.output_size,
            self.interpolation,
            std::thread::current().id(),
        )
    }
    pub fn get_current_checksum(&self, buffers: &Buffers) -> u32 {
        crc32fast::hash(self.get_current_key(buffers).as_bytes())
    }

    /// Decompose every input of `get_current_key` into named segments for the
    /// `backend_rebuild` diagnostic. Segment count and order are static so two
    /// snapshots can be compared pairwise. Must stay in sync with
    /// `get_current_key` / `BufferDescription::get_checksum` — a key change with
    /// no differing segment here means this list is missing a component.
    fn backend_key_diag_segments(&self, buffers: &Buffers) -> Vec<(&'static str, String)> {
        let mut flags = self.get_kernel_flags(0, buffers);
        flags.set(KernelParamsFlags::FILL_WITH_BACKGROUND, false);
        vec![
            ("in_geom",  format!("{:?}", buffers.input.size)),
            ("in_rect",  format!("{:?}", buffers.input.rect)),
            ("in_rot",   format!("{:?}", buffers.input.rotation)),
            ("in_pa",    format!("{:?}", buffers.input.post_affine)),
            ("in_flip",  format!("{}{}", buffers.input.flip_h as u8, buffers.input.flip_v as u8)),
            ("in_src",   buffers.input.source_diag()),
            ("out_geom", format!("{:?}", buffers.output.size)),
            ("out_rect", format!("{:?}", buffers.output.rect)),
            ("out_rot",  format!("{:?}", buffers.output.rotation)),
            ("out_pa",   format!("{:?}", buffers.output.post_affine)),
            ("out_flip", format!("{}{}", buffers.output.flip_h as u8, buffers.output.flip_v as u8)),
            ("out_src",  buffers.output.source_diag()),
            ("lens",     format!("{}", self.compute_params.distortion_model.id())),
            ("dlens",    format!("{}", self.compute_params.digital_lens.as_ref().map(|x| x.id()).unwrap_or_default())),
            ("interp",   format!("{}", self.interpolation as u32)),
            ("flags",    format!("{}", flags.bits())),
            ("proc_size", format!("{:?}->{:?}", self.size, self.output_size)),
            ("thread",   format!("{:?}", std::thread::current().id())),
        ]
    }

    /// Emit one `target="gpu"` line naming exactly which key segment(s) forced
    /// this backend (re-)init, then store the new snapshot. Only called from the
    /// rebuild path so it costs nothing on the steady-state per-frame path.
    fn log_backend_rebuild_diff(&mut self, buffers: &Buffers) {
        let new_segs = self.backend_key_diag_segments(buffers);
        match self.last_backend_key_diag.as_ref() {
            None => {
                let full: Vec<String> = new_segs.iter().map(|(k, v)| format!("{k}={v}")).collect();
                log::info!(target: "gpu", "backend_rebuild first_init key: {}", full.join(" | "));
            }
            Some(old) => {
                let changed: Vec<String> = old.iter()
                    .zip(new_segs.iter())
                    .filter(|(o, n)| o.1 != n.1)
                    .map(|(o, n)| format!("{}: {} -> {}", o.0, o.1, n.1))
                    .collect();
                if changed.is_empty() {
                    // Key identical to last init: the backend was invalidated externally
                    // (device change, take_gpu_bindings, TLS cache miss on same thread).
                    log::info!(target: "gpu", "backend_rebuild no key change (external invalidation)");
                } else {
                    log::info!(target: "gpu", "backend_rebuild changed: {}", changed.join(" | "));
                }
            }
        }
        self.last_backend_key_diag = Some(new_segs);
    }

    pub fn init_size(&mut self, size: (usize, usize), output_size: (usize, usize)) {
        // Move the previous OpenCL / wgpu wrappers out (this also resets
        // initialized_backend / next_backend / stab_data synchronously) but
        // DEFER their Drop to the next `update_device`, which runs inline on
        // the device-owning render thread. `init_size` can run on the QML/main
        // thread (MDK `surfaceSizeUpdated` -> `onResize` queued callback on a
        // preview resize). Releasing a D3D11-shared OpenCL `Mem` re-enters the
        // D3D11 device critical section; doing that here — or on a detached
        // `gpu-wrapper-drop` worker — races the scene-graph render thread on
        // the shared device and hard-deadlocks the driver (dump 2026-06-04:
        // the render thread and the worker were both parked in nvwgf2umx at the
        // same device-CS wait). Stashing keeps the QML thread free AND lets the
        // teardown happen serialized with rendering on the render thread.
        if let Some(taken) = self.take_gpu_bindings() {
            self.pending_gpu_drop.push(taken);
        }

        self.size = size;
        self.output_size = output_size;

        if self
            .kernel_flags
            .contains(KernelParamsFlags::DRAWING_ENABLED)
        {
            self.drawing = DrawCanvas::new(
                size.0,
                size.1,
                output_size.0,
                output_size.1,
                (size.1 as f64 / 720.0).max(1.0) as usize,
            );
        }
    }

    pub fn clear_stab_data(&mut self) {
        self.stab_data.clear();
    }

    pub fn get_undistortion_data(&self, timestamp_us: i64) -> Option<&FrameTransform> {
        self.stab_data.get(&timestamp_us)
    }

    pub fn list_devices(&self) -> Vec<ProcessingDeviceInfo> {
        let mut ret = Vec::new();

        #[cfg(feature = "use-opencl")]
        if std::env::var("NO_OPENCL").unwrap_or_default().is_empty() {
            ret.extend(opencl::OclWrapper::list_devices());
        }
        if std::env::var("NO_WGPU").unwrap_or_default().is_empty() {
            ret.extend(wgpu::WgpuWrapper::list_devices());
        }
        prepare_processing_device_infos(ret)
    }

    pub fn set_device(&mut self, i: isize) {
        self.pending_device_change = Some(i);
    }

    pub fn update_device(&mut self, i: isize, buffers: &Buffers) -> bool {
        // Reset backend state and Drop the previous GPU wrappers INLINE on this
        // thread. `update_device` runs inside the per-frame processing path
        // (`ensure_ready_for_processing` -> `process_pixels`, call sites below at
        // ~944/963/984) on the device-owning thread: the Qt scene-graph render
        // thread for live preview, the ffmpeg render worker for batch. Releasing
        // a D3D11-shared OpenCL Mem here re-acquires the D3D11 device critical
        // section recursively on the SAME thread, so there is no cross-thread
        // contention. Deferring this Drop to a separate background thread instead
        // races the render thread on the shared D3D11 device and hard-deadlocks
        // the UI (dump 2026-06-02: render thread owned the device CS inside the
        // mdk D3D11 draw while the `gpu-wrapper-drop` worker blocked entering the
        // same CS from clReleaseMemObject). Keep it inline.
        //
        // First drain wrappers deferred by `init_size()` (which may have run on
        // the QML/main thread and stashed them instead of dropping): Drop them
        // INLINE here too, on this device-owning render thread, so the D3D11
        // teardown stays serialized with rendering. A detached worker doing the
        // same Drop concurrently with this render thread re-creates the exact
        // hang above (dump 2026-06-04). `update_device` is the first thing the
        // render path runs after `init_size` invalidates the backend, so the
        // stash never lingers more than one frame on the live-preview path.
        for taken in self.pending_gpu_drop.drain(..) {
            drop(taken);
        }
        drop(self.take_gpu_bindings());

        let hash = self.get_current_checksum(buffers);
        if i < 0 {
            // CPU
            CACHED_WGPU.with(|x| x.0.borrow_mut().clear());
            #[cfg(feature = "use-opencl")]
            {
                CACHED_OPENCL.with(|x| x.borrow_mut().clear());
            }
            self.initialized_backend = BackendType::Cpu(hash);
            return true;
        }
        let gpu_list = GPU_LIST.read();
        if let Some(name) = gpu_list.get(i as usize) {
            if name.starts_with("[OpenCL]") {
                self.initialized_backend = BackendType::None;
                #[cfg(feature = "use-opencl")]
                match opencl::OclWrapper::set_device(i as usize, buffers) {
                    Ok(_) => {
                        self.next_backend = Some("opencl");
                        return true;
                    }
                    Err(e) => {
                        log::error!("Failed to set OpenCL device {}: {:?}", name, e);
                    }
                }
            } else if name.starts_with("[wgpu]") {
                self.initialized_backend = BackendType::None;
                let first_ind = gpu_list
                    .iter()
                    .enumerate()
                    .find(|(_, m)| m.starts_with("[wgpu]"))
                    .map(|(idx, _)| idx)
                    .unwrap_or(0);
                let wgpu_ind = i - first_ind as isize;
                if wgpu_ind >= 0 {
                    match wgpu::WgpuWrapper::set_device(wgpu_ind as usize) {
                        Some(_) => {
                            self.next_backend = Some("wgpu");
                            return true;
                        }
                        None => {
                            log::error!("Failed to set wgpu device {}", name);
                        }
                    }
                }
            }
        }
        false
    }

    pub fn init_backends<T: PixelType>(
        &mut self,
        timestamp_us: i64,
        frame: Option<usize>,
        buffers: &Buffers,
    ) {
        let hash = self.get_current_checksum(buffers);
        let current_hash = self.initialized_backend.get_hash();

        if current_hash != hash {
            // Name the exact key segment that forced this rebuild (rect values,
            // thread rotation, sizes, ...) — backend_init_ms alone only proves
            // that rebuilds happen, not why.
            self.log_backend_rebuild_diff(buffers);
            // §3.3: measure wall time of the (re-)init path and emit it under
            // the chosen backend's bucket. Lazy `add_and_maybe_emit` at the
            // end of this function so we know which backend won the init.
            let init_t0 = std::time::Instant::now();
            self.initialized_backend = BackendType::None;
            let canvas_len = self.drawing.get_buffer_len();
            #[allow(unused_mut)]
            let mut next_backend = self.next_backend.take().unwrap_or_default();

            #[cfg(feature = "use-opencl")]
            if std::env::var("NO_OPENCL").unwrap_or_default().is_empty()
                && next_backend != "wgpu"
                && opencl::is_buffer_supported(buffers)
            {
                if self.share_wgpu_instances && CACHED_OPENCL.with(|x| x.borrow().contains(&hash)) {
                    self.cl = None;
                    self.initialized_backend = BackendType::OpenCL(hash);
                } else {
                    self.cl = None;
                    let transform = self.get_frame_transform_at::<T>(timestamp_us, frame, buffers);
                    let params = transform.kernel_params;
                    let distortion_model = self.compute_params.distortion_model.clone();
                    let digital_lens = self.compute_params.digital_lens.clone();
                    let cl = std::panic::catch_unwind(|| {
                        opencl::OclWrapper::new(
                            &params,
                            T::ocl_names(),
                            distortion_model,
                            digital_lens,
                            buffers,
                            canvas_len,
                        )
                    });
                    match cl {
                        Ok(Ok(cl)) => {
                            if self.share_wgpu_instances {
                                CACHED_OPENCL.with(|x| x.borrow_mut().put(hash, cl));
                            } else {
                                self.cl = Some(cl);
                            }
                            self.initialized_backend = BackendType::OpenCL(hash);
                            log::info!(
                                "Initialized OpenCL for {:?} -> {:?}, key: {}",
                                buffers.input.size,
                                buffers.output.size,
                                self.get_current_key(buffers)
                            );
                            CACHED_WGPU.with(|x| x.0.borrow_mut().clear());
                        }
                        Ok(Err(e)) => {
                            next_backend = "";
                            log::error!("OpenCL error init_backends: {:?}", e);
                            if self.share_wgpu_instances {
                                CACHED_OPENCL.with(|x| x.borrow_mut().clear())
                            }
                        }
                        Err(e) => {
                            next_backend = "";
                            if let Some(s) = e.downcast_ref::<&str>() {
                                log::error!("Failed to initialize OpenCL {}", s);
                            } else if let Some(s) = e.downcast_ref::<String>() {
                                log::error!("Failed to initialize OpenCL {}", s);
                            } else {
                                log::error!("Failed to initialize OpenCL {:?}", e);
                            }
                            if self.share_wgpu_instances {
                                CACHED_OPENCL.with(|x| x.borrow_mut().clear());
                            }
                        }
                    }
                }
            }
            if self.initialized_backend.is_none()
                && T::wgpu_format().is_some()
                && next_backend != "opencl"
                && std::env::var("NO_WGPU").unwrap_or_default().is_empty()
                && wgpu::is_buffer_supported(buffers)
            {
                if self.share_wgpu_instances && CACHED_WGPU.with(|x| x.0.borrow().contains(&hash)) {
                    self.wgpu = None;
                    self.initialized_backend = BackendType::Wgpu(hash);
                } else {
                    self.wgpu = None;
                    let transform = self.get_frame_transform_at::<T>(timestamp_us, frame, buffers);
                    let params = transform.kernel_params;
                    let distortion_model = self.compute_params.distortion_model.clone();
                    let digital_lens = self.compute_params.digital_lens.clone();
                    let wgpu = std::panic::catch_unwind(|| {
                        wgpu::WgpuWrapper::new(
                            &params,
                            T::wgpu_format().unwrap(),
                            distortion_model,
                            digital_lens,
                            buffers,
                            canvas_len,
                        )
                    });
                    match wgpu {
                        Ok(Ok(wgpu)) => {
                            if self.share_wgpu_instances {
                                CACHED_WGPU.with(|x| x.0.borrow_mut().put(hash, wgpu));
                            } else {
                                self.wgpu = Some(wgpu);
                            }
                            self.initialized_backend = BackendType::Wgpu(hash);
                            log::info!(
                                "Initialized wgpu for {:?} -> {:?} | key: {}",
                                buffers.input.size,
                                buffers.output.size,
                                self.get_current_key(buffers)
                            );
                            #[cfg(feature = "use-opencl")]
                            {
                                CACHED_OPENCL.with(|x| x.borrow_mut().clear());
                            }
                        }
                        Ok(Err(e)) => {
                            log::error!("Failed to initialize wgpu {:?}", e);
                            if self.share_wgpu_instances {
                                CACHED_WGPU.with(|x| x.0.borrow_mut().clear())
                            }
                        }
                        Err(e) => {
                            if let Some(s) = e.downcast_ref::<&str>() {
                                log::error!("Failed to initialize wgpu {}", s);
                            } else if let Some(s) = e.downcast_ref::<String>() {
                                log::error!("Failed to initialize wgpu {}", s);
                            } else {
                                log::error!("Failed to initialize wgpu {:?}", e);
                            }
                            if self.share_wgpu_instances {
                                CACHED_WGPU.with(|x| x.0.borrow_mut().clear());
                            }
                        }
                    }
                }
            }
            // §3.3: backend init wall time, attributed to whichever backend
            // we ended up settling on. Throttled to ≤1 emit/sec/backend.
            let init_ms = init_t0.elapsed().as_millis() as u64;
            add_and_maybe_emit(self.initialized_backend.name(), 0, 0, init_ms);
        }
    }

    pub fn ensure_ready_for_processing<T: PixelType>(
        &mut self,
        timestamp_us: i64,
        frame: Option<usize>,
        buffers: &mut Buffers,
    ) {
        let pending_dev = self.pending_device_change.clone();
        if let Some(dev) = self.pending_device_change.take() {
            log::debug!("Setting device {dev}");
            self.update_device(dev, buffers);
        }

        self.init_backends::<T>(timestamp_us, frame, buffers);
        self.ensure_stab_data_at_timestamp::<T>(timestamp_us, frame, buffers, false);

        if self.share_wgpu_instances {
            if wgpu::is_buffer_supported(buffers) && CACHED_WGPU.with(|x| !x.0.borrow().is_empty())
            {
                let hash = self.get_current_checksum(buffers);
                let has_cached = CACHED_WGPU.with(|x| x.0.borrow().contains(&hash));
                if !has_cached {
                    log::warn!(
                        "Cached wgpu not found, reinitializing. Key: {}",
                        self.get_current_key(buffers)
                    );
                    self.initialized_backend = BackendType::None;
                    if let Some(dev) = pending_dev {
                        log::debug!("Setting device {dev}");
                        self.update_device(dev, buffers);
                    }
                    self.init_backends::<T>(timestamp_us, frame, buffers);
                } else {
                    self.initialized_backend = BackendType::Wgpu(hash);
                }
            } else {
                #[cfg(feature = "use-opencl")]
                if opencl::is_buffer_supported(buffers)
                    && CACHED_OPENCL.with(|x| !x.borrow().is_empty())
                {
                    let hash = self.get_current_checksum(buffers);
                    let has_cached = CACHED_OPENCL.with(|x| x.borrow().contains(&hash));
                    if !has_cached {
                        log::warn!(
                            "Cached OpenCL not found, reinitializing. Key: {}",
                            self.get_current_key(buffers)
                        );
                        self.initialized_backend = BackendType::None;
                        if let Some(dev) = pending_dev {
                            log::debug!("Setting device {dev}");
                            self.update_device(dev, buffers);
                        }
                        self.init_backends::<T>(timestamp_us, frame, buffers);
                    } else {
                        self.initialized_backend = BackendType::OpenCL(hash);
                    }
                }
            }
        }
    }
    pub fn process_pixels<T: PixelType>(
        &self,
        timestamp_us: i64,
        frame: Option<usize>,
        buffers: &mut Buffers,
        frame_transform: Option<&FrameTransform>,
    ) -> Result<ProcessedInfo, GyroflowCoreError> {
        // §3.2 / §3.4: per-call render timing. The Drop guard fires on every
        // exit (Ok, Err, panic) and accumulates `gpu_ms` against the chosen
        // backend's `stab.timing` bucket. Throttle in `add_and_maybe_emit`
        // collapses 60×/sec calls to ≤1 emit/sec/backend.
        struct TimingGuard {
            backend: &'static str,
            start: std::time::Instant,
        }
        impl Drop for TimingGuard {
            fn drop(&mut self) {
                let gpu_ms = self.start.elapsed().as_millis() as u64;
                add_and_maybe_emit(self.backend, 0, gpu_ms, 0);
            }
        }
        let _timing_guard = TimingGuard {
            backend: self.initialized_backend.name(),
            start: std::time::Instant::now(),
        };

        if buffers.input.size.1 < 4 || buffers.output.size.1 < 4 {
            return Err(GyroflowCoreError::SizeTooSmall);
        }

        let mut _tmp_transform = None;
        if frame_transform.is_none() && !self.cache_frame_transform {
            _tmp_transform = Some(self.get_frame_transform_at::<T>(timestamp_us, frame, buffers));
        }
        let itm = frame_transform.map(|x| Some(x)).unwrap_or_else(|| {
            if !self.cache_frame_transform {
                _tmp_transform.as_ref()
            } else {
                self.stab_data.get(&timestamp_us)
            }
        });

        if let Some(itm) = itm {
            let mut ret = ProcessedInfo {
                fov: itm.fov,
                minimal_fov: itm.minimal_fov,
                focal_length: itm.focal_length,
                backend: "",
            };
            let drawing_buffer = self.drawing.get_buffer();

            if self.size
                != (
                    itm.kernel_params.width as usize,
                    itm.kernel_params.height as usize,
                )
            {
                return Err(GyroflowCoreError::SizeMismatch(
                    self.size,
                    (
                        itm.kernel_params.width as usize,
                        itm.kernel_params.height as usize,
                    ),
                ));
            }
            if self.output_size
                != (
                    itm.kernel_params.output_width as usize,
                    itm.kernel_params.output_height as usize,
                )
            {
                return Err(GyroflowCoreError::SizeMismatch(
                    self.size,
                    (
                        itm.kernel_params.output_width as usize,
                        itm.kernel_params.output_height as usize,
                    ),
                ));
            }

            if buffers.input.size.0 as i32 > itm.kernel_params.stride {
                return Err(GyroflowCoreError::InvalidStride(
                    itm.kernel_params.stride,
                    buffers.input.size.0 as i32,
                ));
            }
            if buffers.output.size.0 as i32 > itm.kernel_params.output_stride {
                return Err(GyroflowCoreError::InvalidStride(
                    itm.kernel_params.output_stride,
                    buffers.output.size.0 as i32,
                ));
            }

            // OpenCL path
            #[cfg(feature = "use-opencl")]
            if !matches!(self.initialized_backend, BackendType::Cpu(_))
                && opencl::is_buffer_supported(buffers)
            {
                if self.share_wgpu_instances {
                    let hash = self.get_current_checksum(buffers);
                    let has_cache = CACHED_OPENCL.with(|lru| lru.borrow().contains(&hash));
                    if has_cache {
                        return CACHED_OPENCL.with(|x| {
                            let mut cached = x.borrow_mut();
                            if let Some(cl) = cached.get(&hash) {
                                if let Err(err) = cl.undistort_image(buffers, &itm, drawing_buffer)
                                {
                                    log::error!("OpenCL error undistort: {:?}", err);
                                }
                                ret.backend = "OpenCL";
                                Ok(ret)
                            } else {
                                Err(GyroflowCoreError::NoCachedWgpuInstance(
                                    self.get_current_key(buffers),
                                ))
                            }
                        });
                    }
                } else {
                    if let Some(ref cl) = self.cl {
                        if let Err(err) = cl.undistort_image(buffers, &itm, drawing_buffer) {
                            log::error!("OpenCL error undistort: {:?}", err);
                        } else {
                            ret.backend = "OpenCL";
                            return Ok(ret);
                        }
                    }
                }
            }

            // wgpu path
            if !matches!(self.initialized_backend, BackendType::Cpu(_))
                && wgpu::is_buffer_supported(buffers)
            {
                if self.share_wgpu_instances {
                    let hash = self.get_current_checksum(buffers);
                    let has_any_cache = CACHED_WGPU.with(|x| !x.0.borrow().is_empty());
                    if has_any_cache {
                        return CACHED_WGPU.with(|x| {
                            let mut cached = x.0.borrow_mut();
                            if let Some(wgpu) = cached.get(&hash) {
                                wgpu.undistort_image(buffers, &itm, drawing_buffer);
                                ret.backend = "wgpu";
                                Ok(ret)
                            } else {
                                Err(GyroflowCoreError::NoCachedWgpuInstance(
                                    self.get_current_key(buffers),
                                ))
                            }
                        });
                    } else {
                        log::error!(
                            "No cached wgpu found for key: {}",
                            self.get_current_key(buffers)
                        );
                    }
                } else {
                    if let Some(ref wgpu) = self.wgpu {
                        wgpu.undistort_image(buffers, &itm, drawing_buffer);
                        ret.backend = "wgpu";
                        return Ok(ret);
                    } else {
                        log::error!("No wgpu instance!");
                    }
                }
            }

            //let ok = Self::undistort_image_cpu_spirv::<T>(buffers, &itm.kernel_params, &self.compute_params.distortion_model, self.compute_params.digital_lens.as_ref(), &itm.matrices, drawing_buffer);
            // CPU path
            let ok = match self.interpolation {
                Interpolation::Bilinear => Self::undistort_image_cpu::<2, T>(
                    buffers,
                    &itm.kernel_params,
                    &self.compute_params.distortion_model,
                    self.compute_params.digital_lens.as_ref(),
                    &itm.matrices,
                    drawing_buffer,
                    &itm.mesh_data,
                ),
                Interpolation::Bicubic => Self::undistort_image_cpu::<4, T>(
                    buffers,
                    &itm.kernel_params,
                    &self.compute_params.distortion_model,
                    self.compute_params.digital_lens.as_ref(),
                    &itm.matrices,
                    drawing_buffer,
                    &itm.mesh_data,
                ),
                Interpolation::Lanczos4 => Self::undistort_image_cpu::<8, T>(
                    buffers,
                    &itm.kernel_params,
                    &self.compute_params.distortion_model,
                    self.compute_params.digital_lens.as_ref(),
                    &itm.matrices,
                    drawing_buffer,
                    &itm.mesh_data,
                ),
                Interpolation::RobidouxSharp => Self::undistort_image_cpu::<10, T>(
                    buffers,
                    &itm.kernel_params,
                    &self.compute_params.distortion_model,
                    self.compute_params.digital_lens.as_ref(),
                    &itm.matrices,
                    drawing_buffer,
                    &itm.mesh_data,
                ),
                Interpolation::Robidoux => Self::undistort_image_cpu::<11, T>(
                    buffers,
                    &itm.kernel_params,
                    &self.compute_params.distortion_model,
                    self.compute_params.digital_lens.as_ref(),
                    &itm.matrices,
                    drawing_buffer,
                    &itm.mesh_data,
                ),
                Interpolation::Mitchell => Self::undistort_image_cpu::<12, T>(
                    buffers,
                    &itm.kernel_params,
                    &self.compute_params.distortion_model,
                    self.compute_params.digital_lens.as_ref(),
                    &itm.matrices,
                    drawing_buffer,
                    &itm.mesh_data,
                ),
                Interpolation::CatmullRom => Self::undistort_image_cpu::<13, T>(
                    buffers,
                    &itm.kernel_params,
                    &self.compute_params.distortion_model,
                    self.compute_params.digital_lens.as_ref(),
                    &itm.matrices,
                    drawing_buffer,
                    &itm.mesh_data,
                ),
            };
            if ok {
                ret.backend = "CPU";
                return Ok(ret);
            }
        } else {
            // Transient state during project/video load — ensure_ready_for_processing
            // may not have populated stab_data[ts] yet, or another writer cleared
            // it between ensure (write lock released) and process_pixels (read lock).
            // Next frame typically self-heals; documented behavior, not an incident.
            log::info!("No stab data at {timestamp_us}");
            return Err(GyroflowCoreError::NoStabilizationData(timestamp_us));
        }
        Err(GyroflowCoreError::Unknown)
    }
}

unsafe impl Send for Stabilization {}
unsafe impl Sync for Stabilization {}
