// SPDX-License-Identifier: GPL-3.0-or-later
// Native CUDA rendering on the OFX host's stream, using the shared OpenCL equations.
use super::{BufferSource, Buffers};
use crate::stabilization::{FrameTransform, PixelType, distortion_models::DistortionModel};
use libloading::Library;
use parking_lot::Mutex;
use std::{
    cell::Cell,
    ffi::{CString, c_char, c_void},
    sync::{Arc, OnceLock},
};

thread_local! { static HOST_STREAM: Cell<Option<usize>> = const { Cell::new(None) }; }
pub struct HostStreamGuard(Option<usize>, std::marker::PhantomData<*mut ()>);
impl HostStreamGuard {
    pub fn new(stream: Option<usize>) -> Self {
        Self(
            HOST_STREAM.with(|s| s.replace(stream)),
            std::marker::PhantomData,
        )
    }
}
impl Drop for HostStreamGuard {
    fn drop(&mut self) {
        HOST_STREAM.with(|s| s.set(self.0));
    }
}

/// Opt out for diagnostics; unsupported formats and hosts keep the old backend.
pub fn configured() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("GYROFLOW_NATIVE_CUDA")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "off" | "false" | "no"
                )
            })
            .unwrap_or(true)
    })
}

/// Check runtime libraries before changing the host's synchronization contract.
pub fn available() -> bool {
    configured() && driver().is_ok() && nvrtc().is_ok()
}

pub fn enabled_for<T: PixelType>(buffers: &Buffers) -> bool {
    configured()
        && has_host_stream(buffers)
        && T::ocl_names().0 == "float4"
        && matches!(buffers.input.data, BufferSource::CUDABuffer { .. })
        && matches!(buffers.output.data, BufferSource::CUDABuffer { .. })
}

pub fn has_host_stream(buffers: &Buffers) -> bool {
    HOST_STREAM.with(|s| s.get().is_some())
        && matches!(buffers.input.data, BufferSource::CUDABuffer { .. })
}

macro_rules! api {
    ($name:ident { $($f:ident($($a:ident: $t:ty),*) -> $r:ty;)* }) => {
        #[allow(non_snake_case)]
        struct $name { $($f: unsafe extern "system" fn($($t),*) -> $r,)* _library: Library }
        impl $name {
            fn load(library: Library) -> Result<Self, String> {
                unsafe { Ok(Self {
                    $($f: *library.get(concat!(stringify!($f), "\0").as_bytes()).map_err(|e| e.to_string())?,)*
                    _library: library,
                }) }
            }
        }
    }
}
api!(Driver {
    cuCtxGetCurrent(context: *mut usize) -> i32;
    cuCtxGetDevice(device: *mut i32) -> i32;
    cuDeviceGetAttribute(value: *mut i32, attribute: i32, device: i32) -> i32;
    cuCtxPushCurrent_v2(context: usize) -> i32;
    cuCtxPopCurrent_v2(context: *mut usize) -> i32;
    cuModuleLoadData(module: *mut usize, image: *const c_void) -> i32;
    cuModuleGetFunction(function: *mut usize, module: usize, name: *const c_char) -> i32;
    cuModuleUnload(module: usize) -> i32;
    cuMemAlloc_v2(ptr: *mut u64, bytes: usize) -> i32;
    cuMemFree_v2(ptr: u64) -> i32;
    cuMemHostAlloc(ptr: *mut usize, bytes: usize, flags: u32) -> i32;
    cuMemFreeHost(ptr: usize) -> i32;
    cuMemcpyHtoDAsync_v2(dst: u64, src: *const c_void, bytes: usize, stream: usize) -> i32;
    cuEventCreate(event: *mut usize, flags: u32) -> i32;
    cuEventRecord(event: usize, stream: usize) -> i32;
    cuEventSynchronize(event: usize) -> i32;
    cuEventDestroy_v2(event: usize) -> i32;
    cuStreamSynchronize(stream: usize) -> i32;
    cuLaunchKernel(function: usize, gx: u32, gy: u32, gz: u32, bx: u32, by: u32, bz: u32, shared: u32, stream: usize, args: *mut *mut c_void, extra: *mut *mut c_void) -> i32;
});
api!(Nvrtc {
    nvrtcCreateProgram(program: *mut usize, source: *const c_char, name: *const c_char, count: i32, headers: *const *const c_char, names: *const *const c_char) -> i32;
    nvrtcCompileProgram(program: usize, count: i32, options: *const *const c_char) -> i32;
    nvrtcGetProgramLogSize(program: usize, size: *mut usize) -> i32;
    nvrtcGetProgramLog(program: usize, log: *mut c_char) -> i32;
    nvrtcGetPTXSize(program: usize, size: *mut usize) -> i32;
    nvrtcGetPTX(program: usize, ptx: *mut c_char) -> i32;
    nvrtcDestroyProgram(program: *mut usize) -> i32;
});
fn check(code: i32, operation: &str) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("{operation}: CUDA error {code}"))
    }
}
fn driver() -> Result<&'static Driver, String> {
    static API: OnceLock<Result<Driver, String>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        Driver::load(
            Library::new(if cfg!(windows) {
                "nvcuda.dll"
            } else {
                "libcuda.so.1"
            })
            .map_err(|e| e.to_string())?,
        )
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Establish readiness before returning a failed native attempt to a backend
/// which does not know about the host's non-default CUDA stream.
pub fn wait_for_host_stream() -> Result<(), String> {
    if let Some(stream) = HOST_STREAM.with(|s| s.get()) {
        let d = driver()?;
        unsafe {
            check((d.cuStreamSynchronize)(stream), "host stream fallback wait")?;
        }
    }
    Ok(())
}
fn nvrtc() -> Result<&'static Nvrtc, String> {
    static API: OnceLock<Result<Nvrtc, String>> = OnceLock::new();
    API.get_or_init(|| {
        let names = if cfg!(windows) {
            vec!["nvrtc64_120_0.dll", "nvrtc64_130_0.dll"]
        } else {
            vec!["libnvrtc.so.12", "libnvrtc.so.13", "libnvrtc.so"]
        };
        for name in names {
            if let Ok(lib) = unsafe { Library::new(name) } {
                return Nvrtc::load(lib);
            }
        }
        Err("NVRTC library unavailable".into())
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn shader_source(
    params: &crate::stabilization::KernelParams,
    model: &DistortionModel,
    digital: Option<&DistortionModel>,
) -> String {
    let interpolation = params.interpolation;
    let mut lens = model.opencl_functions().to_string();
    lens.push_str(digital.map(|m| m.opencl_functions()).unwrap_or(
        "float2 digital_undistort_point(float2 uv, KernelParams *p){return uv;}\nfloat2 digital_distort_point(float2 uv, KernelParams *p){return uv;}"));
    let mut code = include_str!("opencl_undistort.cl")
        .replace("\r\n", "\n")
        .replace("LENS_MODEL_FUNCTIONS;", &lens)
        .replace("EXTENSIONS;", "")
        .replace("DATA_CONVERTF", "convert_float4")
        .replace("DATA_TYPEF", "float4")
        .replace("DATA_CONVERT", "convert_float4")
        .replace("DATA_TYPE", "float4")
        .replace("PIXEL_BYTES", "16")
        .replace("INTERPOLATION", &interpolation.to_string())
        // Interpolation phases differ across warp lanes. Constant-memory
        // broadcasts serialize those addresses; a cached global read does not.
        .replace("__constant float coeffs[", "__device__ const float coeffs[")
        .replace("__constant float *", "const float *");
    // Match the replaced WGSL path: adding 0.5 then truncating shifts negative
    // phases at the top/left edge and treats halfway phases differently.
    code = code.replace("convert_int_sat_rtz(0.5f + ", "convert_int_sat_rte(");
    // WGSL skips identity rect remaps. Performing a redundant multiply/divide
    // can move a coordinate across a halfway interpolation phase at 4K sizes.
    code = regex::Regex::new(
        r"(?m)^    out_pos\.x = map_coord\([^\n]+\);\n    out_pos\.y = map_coord\([^\n]+\);",
    )
    .unwrap()
    .replace(&code, "    if ((params->flags & 64)) {\n$0\n    }")
    .into_owned();
    code = code.replace(
        "if (params->background_mode != 3) {",
        "if ((params->flags & 32) && params->background_mode != 3) {",
    );
    code = regex::Regex::new(r"(?m)^                uv\.x  = map_coord\([^\n]+\);\n                uv\.y  = map_coord\([^\n]+\);\n                pt2\.x = map_coord\([^\n]+\);\n                pt2\.y = map_coord\([^\n]+\);")
        .unwrap().replace(&code, "                if ((params->flags & 32)) {\n$0\n                }").into_owned();
    // Preserve OpenCL splat casts, but convert multi-component constructors.
    code = regex::Regex::new(r"\((float[234]|int4)\)\s*\(")
        .unwrap()
        .replace_all(&code, "$1(")
        .into_owned();
    code = code.replace("int4(", "gf_make_int4(");
    code = regex::Regex::new(r"(__kernel void undistort_image\([^\n]+)(\) \{)")
        .unwrap()
        .replace(&code, "$1, int buf_width, int buf_height$2")
        .into_owned();
    code = code.replace(
        "int buf_y = get_global_id(1);",
        "int buf_y = get_global_id(1);\nif (buf_x >= buf_width || buf_y >= buf_height) return;",
    );
    // The OpenCL template leaves pixels outside output_rect unwritten. Vulkan
    // defines them as background, so direct CUDA output must do so explicitly.
    code = code.replace("    if (matrices == 0 || params->width < 1) return;",
        "    if (x < 0.0f || y < 0.0f || x >= (float)params->output_width || y >= (float)params->output_height) {\n        *(__global float4 *)&dstptr[buf_x * 16 + buf_y * params->output_stride] = bg;\n        return;\n    }\n    if (matrices == 0 || params->width < 1) return;");
    if interpolation == 8 {
        // Transpose the phase/tap table into shared memory. Lane-dependent
        // phases then select banks directly rather than serializing reads.
        code = regex::Regex::new(r"(float4 sample_input_at\([^\n]+)(\) \{)")
            .unwrap()
            .replace(&code, "$1, const float *weights$2")
            .into_owned();
        code = regex::Regex::new(
            r"sample_input_at\((uv|pt2),\s*jac,\s*srcptr,\s*params,\s*drawing,\s*bg\)",
        )
        .unwrap()
        .replace_all(
            &code,
            "sample_input_at($1, jac, srcptr, params, drawing, bg, weights)",
        )
        .into_owned();
        code = code
            .replace("coeffs_x[xp]", "weights[xp*32+(sx0 & 31)]")
            .replace("coeffs_y[yp]", "weights[yp*32+(sy0 & 31)]");
        code = code.replace("int buf_x = get_global_id(0);",
            "__shared__ float weights[256]; int tid=threadIdx.y*blockDim.x+threadIdx.x; for(int i=tid;i<256;i+=blockDim.x*blockDim.y){weights[i]=coeffs[(i & 31)*8+i/32];} __syncthreads();\nint buf_x = get_global_id(0);");
        code = code
            .replace(
                "xsum += srcpx * weights[xp*32+(sx0 & 31)];",
                "xsum = color_mad(srcpx, weights[xp*32+(sx0 & 31)], xsum);",
            )
            .replace(
                "xsum += bg * weights[xp*32+(sx0 & 31)];",
                "xsum = color_mad(bg, weights[xp*32+(sx0 & 31)], xsum);",
            )
            .replace(
                "sum += xsum * weights[yp*32+(sy0 & 31)];",
                "sum = color_mad(xsum, weights[yp*32+(sy0 & 31)], sum);",
            );
    }
    for i in 0..31 {
        let flag = 1i32 << i;
        if flag != 4 {
            code = code.replace(
                &format!("(params->flags & {flag})"),
                if params.flags & flag != 0 {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }
    format!(
        "{}\n{}\nstatic_assert(sizeof(KernelParams)==400, \"CUDA parameter ABI\");",
        include_str!("cuda_compat.cuh"),
        code
    )
}

fn compile(source: &str) -> Result<Vec<u8>, String> {
    let n = nvrtc()?;
    let d = driver()?;
    let source = CString::new(source).map_err(|e| e.to_string())?;
    let mut device = 0;
    let mut major = 0;
    let mut minor = 0;
    unsafe {
        check((d.cuCtxGetDevice)(&mut device), "current device")?;
        check(
            (d.cuDeviceGetAttribute)(&mut major, 75, device),
            "compute capability",
        )?;
        check(
            (d.cuDeviceGetAttribute)(&mut minor, 76, device),
            "compute capability",
        )?;
    }
    let options = [
        format!("--gpu-architecture=compute_{major}{minor}"),
        "--std=c++14".into(),
        "--fmad=false".into(),
        "-default-device".into(),
    ]
    .into_iter()
    .map(|s| CString::new(s).unwrap())
    .collect::<Vec<_>>();
    let pointers = options.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
    let mut program = 0;
    unsafe {
        check(
            (n.nvrtcCreateProgram)(
                &mut program,
                source.as_ptr(),
                c"gyroflow.cu".as_ptr(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            ),
            "NVRTC create",
        )?;
        let result = (|| {
            let result = (n.nvrtcCompileProgram)(program, pointers.len() as i32, pointers.as_ptr());
            if result != 0 {
                let mut len = 0;
                (n.nvrtcGetProgramLogSize)(program, &mut len);
                let mut log = vec![0u8; len.max(1)];
                (n.nvrtcGetProgramLog)(program, log.as_mut_ptr().cast());
                return Err(format!(
                    "NVRTC compilation {result}: {}",
                    String::from_utf8_lossy(&log)
                ));
            }
            let mut len = 0;
            check((n.nvrtcGetPTXSize)(program, &mut len), "PTX size")?;
            let mut ptx = vec![0u8; len];
            check((n.nvrtcGetPTX)(program, ptx.as_mut_ptr().cast()), "PTX")?;
            Ok(ptx)
        })();
        (n.nvrtcDestroyProgram)(&mut program);
        result
    }
}

#[derive(Default)]
struct Slot {
    host: usize,
    device: u64,
    capacity: usize,
    event: usize,
    pending: bool,
    // A queued launch must keep its module alive even after cache eviction.
    kernel: Option<Arc<Kernel>>,
}
impl Slot {
    unsafe fn prepare(&mut self, d: &Driver, bytes: usize) -> Result<(), String> {
        unsafe {
            if self.pending {
                check(
                    (d.cuEventSynchronize)(self.event),
                    "parameter slot completion",
                )?;
                self.pending = false;
                self.kernel = None;
            }
            if self.capacity < bytes {
                self.free(d);
                check(
                    (d.cuMemAlloc_v2)(&mut self.device, bytes),
                    "parameter allocation",
                )?;
                check(
                    (d.cuMemHostAlloc)(&mut self.host, bytes, 0),
                    "pinned parameter allocation",
                )?;
                // BLOCKING_SYNC | DISABLE_TIMING: waiting must not spin a host render thread.
                check((d.cuEventCreate)(&mut self.event, 1 | 2), "parameter event")?;
                self.capacity = bytes;
            }
            Ok(())
        }
    }
    unsafe fn free(&mut self, d: &Driver) {
        unsafe {
            if self.pending {
                (d.cuEventSynchronize)(self.event);
            }
            if self.event != 0 {
                (d.cuEventDestroy_v2)(self.event);
            }
            if self.device != 0 {
                (d.cuMemFree_v2)(self.device);
            }
            if self.host != 0 {
                (d.cuMemFreeHost)(self.host);
            }
            *self = Self::default();
        }
    }
}
struct Kernel {
    context: usize,
    module: usize,
    function: usize,
}
impl Kernel {
    fn load(context: usize, ptx: &[u8]) -> Result<Self, String> {
        let d = driver()?;
        let mut module = 0;
        let mut function = 0;
        unsafe {
            check(
                (d.cuModuleLoadData)(&mut module, ptx.as_ptr().cast()),
                "module load",
            )?;
            if let Err(e) = check(
                (d.cuModuleGetFunction)(&mut function, module, c"undistort_image".as_ptr()),
                "kernel lookup",
            ) {
                (d.cuModuleUnload)(module);
                return Err(e);
            }
        }
        Ok(Self {
            context,
            module,
            function,
        })
    }
}

struct StreamState {
    context: usize,
    stream: usize,
    slots: [Slot; 8],
    next: usize,
    failed_submission: bool,
}
impl StreamState {
    fn new(context: usize, stream: usize) -> Self {
        Self {
            context,
            stream,
            slots: std::array::from_fn(|_| Slot::default()),
            next: 0,
            failed_submission: false,
        }
    }

    fn render(
        &mut self,
        kernel: Arc<Kernel>,
        src: u64,
        dst: u64,
        dimensions: (usize, usize),
        itm: &FrameTransform,
        drawing: &[u8],
        stream: usize,
    ) -> Result<(), String> {
        let d = driver()?;
        if self.failed_submission {
            unsafe {
                check(
                    (d.cuStreamSynchronize)(stream),
                    "previous failed launch completion",
                )?;
            }
            self.failed_submission = false;
        }
        let params = bytemuck::bytes_of(&itm.kernel_params);
        let matrices: &[u8] = bytemuck::cast_slice(&itm.matrices);
        let mesh: &[u8] = bytemuck::cast_slice(&itm.mesh_data);
        let drawing = if itm.kernel_params.flags & 8 != 0 {
            drawing
        } else {
            &[]
        };
        let bytes = params.len() + matrices.len() + mesh.len() + drawing.len();
        let slot = &mut self.slots[self.next];
        self.next = (self.next + 1) % 8;
        unsafe {
            slot.prepare(d, bytes)?;
            let mut offset = 0;
            for slice in [params, matrices, mesh, drawing] {
                std::ptr::copy_nonoverlapping(
                    slice.as_ptr(),
                    (slot.host as *mut u8).add(offset),
                    slice.len(),
                );
                offset += slice.len();
            }
            let mut args = [
                src,
                dst,
                slot.device,
                slot.device + params.len() as u64,
                if drawing.is_empty() {
                    0
                } else {
                    slot.device + (params.len() + matrices.len() + mesh.len()) as u64
                },
                if mesh.is_empty() {
                    0
                } else {
                    slot.device + (params.len() + matrices.len()) as u64
                },
            ];
            let mut width = dimensions.0 as i32;
            let mut height = dimensions.1 as i32;
            let mut pointers = args
                .iter_mut()
                .map(|v| v as *mut u64 as *mut c_void)
                .collect::<Vec<_>>();
            pointers.push((&mut width as *mut i32).cast());
            pointers.push((&mut height as *mut i32).cast());
            slot.kernel = Some(kernel.clone());
            // Narrower rows improve locality for the separable interpolation
            // phases. EWA keeps its original layout (different sampling loop).
            let (bx, by) = if itm.kernel_params.interpolation <= 8 {
                (8u32, 32u32)
            } else {
                (16, 8)
            };
            let submitted = (|| {
                check(
                    (d.cuMemcpyHtoDAsync_v2)(
                        slot.device,
                        slot.host as *const c_void,
                        bytes,
                        stream,
                    ),
                    "parameter upload",
                )?;
                check(
                    (d.cuLaunchKernel)(
                        kernel.function,
                        (width as u32).div_ceil(bx),
                        (height as u32).div_ceil(by),
                        1,
                        bx,
                        by,
                        1,
                        0,
                        stream,
                        pointers.as_mut_ptr(),
                        std::ptr::null_mut(),
                    ),
                    "kernel launch",
                )?;
                check(
                    (d.cuEventRecord)(slot.event, stream),
                    "parameter completion event",
                )
            })();
            if let Err(error) = submitted {
                // The upload may have succeeded even if a later enqueue failed.
                self.failed_submission = true;
                check((d.cuStreamSynchronize)(stream), "failed launch completion")?;
                self.failed_submission = false;
                slot.kernel = None;
                return Err(error);
            }
            slot.pending = true;
        }
        Ok(())
    }
}
impl Drop for Kernel {
    fn drop(&mut self) {
        if let Ok(d) = driver() {
            unsafe {
                if (d.cuCtxPushCurrent_v2)(self.context) == 0 {
                    (d.cuModuleUnload)(self.module);
                    let mut previous = 0;
                    (d.cuCtxPopCurrent_v2)(&mut previous);
                }
            }
        }
    }
}

impl Drop for StreamState {
    fn drop(&mut self) {
        if let Ok(d) = driver() {
            unsafe {
                if (d.cuCtxPushCurrent_v2)(self.context) == 0 {
                    if !self.failed_submission || (d.cuStreamSynchronize)(self.stream) == 0 {
                        for slot in &mut self.slots {
                            slot.free(d);
                        }
                    } else {
                        // An unresolved failed enqueue has no completion event.
                        // Retain its allocations/modules rather than reuse live memory.
                        for slot in &mut self.slots {
                            std::mem::forget(slot.kernel.take());
                        }
                    }
                    let mut previous = 0;
                    (d.cuCtxPopCurrent_v2)(&mut previous);
                }
            }
        }
    }
}

// Only compilation failures are persistent. Device allocation/load failures and
// render errors may recover on the next frame without restarting the host.
#[derive(Default)]
struct KernelEntry {
    ptx: OnceLock<Result<Vec<u8>, String>>,
    kernel: Mutex<Option<Arc<Kernel>>>,
}
impl KernelEntry {
    fn get(&self, context: usize, source: impl FnOnce() -> String) -> Result<Arc<Kernel>, String> {
        let ptx = self
            .ptx
            .get_or_init(|| compile(&source()))
            .as_ref()
            .map_err(Clone::clone)?;
        let mut kernel = self.kernel.lock();
        if kernel.is_none() {
            *kernel = Some(Arc::new(Kernel::load(context, ptx)?));
        }
        Ok(kernel.as_ref().unwrap().clone())
    }
}

struct SharedCache<K, V>(OnceLock<Mutex<lru::LruCache<K, Arc<V>>>>);
impl<K: std::hash::Hash + Eq, V> SharedCache<K, V> {
    const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn get(&self, key: K, create: impl FnOnce() -> V) -> Arc<V> {
        let mut cache = self
            .0
            .get_or_init(|| Mutex::new(lru::LruCache::new(std::num::NonZeroUsize::new(8).unwrap())))
            .lock();
        if let Some(value) = cache.get(&key) {
            return value.clone();
        }
        let value = Arc::new(create());
        let evicted = cache.push(key, value.clone());
        drop(cache);
        // CUDA teardown can wait for queued work; never hold the lookup lock here.
        drop(evicted);
        value
    }
}

type Key = (usize, i32, i32, &'static str, &'static str);
pub fn process(
    buffers: &Buffers,
    itm: &FrameTransform,
    model: &DistortionModel,
    digital: Option<&DistortionModel>,
    drawing: &[u8],
) -> Result<(), String> {
    static KERNELS: SharedCache<Key, KernelEntry> = SharedCache::new();
    static STREAMS: SharedCache<(usize, usize), Mutex<StreamState>> = SharedCache::new();
    if buffers.input.size.0 == 0
        || buffers.input.size.1 < 4
        || buffers.output.size.0 == 0
        || buffers.output.size.1 < 4
        || buffers.output.size.0 > i32::MAX as usize
        || buffers.output.size.1 > i32::MAX as usize
        || buffers.input.size.2 < buffers.input.size.0.saturating_mul(16)
        || buffers.output.size.2 < buffers.output.size.0.saturating_mul(16)
        || buffers.input.size.2 > i32::MAX as usize
        || buffers.output.size.2 > i32::MAX as usize
        || buffers.input.size.2 % 16 != 0
        || buffers.output.size.2 % 16 != 0
    {
        return Err("unsupported native CUDA buffer geometry".into());
    }
    let d = driver()?;
    let mut context = 0;
    unsafe {
        check((d.cuCtxGetCurrent)(&mut context), "current context")?;
    }
    if context == 0 {
        return Err("no current CUDA context".into());
    }
    let (src, dst) = match (&buffers.input.data, &buffers.output.data) {
        (BufferSource::CUDABuffer { buffer: src }, BufferSource::CUDABuffer { buffer: dst }) => {
            (*src as u64, *dst as u64)
        }
        _ => return Err("native CUDA requires CUDA buffers".into()),
    };
    if src == 0 || dst == 0 || src == dst {
        return Err("invalid or aliasing CUDA buffers".into());
    }
    let stream = HOST_STREAM
        .with(|s| s.get())
        .ok_or("host stream unavailable")?;
    let key = (
        context,
        itm.kernel_params.flags & !4,
        itm.kernel_params.interpolation,
        model.id(),
        digital.map(|m| m.id()).unwrap_or(""),
    );
    let entry = KERNELS.get(key, KernelEntry::default);
    let kernel = entry.get(context, || {
        shader_source(&itm.kernel_params, model, digital)
    })?;
    let stream_state = STREAMS.get((context, stream), || {
        Mutex::new(StreamState::new(context, stream))
    });
    stream_state.lock().render(
        kernel,
        src,
        dst,
        (buffers.output.size.0, buffers.output.size.1),
        itm,
        drawing,
        stream,
    )
}

#[cfg(test)]
#[path = "cuda_native_tests.rs"]
mod tests;
