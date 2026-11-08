// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::{
    gpu::BufferDescription,
    stabilization::{KernelParams, RGBA8, RGBAf},
};

#[test]
fn host_stream_scope_restores_and_stays_on_the_render_thread() {
    assert_eq!(HOST_STREAM.with(Cell::get), None);
    {
        let _outer = HostStreamGuard::new(Some(12));
        {
            let _inner = HostStreamGuard::new(Some(34));
            assert_eq!(HOST_STREAM.with(Cell::get), Some(34));
        }
        assert_eq!(HOST_STREAM.with(Cell::get), Some(12));
        assert_eq!(
            std::thread::spawn(|| HOST_STREAM.with(Cell::get))
                .join()
                .unwrap(),
            None
        );
    }
    assert_eq!(HOST_STREAM.with(Cell::get), None);
}

#[test]
fn native_cuda_requires_host_stream_and_float_cuda_images() {
    let mut buffers = Buffers {
        input: BufferDescription {
            data: BufferSource::CUDABuffer {
                buffer: 16usize as *mut c_void,
            },
            ..Default::default()
        },
        output: BufferDescription {
            data: BufferSource::CUDABuffer {
                buffer: 32usize as *mut c_void,
            },
            ..Default::default()
        },
    };
    assert!(!enabled_for::<RGBAf>(&buffers));
    let _scope = HostStreamGuard::new(Some(0));
    assert_eq!(enabled_for::<RGBAf>(&buffers), configured());
    assert!(!enabled_for::<RGBA8>(&buffers));
    buffers.output = BufferDescription::default();
    assert!(!enabled_for::<RGBAf>(&buffers));
}

#[test]
fn cache_lookup_does_not_wait_for_another_stream() {
    let cache = SharedCache::<usize, Mutex<()>>::new();
    let first = cache.get(1, || Mutex::new(()));
    let _busy = first.lock();
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        let cache = &cache;
        scope.spawn(move || {
            let second = cache.get(2, || Mutex::new(()));
            let _ready = second.lock();
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    });
}

#[test]
fn eviction_releases_resources_outside_the_lookup_lock() {
    struct Resource(std::sync::Weak<SharedCache<usize, Resource>>);
    impl Drop for Resource {
        fn drop(&mut self) {
            if let Some(cache) = self.0.upgrade() {
                assert!(cache.0.get().unwrap().try_lock().is_some());
            }
        }
    }
    // Explicitly empty the cache before its owner is dropped.
    let cache = Arc::new(SharedCache::new());
    for key in 0..10 {
        cache.get(key, || Resource(Arc::downgrade(&cache)));
    }
    let old = {
        let mut lock = cache.0.get().unwrap().lock();
        std::mem::replace(
            &mut *lock,
            lru::LruCache::new(std::num::NonZeroUsize::new(8).unwrap()),
        )
    };
    drop(old);
}

api!(TestDriver {
    cuInit(flags: u32) -> i32;
    cuDeviceGet(device: *mut i32, ordinal: i32) -> i32;
    cuCtxCreate_v2(context: *mut usize, flags: u32, device: i32) -> i32;
    cuCtxDestroy_v2(context: usize) -> i32;
    cuStreamCreate(stream: *mut usize, flags: u32) -> i32;
    cuStreamDestroy_v2(stream: usize) -> i32;
    cuMemcpyHtoD_v2(dst: u64, src: *const c_void, bytes: usize) -> i32;
    cuMemcpyDtoH_v2(dst: *mut c_void, src: u64, bytes: usize) -> i32;
    cuCtxSynchronize() -> i32;
});

struct Gpu {
    api: TestDriver,
    context: usize,
    streams: [usize; 2],
    allocations: Vec<u64>,
}
impl Gpu {
    fn new() -> Self {
        let api = unsafe {
            TestDriver::load(
                Library::new(if cfg!(windows) {
                    "nvcuda.dll"
                } else {
                    "libcuda.so.1"
                })
                .unwrap(),
            )
            .unwrap()
        };
        let mut context = 0;
        let mut device = 0;
        let mut streams = [0; 2];
        unsafe {
            check((api.cuInit)(0), "init").unwrap();
            check((api.cuDeviceGet)(&mut device, 0), "device").unwrap();
            check(
                (api.cuCtxCreate_v2)(&mut context, 0, device),
                "test context",
            )
            .unwrap();
            for stream in &mut streams {
                check((api.cuStreamCreate)(stream, 1), "test stream").unwrap();
            }
        }
        Self {
            api,
            context,
            streams,
            allocations: Vec::new(),
        }
    }
    fn upload(&mut self, values: &[f32]) -> u64 {
        let mut ptr = 0;
        unsafe {
            check(
                (driver().unwrap().cuMemAlloc_v2)(&mut ptr, std::mem::size_of_val(values)),
                "test allocation",
            )
            .unwrap();
            self.allocations.push(ptr);
            check(
                (self.api.cuMemcpyHtoD_v2)(
                    ptr,
                    values.as_ptr().cast(),
                    std::mem::size_of_val(values),
                ),
                "test upload",
            )
            .unwrap();
        }
        ptr
    }
    fn read(&self, ptr: u64, count: usize) -> Vec<f32> {
        let mut values = vec![0.0; count];
        unsafe {
            check((self.api.cuCtxSynchronize)(), "test completion").unwrap();
            check(
                (self.api.cuMemcpyDtoH_v2)(values.as_mut_ptr().cast(), ptr, count * 4),
                "test readback",
            )
            .unwrap();
        }
        values
    }
}
impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            (self.api.cuCtxSynchronize)();
            for ptr in &self.allocations {
                (driver().unwrap().cuMemFree_v2)(*ptr);
            }
            for stream in self.streams {
                (self.api.cuStreamDestroy_v2)(stream);
            }
            (self.api.cuCtxDestroy_v2)(self.context);
        }
    }
}

fn identity_frame(width: usize, height: usize, stride: usize) -> FrameTransform {
    FrameTransform {
        kernel_params: KernelParams {
            width: width as i32,
            height: height as i32,
            stride: stride as i32,
            output_width: width as i32,
            output_height: height as i32,
            output_stride: stride as i32,
            matrix_count: 1,
            interpolation: 8,
            bytes_per_pixel: 16,
            pix_element_count: 4,
            f: [1.0; 2],
            fov: 1.0,
            lens_correction_amount: 1.0,
            input_horizontal_stretch: 1.0,
            input_vertical_stretch: 1.0,
            canvas_scale: 1.0,
            source_rect: [0, 0, width as i32, height as i32],
            output_rect: [0, 0, width as i32, height as i32],
            safe_area_rect: [0.0, 0.0, width as f32, height as f32],
            max_pixel_value: 1.0,
            pixel_value_limit: 1e10,
            light_refraction_coefficient: 1.0,
            ..Default::default()
        },
        matrices: vec![[1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 0., 0.]],
        ..Default::default()
    }
}

fn cuda_buffers(
    src: u64,
    dst: u64,
    width: usize,
    height: usize,
    stride: usize,
) -> Buffers<'static> {
    Buffers {
        input: BufferDescription {
            size: (width, height, stride),
            data: BufferSource::CUDABuffer {
                buffer: src as *mut c_void,
            },
            ..Default::default()
        },
        output: BufferDescription {
            size: (width, height, stride),
            data: BufferSource::CUDABuffer {
                buffer: dst as *mut c_void,
            },
            ..Default::default()
        },
    }
}

#[test]
#[ignore = "requires an NVIDIA GPU and NVRTC in the library search path"]
fn native_cuda_gpu_pixels_stream_reuse_failure_recovery_and_lazy_backend() {
    let mut gpu = Gpu::new();
    let (width, height, pitch) = (65, 49, 81);
    let source: Vec<f32> = (0..pitch * height * 4)
        .map(|i| (i % 127) as f32 / 127.0)
        .collect();
    let src = gpu.upload(&source);
    let sentinel = vec![-99.0f32; source.len()];
    let dst = gpu.upload(&sentinel);
    let model = DistortionModel::from_name("opencv_standard");
    let mut frame = identity_frame(width, height, pitch * 16);
    let mut buffers = cuda_buffers(src, dst, width, height, pitch * 16);
    let _scope = HostStreamGuard::new(Some(gpu.streams[0]));
    process(&buffers, &frame, &model, None, &[]).unwrap();
    let actual = gpu.read(dst, source.len());
    for y in 8..height - 8 {
        for x in 8..width - 8 {
            for c in 0..4 {
                let i = (y * pitch + x) * 4 + c;
                assert!((actual[i] - source[i]).abs() < 1e-6, "identity {x},{y},{c}");
            }
        }
    }
    for y in 0..height {
        assert!(
            actual[(y * pitch + width) * 4..(y + 1) * pitch * 4]
                .iter()
                .all(|v| *v == -99.0)
        );
    }

    // Each output uses a different parameter block; enqueue beyond the ring's
    // capacity on two streams before reading any result.
    // Allocate before enqueueing: CUDA allocation itself may synchronize work.
    let outputs: Vec<_> = (0..24)
        .map(|index| (gpu.upload(&sentinel), [index as f32 / 24.0, 0.25, 0.5, 1.0]))
        .collect();
    std::thread::scope(|scope| {
        for stream_index in 0..2 {
            let outputs = &outputs;
            let model = &model;
            let mut frame = frame.clone();
            let context = gpu.context;
            let stream = gpu.streams[stream_index];
            scope.spawn(move || {
                let d = driver().unwrap();
                unsafe {
                    check((d.cuCtxPushCurrent_v2)(context), "worker context").unwrap();
                }
                let _scope = HostStreamGuard::new(Some(stream));
                for (dst, color) in outputs.iter().skip(stream_index).step_by(2) {
                    frame.kernel_params.flags = 4;
                    frame.kernel_params.background = *color;
                    process(
                        &cuda_buffers(src, *dst, width, height, pitch * 16),
                        &frame,
                        model,
                        None,
                        &[],
                    )
                    .unwrap();
                }
                let mut previous = 0;
                unsafe {
                    check(
                        (d.cuCtxPopCurrent_v2)(&mut previous),
                        "worker context restore",
                    )
                    .unwrap();
                }
            });
        }
    });
    for (dst, expected) in outputs {
        let actual = gpu.read(dst, source.len());
        for y in 0..height {
            for x in 0..pitch {
                for c in 0..4 {
                    let value = if x < width { expected[c] } else { -99.0 };
                    assert_eq!(actual[(y * pitch + x) * 4 + c], value);
                }
            }
        }
    }

    frame.kernel_params.flags = 4;
    frame.kernel_params.background = [23.0 / 24.0, 0.25, 0.5, 1.0];

    // A real rejected CUDA launch must not poison the stream's next submission.
    let entry = KernelEntry::default();
    let kernel = entry
        .get(gpu.context, || {
            shader_source(&frame.kernel_params, &model, None)
        })
        .unwrap();
    let mut state = StreamState::new(gpu.context, gpu.streams[0]);
    let invalid = Arc::new(Kernel {
        context: gpu.context,
        module: 0,
        function: 0,
    });
    assert!(
        state
            .render(
                invalid,
                src,
                dst,
                (width, height),
                &frame,
                &[],
                gpu.streams[0]
            )
            .is_err()
    );
    state
        .render(
            kernel,
            src,
            dst,
            (width, height),
            &frame,
            &[],
            gpu.streams[0],
        )
        .unwrap();
    assert_eq!(gpu.read(dst, source.len())[0], 23.0 / 24.0);

    let mut stab = crate::stabilization::Stabilization::default();
    stab.size = (width, height);
    stab.output_size = (width, height);
    stab.cache_frame_transform = true;
    stab.stab_data.insert(0, frame);
    let manager = crate::StabilizationManager::default();
    manager.params.write().fps = 60.0;
    *manager.stabilization.write() = stab;
    let result = manager
        .process_pixels::<RGBAf>(0, Some(0), &mut buffers)
        .unwrap();
    assert_eq!(result.backend, "CUDA");
    let stab = manager.stabilization.read();
    assert!(stab.wgpu.is_none());
    assert!(stab.initialized_backend.is_none());
}

#[test]
#[ignore = "requires an NVIDIA GPU and NVRTC in the library search path"]
fn native_cuda_gpu_compiles_production_models_and_flag_specializations() {
    let _gpu = Gpu::new();
    let mut params = identity_frame(65, 49, 81 * 16).kernel_params;
    // Exercise the actual production generator, including its flags specialization.
    for model in [
        "opencv_fisheye",
        "opencv_standard",
        "poly3",
        "poly5",
        "ptlens",
        "insta360",
        "sony",
    ] {
        for interpolation in [2, 4, 8, 10, 11, 12, 13] {
            params.interpolation = interpolation;
            params.flags = 224 | 256 | 512 | 1024 | 2048 | 4096 | 8192;
            compile(&shader_source(
                &params,
                &DistortionModel::from_name(model),
                None,
            ))
            .unwrap();
        }
    }
    for model in [
        "digital_stretch",
        "gopro_superview",
        "gopro6_superview",
        "gopro_hyperview",
    ] {
        for interpolation in [2, 8, 10] {
            params.interpolation = interpolation;
            params.flags = 2 | 32 | 64;
            compile(&shader_source(
                &params,
                &DistortionModel::default(),
                Some(&DistortionModel::from_name(model)),
            ))
            .unwrap();
        }
    }
}

#[test]
#[ignore = "requires NVIDIA CUDA/Vulkan interop and NVRTC; reports a synthetic benchmark"]
fn native_cuda_gpu_compare_legacy_pixels_and_throughput() {
    let mut gpu = Gpu::new();
    let (width, height) = (3840, 2160);
    let source: Vec<f32> = (0..width * height * 4)
        .map(|i| (i % 127) as f32 / 127.0)
        .collect();
    let src = gpu.upload(&source);
    let dst = gpu.upload(&vec![0.0; source.len()]);
    let mut buffers = cuda_buffers(src, dst, width, height, width * 16);
    buffers.input.texture_copy = true;
    buffers.output.texture_copy = true;
    let mut frame = identity_frame(width, height, width * 16);
    let model = DistortionModel::from_name("opencv_standard");
    let legacy = crate::gpu::wgpu::WgpuWrapper::new(
        &frame.kernel_params,
        RGBAf::wgpu_format().unwrap(),
        model.clone(),
        None,
        &buffers,
        0,
    )
    .unwrap();
    assert!(legacy.undistort_image(&mut buffers, &frame, &[]));
    let reference = gpu.read(dst, source.len());
    let _scope = HostStreamGuard::new(Some(gpu.streams[0]));
    process(&buffers, &frame, &model, None, &[]).unwrap();
    let native = gpu.read(dst, source.len());
    let max_error = native
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    let worst = native
        .iter()
        .zip(&reference)
        .enumerate()
        .max_by(|(_, (a, b)), (_, (c, d))| (*a - *b).abs().total_cmp(&(*c - *d).abs()))
        .unwrap()
        .0;
    let native_error = native
        .iter()
        .zip(&source)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    let legacy_error = reference
        .iter()
        .zip(&source)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    eprintln!(
        "worst x={} y={} c={} native={} legacy={} input={} native_input_error={} legacy_input_error={}",
        worst / 4 % width,
        worst / 4 / width,
        worst % 4,
        native[worst],
        reference[worst],
        source[worst],
        native_error,
        legacy_error
    );
    assert!(native.iter().all(|v| v.is_finite()));
    assert!(max_error < 1e-6, "identity CUDA/wgpu max error {max_error}");
    eprintln!("CUDA/wgpu 3840x2160 identity max error: {max_error}");
    for shift in [-1.0 / 64.0, 1.0 / 64.0] {
        frame.kernel_params.translation2d = [shift, shift];
        assert!(legacy.undistort_image(&mut buffers, &frame, &[]));
        let reference = gpu.read(dst, source.len());
        process(&buffers, &frame, &model, None, &[]).unwrap();
        let native = gpu.read(dst, source.len());
        let error = native
            .iter()
            .zip(&reference)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(native.iter().all(|v| v.is_finite()));
        assert!(
            error < 1e-6,
            "halfway phase {shift}: CUDA/wgpu max error {error}"
        );
        eprintln!("CUDA/wgpu halfway phase {shift} max error: {error}");
    }
    frame.kernel_params.translation2d = [0.0; 2];
    for native in [false, true] {
        for _ in 0..5 {
            if native {
                process(&buffers, &frame, &model, None, &[]).unwrap();
            } else {
                assert!(legacy.undistort_image(&mut buffers, &frame, &[]));
            }
        }
        unsafe {
            check((gpu.api.cuCtxSynchronize)(), "warmup completion").unwrap();
        }
    }
    for round in 0..4 {
        for native in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let start = std::time::Instant::now();
            for _ in 0..30 {
                if native {
                    process(&buffers, &frame, &model, None, &[]).unwrap();
                } else {
                    assert!(legacy.undistort_image(&mut buffers, &frame, &[]));
                }
            }
            unsafe {
                check((gpu.api.cuCtxSynchronize)(), "benchmark completion").unwrap();
            }
            eprintln!(
                "synthetic round={round} native={native} ms_per_frame={:.4}",
                start.elapsed().as_secs_f64() * 1000.0 / 30.0
            );
        }
    }
}

#[test]
fn lanczos_interior_preserves_cuda_shared_weights_and_fma() {
    let params = KernelParams { interpolation: 8, ..Default::default() };
    let source = shader_source(&params, &DistortionModel::default(), None);
    assert!(!source.contains("coeffs_x[xp]"));
    assert!(!source.contains("coeffs_y[yp]"));
    assert_eq!(source.matches("xsum = color_mad(srcpx, weights[xp*32+(sx0 & 31)], xsum);").count(), 2);
    assert_eq!(source.matches("sum = color_mad(xsum, weights[yp*32+(sy0 & 31)], sum);").count(), 2);
    assert!(source.find("__syncthreads();").unwrap() < source.find("if (buf_x >= buf_width").unwrap());
}
