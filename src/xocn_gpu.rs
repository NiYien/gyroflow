// SPDX-License-Identifier: GPL-3.0-or-later
//! Integer X-OCN entropy decoding and reconstruction on the hardware compute backend.

use std::ffi::c_void;
use std::sync::{
    Arc, Mutex, Once,
    atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
};
use wgpu::util::DeviceExt;

#[repr(C)]
pub struct Plane {
    data: *const i32,
    rows: u32,
    cols: u32,
}

#[repr(C)]
pub struct Record {
    values: [u32; 24],
}

#[repr(C)]
pub struct PlaneLayout {
    offset: u32,
    rows: u32,
    cols: u32,
    reserved: u32,
}

#[repr(C)]
pub struct Compressed {
    bytes: *const u8,
    length: usize,
    records: *const Record,
    record_count: u32,
    planes: *const PlaneLayout,
    before_wait: Option<unsafe extern "C" fn(*mut c_void)>,
    before_wait_user: *mut c_void,
}

#[repr(C)]
pub struct Image {
    raw_width: u32,
    raw_height: u32,
    crop_x: u32,
    crop_y: u32,
    width: u32,
    height: u32,
    output_width: u32,
    output_height: u32,
    stride: u32,
    output_mode: u32,
}

type Create = unsafe extern "C" fn() -> *mut c_void;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Reconstruct = unsafe extern "C" fn(
    *mut c_void,
    *const Plane,
    *const Image,
    *const u16,
    *const u8,
    *mut u8,
) -> bool;
type Decode = unsafe extern "C" fn(
    *mut c_void,
    *const Compressed,
    *const Image,
    *const u16,
    *const u8,
    *mut u8,
) -> bool;

unsafe extern "C" {
    fn niyien_xocn_register_gpu(
        create: Option<Create>,
        destroy: Option<Destroy>,
        reconstruct: Option<Reconstruct>,
        decode: Option<Decode>,
    );
}

pub fn register() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| unsafe {
        niyien_xocn_register_gpu(Some(create), Some(destroy), Some(reconstruct), Some(decode));
    });
}

static SUCCESSFUL_FRAMES: AtomicU64 = AtomicU64::new(0);

pub fn successful_frames() -> u64 {
    SUCCESSFUL_FRAMES.load(Relaxed)
}

struct Shared {
    name: String,
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    inverse: wgpu::ComputePipeline,
    green: wgpu::ComputePipeline,
    merge: wgpu::ComputePipeline,
    entropy: wgpu::ComputePipeline,
    failed: Arc<AtomicBool>,
}

impl Shared {
    fn get() -> Result<Arc<Self>, String> {
        static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
        let mut shared = SHARED.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(context) = shared
            .as_ref()
            .filter(|context| !context.failed.load(Relaxed))
        {
            return Ok(context.clone());
        }
        let started = std::time::Instant::now();
        let adapter = gyroflow_core::gpu::wgpu::WgpuWrapper::compute_adapter(None)
            .ok_or("No hardware compute adapter")?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu
            || !adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        {
            return Err(format!("{} has no hardware compute support", info.name));
        }
        let available = adapter.limits();
        let mut limits = wgpu::Limits::downlevel_defaults();
        limits.max_buffer_size = available.max_buffer_size;
        limits.max_storage_buffer_binding_size = available.max_storage_buffer_binding_size;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("X-OCN reconstruction"),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        let failed = Arc::new(AtomicBool::new(false));
        let failure = failed.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            log::error!(target: "video.codec", "X-OCN GPU: {error}");
            failure.store(true, Relaxed);
        }));
        let failure = failed.clone();
        device.set_device_lost_callback(move |reason, message| {
            if reason == wgpu::DeviceLostReason::Unknown {
                log::error!(target: "video.codec", "X-OCN GPU device lost: {message}");
            }
            failure.store(true, Relaxed);
        });
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::Internal,
            wgpu::ErrorFilter::OutOfMemory,
        ]
        .map(|filter| device.push_error_scope(filter));
        let uniform_size = std::mem::size_of::<[u32; 28]>() as u64;
        let mut entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(uniform_size),
            },
            count: None,
        }];
        for binding in 1..=4 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: binding == 1 || binding == 3,
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("X-OCN"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("X-OCN"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("X-OCN integer reconstruction"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("rendering/xocn_reconstruct.wgsl").into(),
            ),
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let inverse = pipeline("inverse53");
        let green = pipeline("green");
        let merge = pipeline("merge");
        let entropy_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("X-OCN LLVC3 entropy"),
            source: wgpu::ShaderSource::Wgsl(include_str!("rendering/xocn_entropy.wgsl").into()),
        });
        let entropy = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("X-OCN entropy"),
            layout: None,
            module: &entropy_shader,
            entry_point: Some("entropy"),
            compilation_options: Default::default(),
            cache: None,
        });
        let errors: Vec<_> = scopes
            .into_iter()
            .rev()
            .filter_map(|scope| pollster::block_on(scope.pop()))
            .collect();
        if let Some(error) = errors.first() {
            return Err(error.to_string());
        }
        if failed.load(Relaxed) {
            return Err("GPU initialization failed".into());
        }
        log::info!(target: "video.codec", "X-OCN GPU initialized: {} ({:?}), device_type={:?} initialization_ms={:.2}", info.name, info.backend, info.device_type, started.elapsed().as_secs_f64() * 1000.0);
        let result = Arc::new(Self {
            name: format!("{} ({:?})", info.name, info.backend),
            device,
            queue,
            layout,
            inverse,
            green,
            merge,
            entropy,
            failed,
        });
        *shared = Some(result.clone());
        Ok(result)
    }
}

struct Resources {
    coefficients: wgpu::Buffer,
    _work: wgpu::Buffer,
    parameters: wgpu::Buffer,
    _tables: wgpu::Buffer,
    output: wgpu::Buffer,
    readback: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    key: (u32, u32, u32, u32, u32),
    encoded: Option<EncodedResources>,
}

struct EncodedResources {
    payload: wgpu::Buffer,
    records: wgpu::Buffer,
    parameters: wgpu::Buffer,
    statuses: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    payload_capacity: usize,
    record_capacity: u32,
}

impl Resources {
    fn prepare_encoded(
        &mut self,
        shared: &Shared,
        input: &Compressed,
        output_bytes: u64,
    ) -> Result<(), String> {
        if self.encoded.as_ref().is_some_and(|encoded| {
            encoded.payload_capacity >= input.length.div_ceil(4) * 4
                && encoded.record_capacity >= input.record_count
        }) {
            return Ok(());
        }
        let payload_capacity = input.length.next_power_of_two().max(
            self.encoded
                .as_ref()
                .map_or(4, |encoded| encoded.payload_capacity),
        );
        let record_capacity = input.record_count.next_power_of_two().max(
            self.encoded
                .as_ref()
                .map_or(1, |encoded| encoded.record_capacity),
        );
        let record_bytes = u64::from(record_capacity) * std::mem::size_of::<Record>() as u64;
        let limit = u64::from(shared.device.limits().max_storage_buffer_binding_size);
        if payload_capacity as u64 > limit || record_bytes > limit {
            return Err(format!(
                "Compressed buffers exceed GPU binding limit {limit}"
            ));
        }
        let buffer = |label, size, usage| {
            shared.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let payload = buffer(
            "X-OCN compressed picture",
            payload_capacity as u64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let records = buffer(
            "X-OCN compressed records",
            record_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let parameters = buffer(
            "X-OCN entropy parameters",
            16,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let statuses = buffer(
            "X-OCN record status",
            u64::from(record_capacity) * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let bindings = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("X-OCN entropy"),
            layout: &shared.entropy.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: payload.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: records.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.coefficients.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: statuses.as_entire_binding(),
                },
            ],
        });
        self.readback = buffer(
            "X-OCN preview and entropy status",
            output_bytes + u64::from(record_capacity) * 4,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        self.encoded = Some(EncodedResources {
            payload,
            records,
            parameters,
            statuses,
            bindings,
            payload_capacity,
            record_capacity,
        });
        Ok(())
    }
}

struct Context {
    shared: Arc<Shared>,
    resources: Option<Resources>,
    packed: Vec<i32>,
    logged: bool,
}

#[derive(Clone, Copy)]
struct Band {
    offset: u32,
    rows: u32,
    cols: u32,
    source: u32,
}

struct Dispatch {
    entry: u32,
    parameters: [u32; 28],
}

enum Coefficients<'a> {
    Planes(&'a [Plane]),
    Compressed(&'a Compressed),
}

unsafe extern "C" fn create() -> *mut c_void {
    let result = std::panic::catch_unwind(Shared::get);
    match result {
        Ok(Ok(shared)) => Box::into_raw(Box::new(Context {
            shared,
            resources: None,
            packed: Vec::new(),
            logged: false,
        }))
        .cast(),
        Ok(Err(error)) => {
            log::warn!(target: "video.codec", "X-OCN GPU unavailable, using CPU: {error}");
            std::ptr::null_mut()
        }
        Err(_) => {
            log::warn!(target: "video.codec", "X-OCN GPU initialization panicked, using CPU");
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    if !context.is_null() {
        drop(unsafe { Box::from_raw(context.cast::<Context>()) });
    }
}

unsafe extern "C" fn reconstruct(
    context: *mut c_void,
    planes: *const Plane,
    image: *const Image,
    linear: *const u16,
    tone: *const u8,
    output: *mut u8,
) -> bool {
    unsafe {
        run_callback(
            context,
            Coefficients::Planes(std::slice::from_raw_parts(planes, 31)),
            image,
            linear,
            tone,
            output,
        )
    }
}

unsafe extern "C" fn decode(
    context: *mut c_void,
    compressed: *const Compressed,
    image: *const Image,
    linear: *const u16,
    tone: *const u8,
    output: *mut u8,
) -> bool {
    unsafe {
        run_callback(
            context,
            Coefficients::Compressed(&*compressed),
            image,
            linear,
            tone,
            output,
        )
    }
}

unsafe fn run_callback(
    context: *mut c_void,
    coefficients: Coefficients<'_>,
    image: *const Image,
    linear: *const u16,
    tone: *const u8,
    output: *mut u8,
) -> bool {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (&mut *context.cast::<Context>()).run(
            coefficients,
            &*image,
            std::slice::from_raw_parts(linear, 4096),
            std::slice::from_raw_parts(tone, 65536),
            output,
        )
    }));
    match result {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            log::warn!(target: "video.codec", "X-OCN GPU reconstruction failed, using CPU: {error}");
            false
        }
        Err(_) => {
            log::warn!(target: "video.codec", "X-OCN GPU reconstruction panicked, using CPU");
            false
        }
    }
}

impl Context {
    unsafe fn run(
        &mut self,
        coefficients: Coefficients<'_>,
        image: &Image,
        linear: &[u16],
        tone: &[u8],
        output: *mut u8,
    ) -> Result<(), String> {
        let started = std::time::Instant::now();
        let shared = &self.shared;
        if shared.failed.load(Relaxed) {
            return Err("GPU device is unavailable".into());
        }
        self.packed.clear();
        let mut bands = Vec::with_capacity(31);
        let compressed = match coefficients {
            Coefficients::Planes(planes) => {
                for plane in planes {
                    bands.push(Band {
                        offset: self.packed.len() as u32,
                        rows: plane.rows,
                        cols: plane.cols,
                        source: 0,
                    });
                    self.packed.extend_from_slice(unsafe {
                        std::slice::from_raw_parts(
                            plane.data,
                            plane.rows as usize * plane.cols as usize,
                        )
                    });
                }
                None
            }
            Coefficients::Compressed(input) => {
                for plane in unsafe { std::slice::from_raw_parts(input.planes, 31) } {
                    bands.push(Band {
                        offset: plane.offset,
                        rows: plane.rows,
                        cols: plane.cols,
                        source: 0,
                    });
                }
                Some(input)
            }
        };
        let width = image.raw_width / 2;
        let height = image.raw_height / 2;
        let samples = width * height;
        let raw = image.output_mode == 1;
        let output_width = if raw {
            image.raw_width
        } else {
            image.output_width
        };
        let output_height = if raw {
            image.raw_height
        } else {
            image.output_height
        };
        let row_bytes = output_width * if raw { 2 } else { 4 };
        let output_bytes = u64::from(row_bytes) * u64::from(output_height);
        let key = (
            image.raw_width,
            image.raw_height,
            output_width,
            output_height,
            image.output_mode,
        );
        if self
            .resources
            .as_ref()
            .is_none_or(|resources| resources.key != key)
        {
            let coefficient_bytes =
                u64::from(bands[30].offset + bands[30].rows * bands[30].cols) * 4;
            let work_bytes = u64::from(samples) * 4 * 4;
            let limit = u64::from(shared.device.limits().max_storage_buffer_binding_size);
            if coefficient_bytes > limit || work_bytes > limit || output_bytes > limit {
                return Err(format!(
                    "Reconstruction buffers exceed GPU binding limit {limit}"
                ));
            }
            let buffer = |label, size, usage| {
                shared.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage,
                    mapped_at_creation: false,
                })
            };
            let coefficients = buffer(
                "X-OCN coefficients",
                coefficient_bytes,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            );
            let work = buffer(
                "X-OCN reconstructed components",
                work_bytes,
                wgpu::BufferUsages::STORAGE,
            );
            let parameters = buffer(
                "X-OCN dispatch parameters",
                29 * 256,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            );
            let values: Vec<u32> = linear
                .iter()
                .map(|value| u32::from(*value))
                .chain(tone.iter().map(|value| u32::from(*value)))
                .collect();
            let tables = shared
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("X-OCN linear and preview curves"),
                    contents: bytemuck::cast_slice(&values),
                    usage: wgpu::BufferUsages::STORAGE,
                });
            let gpu_output = buffer(
                "X-OCN output",
                output_bytes,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            );
            let readback = buffer(
                "X-OCN readback",
                output_bytes,
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            );
            let bindings = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("X-OCN"),
                layout: &shared.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &parameters,
                            offset: 0,
                            size: wgpu::BufferSize::new(112),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: coefficients.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: work.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: tables.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: gpu_output.as_entire_binding(),
                    },
                ],
            });
            self.resources = Some(Resources {
                coefficients,
                _work: work,
                parameters,
                _tables: tables,
                output: gpu_output,
                readback,
                bindings,
                key,
                encoded: None,
            });
        }
        let resources = self.resources.as_mut().unwrap();
        if let Some(input) = compressed {
            resources.prepare_encoded(shared, input, output_bytes)?;
        }
        let mut encoder = shared
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("X-OCN reconstruction"),
            });
        let mut dispatches = Vec::with_capacity(29);
        let top = 4 + (8 - (image.raw_height / 4) % 8) % 8;
        for component in 0..3usize {
            let first = bands[component * 10];
            let offset = component as u32 * samples;
            let mut current = first;
            for group in 1..=3usize {
                let band0 = bands[component * 10 + 1 + (group - 1) * 3];
                let band1 = bands[component * 10 + 2 + (group - 1) * 3];
                let band2 = bands[component * 10 + 3 + (group - 1) * 3];
                let step = 1 << (4 - group);
                let high_first = top.div_ceil(step / 2) & 1;
                let low = inverse_dispatch(
                    &mut dispatches,
                    current,
                    band1,
                    true,
                    high_first,
                    3 * samples,
                );
                let high = inverse_dispatch(
                    &mut dispatches,
                    band0,
                    band2,
                    true,
                    high_first,
                    3 * samples + low.rows * low.cols,
                );
                current = inverse_dispatch(&mut dispatches, low, high, false, 0, offset);
            }
        }
        let mut parameters = [0u32; 28];
        parameters[0] = width;
        parameters[1] = height;
        parameters[13] = image.raw_width;
        parameters[14] = image.raw_height;
        parameters[15] = image.crop_x;
        parameters[16] = image.crop_y;
        parameters[17] = image.width;
        parameters[18] = image.height;
        parameters[19] = 0;
        parameters[20] = samples;
        parameters[21] = samples * 2;
        parameters[22] = samples * 3;
        parameters[23] = bands[30].offset;
        parameters[24] = image.output_mode;
        dispatches.push(Dispatch {
            entry: 1,
            parameters,
        });
        parameters[0] = if raw { width } else { output_width };
        parameters[1] = if raw { height } else { output_height };
        dispatches.push(Dispatch {
            entry: 2,
            parameters,
        });
        let mut uniforms = vec![0u32; dispatches.len() * 64];
        for (index, dispatch) in dispatches.iter().enumerate() {
            uniforms[index * 64..index * 64 + 28].copy_from_slice(&dispatch.parameters);
        }
        if let Some(input) = compressed {
            let encoded = resources.encoded.as_ref().unwrap();
            let padded = input.length.div_ceil(4) * 4;
            {
                let mut staging = shared
                    .queue
                    .write_buffer_with(
                        &encoded.payload,
                        0,
                        wgpu::BufferSize::new(padded as u64).unwrap(),
                    )
                    .ok_or("Compressed upload allocation failed")?;
                staging.slice(..input.length).copy_from_slice(unsafe {
                    std::slice::from_raw_parts(input.bytes, input.length)
                });
                staging.slice(input.length..padded).fill(0);
            }
            if input.record_count != 0 {
                shared.queue.write_buffer(&encoded.records, 0, unsafe {
                    std::slice::from_raw_parts(
                        input.records.cast::<u8>(),
                        input.record_count as usize * std::mem::size_of::<Record>(),
                    )
                });
            }
            shared.queue.write_buffer(
                &encoded.parameters,
                0,
                bytemuck::cast_slice(&[
                    input.record_count,
                    (resources.coefficients.size() / 4) as u32,
                    input.length as u32,
                    0,
                ]),
            );
            // CPU planes start as zero too; absent/empty records must not retain an older frame.
            encoder.clear_buffer(&resources.coefficients, 0, None);
            if input.record_count != 0 {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                pass.set_pipeline(&shared.entropy);
                pass.set_bind_group(0, &encoded.bindings, &[]);
                pass.dispatch_workgroups(input.record_count.div_ceil(64), 1, 1);
            }
        } else {
            shared.queue.write_buffer(
                &resources.coefficients,
                0,
                bytemuck::cast_slice(&self.packed),
            );
        }
        shared
            .queue
            .write_buffer(&resources.parameters, 0, bytemuck::cast_slice(&uniforms));
        for (index, dispatch) in dispatches.iter().enumerate() {
            // Separate passes establish storage dependencies without CPU waits or readbacks.
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(match dispatch.entry {
                0 => &shared.inverse,
                1 => &shared.green,
                _ => &shared.merge,
            });
            pass.set_bind_group(0, &resources.bindings, &[(index * 256) as u32]);
            pass.dispatch_workgroups(
                dispatch.parameters[0].div_ceil(16),
                dispatch.parameters[1].div_ceil(16),
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&resources.output, 0, &resources.readback, 0, output_bytes);
        if let Some(input) = compressed.filter(|input| input.record_count != 0) {
            encoder.copy_buffer_to_buffer(
                &resources.encoded.as_ref().unwrap().statuses,
                0,
                &resources.readback,
                output_bytes,
                u64::from(input.record_count) * 4,
            );
        }
        let submission = shared.queue.submit([encoder.finish()]);
        struct Unmap<'a>(&'a wgpu::Buffer);
        impl Drop for Unmap<'_> {
            fn drop(&mut self) {
                self.0.unmap();
            }
        }
        let _unmap = Unmap(&resources.readback);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        resources
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let mut overlap_io_ms = 0.0;
        if let Some(input) = compressed {
            if let Some(before_wait) = input.before_wait {
                // The hook runs synchronously while the submitted GPU work is in flight.
                let io_started = std::time::Instant::now();
                unsafe {
                    before_wait(input.before_wait_user);
                }
                overlap_io_ms = io_started.elapsed().as_secs_f64() * 1000.0;
            }
        }
        shared
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv()
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if shared.failed.load(Relaxed) {
            return Err("GPU reported a reconstruction error".into());
        }
        let mapped = resources.readback.slice(..).get_mapped_range();
        if let Some(input) = compressed {
            for (record, bytes) in mapped
                [output_bytes as usize..output_bytes as usize + input.record_count as usize * 4]
                .chunks_exact(4)
                .enumerate()
            {
                let status = u32::from_le_bytes(bytes.try_into().unwrap());
                if status != 0 {
                    return Err(format!(
                        "Invalid entropy stream in record {record}: status={status}"
                    ));
                }
            }
        }
        for row in 0..output_height as usize {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    mapped.as_ptr().add(row * row_bytes as usize),
                    output.add(row * image.stride as usize),
                    row_bytes as usize,
                );
            }
        }
        SUCCESSFUL_FRAMES.fetch_add(1, Relaxed);
        if !self.logged {
            let source = if compressed.is_some() {
                "compressed"
            } else {
                "coefficients"
            };
            let input_bytes = compressed.map_or(self.packed.len() * 4, |input| input.length);
            // Frame elapsed time includes the synchronous hook, not just GPU execution.
            log::info!(target: "video.codec", "X-OCN GPU first frame complete: device={} input={} input_bytes={} mode={} output={}x{} reconstruction_ms={:.2} overlap_io_ms={:.2} completed_frames={}", shared.name, source, input_bytes, image.output_mode, output_width, output_height, started.elapsed().as_secs_f64() * 1000.0, overlap_io_ms, successful_frames());
            self.logged = true;
        }
        Ok(())
    }
}

fn inverse_dispatch(
    dispatches: &mut Vec<Dispatch>,
    low: Band,
    high: Band,
    vertical: bool,
    high_first: u32,
    destination: u32,
) -> Band {
    let rows = if vertical {
        low.rows + high.rows
    } else {
        low.rows
    };
    let cols = if vertical {
        low.cols
    } else {
        low.cols + high.cols
    };
    let mut parameters = [0u32; 28];
    parameters[..13].copy_from_slice(&[
        cols,
        rows,
        low.offset,
        high.offset,
        low.cols,
        low.rows,
        high.cols,
        high.rows,
        destination,
        u32::from(vertical),
        high_first,
        low.source,
        high.source,
    ]);
    dispatches.push(Dispatch {
        entry: 0,
        parameters,
    });
    Band {
        offset: destination,
        rows,
        cols,
        source: 1,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn integer_reconstruction_shader_validates() {
        for source in [
            include_str!("rendering/xocn_reconstruct.wgsl"),
            include_str!("rendering/xocn_entropy.wgsl"),
        ] {
            let module = wgpu::naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
            wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .expect("X-OCN decoding must use portable integer shader operations");
        }
    }
}
