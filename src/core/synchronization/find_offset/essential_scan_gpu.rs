// SPDX-License-Identifier: GPL-3.0-or-later
//! Conservative GPU pruning, followed by original-precision CPU scoring.
//!
//! For each observation, S = sum(weight * (abs(gyro) + abs(video))^2) bounds
//! the error scale even when subtraction nearly cancels. Input conversion and
//! the fixed-size weighted score contribute O(epsilon * S); summing n positive
//! scores contributes O(n * epsilon * S). (32 + 8*n) * f32::EPSILON deliberately
//! exceeds these roundoff bounds for n <= 4096, including the error in S itself.
//! Restricted magnitudes prevent overflow; a small absolute floor covers flush
//! to zero. At uncertain timestamp boundaries the shader takes the smallest
//! score among every possible successor sample, not a guessed sample.

use super::*;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;

const BATCH: usize = 65536;
const TIMEOUT: Duration = Duration::from_secs(10);

struct Context {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    failed: Arc<AtomicBool>,
    resources: Mutex<()>,
}

impl Context {
    fn get(selected: i32) -> Result<Arc<Self>, String> {
        if selected < 0 || std::env::var("NO_WGPU").is_ok_and(|s| !s.is_empty()) {
            return Err("CPU processing device selected".into());
        }
        static CACHE: Mutex<Option<Arc<Context>>> = Mutex::new(None);
        let adapter = crate::gpu::wgpu::WgpuWrapper::compute_adapter(Some(selected as usize))
            .ok_or("no hardware compute adapter")?;
        let mut cached = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(context) = cached
            .as_ref()
            .filter(|c| c.adapter == adapter && !c.failed.load(Relaxed))
        {
            return Ok(context.clone());
        }
        let limits = adapter.limits();
        if limits.max_storage_buffers_per_shader_stage < 5 {
            return Err("GPU needs five storage bindings".into());
        }
        let mut requested = wgpu::Limits::downlevel_defaults();
        requested.max_storage_buffers_per_shader_stage = 5;
        requested.max_buffer_size = limits.max_buffer_size;
        requested.max_storage_buffer_binding_size = limits.max_storage_buffer_binding_size;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("deep-match scan"),
            required_limits: requested,
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let failed = Arc::new(AtomicBool::new(false));
        let failure = failed.clone();
        device.on_uncaptured_error(Arc::new(move |e| {
            log::warn!(target: "sync", "[deep-scan] GPU error: {e}");
            failure.store(true, Relaxed);
        }));
        let failure = failed.clone();
        device.set_device_lost_callback(move |_, _| {
            failure.store(true, Relaxed);
        });
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::Internal,
            wgpu::ErrorFilter::OutOfMemory,
        ]
        .map(|filter| device.push_error_scope(filter));
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("deep-match conservative score"),
            source: wgpu::ShaderSource::Wgsl(include_str!("essential_scan_gpu.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("deep-match scan"),
            layout: None,
            module: &shader,
            entry_point: Some("scan"),
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
        log::info!(target: "sync", "[deep-scan] GPU initialized: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
        let context = Arc::new(Self {
            adapter,
            device,
            queue,
            pipeline,
            failed,
            resources: Mutex::new(()),
        });
        *cached = Some(context.clone());
        Ok(context)
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Observation {
    rates: [f32; 4],
    base: [i32; 2],
    padding: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Parameters {
    start: u32,
    count: u32,
    gyro_count: u32,
    observation_count: u32,
    bucket_count: u32,
    tolerance_us: i32,
    error_factor: f32,
    padding: u32,
}

struct GpuInput {
    times: Vec<[i32; 2]>,
    rates: Vec<[f32; 4]>,
    buckets: Vec<u32>,
    observations: Vec<Observation>,
    tolerance_us: i32,
}

impl GpuInput {
    fn new(of: &[TimeIMU], gyro: &PackedGyro, grid: Grid) -> Result<Self, String> {
        if usize::BITS < 64
            || !grid.valid()
            || of.is_empty()
            || of.len() > 4096
            || gyro.times.is_empty()
            || grid.steps() > 500_000_000
            || gyro.times.len() > u32::MAX as usize
            || grid.initial.abs() > 1e9
            || grid.radius > 1e9
            || gyro.times.last().copied().unwrap_or(usize::MAX) as u64 > 1_000_000_000_000u64
            || gyro.present.iter().any(|&p| !p)
        {
            return Err("input outside GPU certification domain".into());
        }
        let rate = |v: [f64; 3]| -> Result<[f32; 4], String> {
            if v.iter().any(|x| {
                !x.is_finite()
                    || x.abs() > 1e12
                    || (*x != 0.0 && x.abs() < f32::MIN_POSITIVE as f64)
            }) {
                return Err("non-finite or out-of-range angular rate".into());
            }
            Ok([v[0] as f32, v[1] as f32, v[2] as f32, 0.0])
        };
        let origin_ms = (gyro.times[0] / 1000) as i64;
        let times: Vec<[i32; 2]> = gyro
            .times
            .iter()
            .map(|&t| {
                let ms = (t / 1000) as i64 - origin_ms;
                i32::try_from(ms)
                    .map(|ms| [ms, (t % 1000) as i32])
                    .map_err(|_| "gyro time span exceeds GPU range".to_string())
            })
            .collect::<Result<_, _>>()?;
        let rates = gyro
            .rates
            .iter()
            .map(|&v| rate(v))
            .collect::<Result<_, _>>()?;
        let mut max_time = grid.initial.abs() + grid.radius + grid.steps() as f64;
        let mut observations = Vec::with_capacity(of.len());
        for o in of {
            if !o.timestamp_ms.is_finite() || o.timestamp_ms.abs() > 1e9 {
                return Err("unsupported observation timestamp".into());
            }
            max_time = max_time
                .max(o.timestamp_ms.abs() + grid.initial.abs() + grid.radius + grid.steps() as f64);
            let q = ((o.timestamp_ms - grid.offset(0)) * 1000.0).floor() as i64;
            let ms = q.div_euclid(1000) - origin_ms;
            if ms.abs() + grid.steps() as i64 > 1_500_000_000 {
                return Err("query time span exceeds GPU range".into());
            }
            observations.push(Observation {
                rates: rate(o.gyro.ok_or("missing observation rate")?)?,
                base: [ms as i32, q.rem_euclid(1000) as i32],
                padding: [0; 2],
            });
        }
        // Bound all f64 subtractions/multiplications and integer conversion, including exact-key boundaries.
        let tolerance_us = (8.0 * f64::EPSILON * max_time * 1000.0).ceil() as i32 + 2;
        if tolerance_us > 16 {
            return Err("timestamp error bound is too wide".into());
        }
        let bucket_count = times.last().unwrap()[0] as usize / 4 + 2;
        if bucket_count > 32 * 1024 * 1024 {
            return Err("time index exceeds GPU memory budget".into());
        }
        let mut buckets = Vec::with_capacity(bucket_count);
        let mut index = 0usize;
        for bucket in 0..bucket_count {
            let t = bucket as i32 * 4;
            while index < times.len() && times[index][0] < t {
                index += 1;
            }
            buckets.push(index as u32);
        }
        Ok(Self {
            times,
            rates,
            buckets,
            observations,
            tolerance_us,
        })
    }
}

struct Session {
    context: Arc<Context>,
    bind: wgpu::BindGroup,
    parameters: wgpu::Buffer,
    output: wgpu::Buffer,
    staging: wgpu::Buffer,
    counts: Parameters,
}

impl Session {
    fn new(context: Arc<Context>, input: &GpuInput) -> Result<Self, String> {
        let _lock = context.resources.lock().unwrap_or_else(|e| e.into_inner());
        let device = &context.device;
        let arrays = [
            bytemuck::cast_slice(&input.times),
            bytemuck::cast_slice(&input.rates),
            bytemuck::cast_slice(&input.buckets),
            bytemuck::cast_slice(&input.observations),
        ];
        if arrays.iter().any(|a| {
            a.is_empty() || a.len() as u64 > device.limits().max_storage_buffer_binding_size as u64
        }) || arrays.iter().map(|a| a.len()).sum::<usize>() > 384 * 1024 * 1024
        {
            return Err("GPU buffer limit exceeded".into());
        }
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::Internal,
            wgpu::ErrorFilter::OutOfMemory,
        ]
        .map(|filter| device.push_error_scope(filter));
        let buffers: Vec<_> = arrays
            .iter()
            .map(|data| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("deep-match measurements"),
                    contents: data,
                    usage: wgpu::BufferUsages::STORAGE,
                })
            })
            .collect();
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("deep-match lower bounds"),
            size: (BATCH * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("deep-match readback"),
            size: (BATCH * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let counts = Parameters {
            start: 0,
            count: 0,
            gyro_count: input.times.len() as u32,
            observation_count: input.observations.len() as u32,
            bucket_count: input.buckets.len() as u32,
            tolerance_us: input.tolerance_us,
            error_factor: (32.0 + 8.0 * input.observations.len() as f32) * f32::EPSILON,
            padding: 0,
        };
        let parameters = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("deep-match parameters"),
            contents: bytemuck::bytes_of(&counts),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let mut entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: output.as_entire_binding(),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: 5,
            resource: parameters.as_entire_binding(),
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("deep-match inputs"),
            layout: &context.pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let errors: Vec<_> = scopes
            .into_iter()
            .rev()
            .filter_map(|scope| pollster::block_on(scope.pop()))
            .collect();
        if let Some(error) = errors.first() {
            return Err(error.to_string());
        }
        drop(_lock);
        Ok(Self {
            context,
            bind,
            parameters,
            output,
            staging,
            counts,
        })
    }

    fn bounds(&self, start: usize, count: usize) -> Result<Vec<f32>, String> {
        let c = &self.context;
        if c.failed.load(Relaxed) {
            return Err("GPU device lost".into());
        }
        let counts = Parameters {
            start: start as u32,
            count: count as u32,
            ..self.counts
        };
        c.queue
            .write_buffer(&self.parameters, 0, bytemuck::bytes_of(&counts));
        let mut encoder = c.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("deep-match score bounds"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&c.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(count.div_ceil(64) as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &self.staging, 0, (count * 4) as u64);
        let submission = c.queue.submit(Some(encoder.finish()));
        let (tx, rx) = mpsc::channel();
        self.staging
            .slice(..(count * 4) as u64)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        let result = c
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(TIMEOUT),
            })
            .map_err(|e| e.to_string())
            .and_then(|_| {
                rx.recv_timeout(TIMEOUT)
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())
            });
        if let Err(error) = result {
            self.staging.unmap();
            c.failed.store(true, Relaxed);
            return Err(error);
        }
        let bytes = self.staging.slice(..(count * 4) as u64).get_mapped_range();
        let values = bytemuck::cast_slice::<u8, f32>(&bytes).to_vec();
        drop(bytes);
        self.staging.unmap();
        if c.failed.load(Relaxed) {
            return Err("GPU execution failed".into());
        }
        Ok(values)
    }
}

pub(super) fn gpu_scan(
    of: &[TimeIMU],
    gyro: &PackedGyro,
    grid: Grid,
    keep_curve: bool,
    selected: i32,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64) + Sync),
) -> Result<ScanResult, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(of, gyro, grid, keep_curve, selected, cancel, progress)
    }))
    .unwrap_or_else(|_| Err("GPU operation panicked".into()))
}

fn run(
    of: &[TimeIMU],
    gyro: &PackedGyro,
    grid: Grid,
    keep_curve: bool,
    selected: i32,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64) + Sync),
) -> Result<ScanResult, String> {
    let started = Instant::now();
    if cancel.load(Relaxed) {
        return Err("cancelled".into());
    }
    let context = Context::get(selected)?;
    let mut input = GpuInput::new(of, gyro, grid)?;
    let session = Session::new(context, &input)?;
    let buckets = std::mem::take(&mut input.buckets);
    let origin_us = gyro.times[0] / 1000 * 1000;
    drop(input);
    let upload_ms = started.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    // The downstream posterior receives original-precision scores, never GPU approximations.
    let curve: Vec<_> = (0..grid.curve_steps())
        .into_par_iter()
        .filter_map(|i| {
            if cancel.load(Relaxed) {
                return None;
            }
            let j = i * 25;
            let cost = gyro.indexed_cost(grid.offset(j), of, origin_us, &buckets);
            (cost.is_finite() && cost != f64::MAX).then_some((j, cost))
        })
        .collect();
    if cancel.load(Relaxed) {
        return Err("cancelled".into());
    }
    let mut best = curve
        .iter()
        .copied()
        .reduce(choose)
        .ok_or("no finite CPU upper bound")?;
    let curve_ms = t.elapsed().as_secs_f64() * 1000.0;
    progress(0.1);
    let mut bound_ms = 0.0;
    let mut verify_ms = 0.0;
    let mut verified = 0usize;
    for start in (0..grid.steps()).step_by(BATCH) {
        if cancel.load(Relaxed) {
            return Err("cancelled".into());
        }
        let count = BATCH.min(grid.steps() - start);
        let t = Instant::now();
        let bounds = session.bounds(start, count)?;
        bound_ms += t.elapsed().as_secs_f64() * 1000.0;
        if cancel.load(Relaxed) {
            return Err("cancelled".into());
        }
        let candidates: Vec<_> = bounds
            .iter()
            .enumerate()
            .filter_map(|(i, &lower)| {
                (!lower.is_finite() || lower as f64 <= best.1).then_some(start + i)
            })
            .collect();
        if candidates.len() > count / 3 {
            return Err("broad score bounds; CPU scan is preferable".into());
        }
        let t = Instant::now();
        if let Some(found) = candidates
            .par_iter()
            .map(|&j| {
                (
                    j,
                    gyro.indexed_cost(grid.offset(j), of, origin_us, &buckets),
                )
            })
            .reduce_with(choose)
        {
            best = choose(best, found);
        }
        verified += candidates.len();
        verify_ms += t.elapsed().as_secs_f64() * 1000.0;
        progress(0.1 + 0.9 * (start + count) as f64 / grid.steps().max(1) as f64);
    }
    if cancel.load(Relaxed) {
        return Err("cancelled".into());
    }
    log::info!(target: "sync", "[deep-scan] GPU certified offsets={} checked={} upload_ms={upload_ms:.1} curve_ms={curve_ms:.1} bounds_ms={bound_ms:.1} verify_ms={verify_ms:.1} total_ms={:.1}",
        grid.steps(), verified, started.elapsed().as_secs_f64() * 1000.0);
    Ok(ScanResult {
        minimum: Some((grid.offset(best.0), best.1)),
        backend: Backend::Gpu,
        curve: if keep_curve {
            curve
                .into_iter()
                .map(|(j, c)| (grid.offset(j), c))
                .collect()
        } else {
            Vec::new()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    static GPU_TEST: Mutex<()> = Mutex::new(());

    #[test]
    fn deep_scan_gpu_rejects_unsupported_inputs_and_cpu_device() {
        let of = vec![TimeIMU {
            timestamp_ms: 1.0,
            gyro: Some([1.0; 3]),
            ..Default::default()
        }];
        let source = BTreeMap::from([(0, of[0].clone())]);
        let mut packed = PackedGyro::new(&source).unwrap();
        let grid = Grid {
            initial: 0.0,
            radius: 100.0,
        };
        assert!(Context::get(-1).is_err());
        packed.present[0] = false;
        assert!(GpuInput::new(&of, &packed, grid).is_err());
        assert!(
            gpu_scan(
                &of,
                &packed,
                grid,
                true,
                0,
                &AtomicBool::new(true),
                &|_| panic!("cancelled")
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires an actual compute GPU"]
    fn deep_scan_gpu_bounds_cover_every_reference_score() {
        let _guard = GPU_TEST.lock().unwrap();
        let context = Context::get(0).expect("test requires GPU");
        for irregular in [false, true] {
            let source: BTreeMap<_, _> = (0..45000)
                .map(|i| {
                    let t = if irregular {
                        i * 713 + i % 11
                    } else {
                        i * 1000
                    };
                    let x = i as f64 * 0.023;
                    (
                        t,
                        TimeIMU {
                            timestamp_ms: t as f64 / 1000.0,
                            gyro: Some([x.sin() * 10.0, x.cos() * 8.0, (x * 0.57).sin()]),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let packed = PackedGyro::new(&source).unwrap();
            let of: Vec<_> = (0..60)
                .map(|i| TimeIMU {
                    timestamp_ms: 1000.0000001 + i as f64 * 16.683333333,
                    gyro: Some([i as f64 * 0.01, (i as f64).sin(), 2.0]),
                    ..Default::default()
                })
                .collect();
            for initial in [-20000.0, -20000.1234567, 0.0] {
                let grid = Grid {
                    initial,
                    radius: 19000.75,
                };
                let input = GpuInput::new(&of, &packed, grid).unwrap();
                let session = Session::new(context.clone(), &input).unwrap();
                let bounds = session.bounds(0, grid.steps()).unwrap();
                for (j, &lower) in bounds.iter().enumerate() {
                    let exact = super::super::super::calculate_cost(grid.offset(j), &of, &source);
                    assert!(
                        (lower as f64) <= exact,
                        "irregular={irregular} j={j} lower={lower} exact={exact}"
                    );
                    assert_eq!(
                        packed
                            .indexed_cost(
                                grid.offset(j),
                                &of,
                                packed.times[0] / 1000 * 1000,
                                &input.buckets
                            )
                            .to_bits(),
                        exact.to_bits()
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "requires an actual compute GPU"]
    fn deep_scan_gpu_certification_and_fallback_preserve_results() {
        let _guard = GPU_TEST.lock().unwrap();
        let source: BTreeMap<_, _> = (0..90000)
            .map(|i| {
                let t = i * 1301;
                let x = i as f64 * 0.0031;
                (
                    t,
                    TimeIMU {
                        timestamp_ms: t as f64 / 1000.0,
                        gyro: Some([
                            x.sin() + (x * 1.73).cos(),
                            (x * 1.17).cos(),
                            (x * 0.37).sin(),
                        ]),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let packed = PackedGyro::new(&source).unwrap();
        let of: Vec<_> = (0..48)
            .map(|i| {
                let time = 1000.0000001 + i as f64 * 23.976543;
                let query = ((time + 15000.13) * 1000.0) as usize;
                TimeIMU {
                    timestamp_ms: time,
                    gyro: source.range(query..).next().unwrap().1.gyro,
                    ..Default::default()
                }
            })
            .collect();
        let grid = Grid {
            initial: -35000.75,
            radius: 35000.75,
        };
        let cancel = AtomicBool::new(false);
        let expected = cpu_scan(&of, &packed, grid, true, &cancel, &|_| {}).unwrap();
        let result = gpu_scan(&of, &packed, grid, true, 0, &cancel, &|_| {}).unwrap();
        assert_eq!(result.backend, Backend::Gpu);
        assert_eq!(result.minimum, expected.minimum);
        assert_eq!(result.curve, expected.curve);

        let flat_source: BTreeMap<_, _> = source
            .iter()
            .map(|(&t, s)| {
                (
                    t,
                    TimeIMU {
                        gyro: Some([1.0; 3]),
                        ..s.clone()
                    },
                )
            })
            .collect();
        let flat_of: Vec<_> = of
            .iter()
            .map(|s| TimeIMU {
                gyro: Some([1.0; 3]),
                ..s.clone()
            })
            .collect();
        let progress = Mutex::new(Vec::new());
        let fallback = scan_with_backend(
            Backend::Gpu,
            &flat_of,
            &flat_source,
            grid,
            true,
            0,
            &cancel,
            &|p| progress.lock().unwrap().push(p),
        )
        .unwrap();
        let reference = cpu_scan(
            &flat_of,
            &PackedGyro::new(&flat_source).unwrap(),
            grid,
            true,
            &cancel,
            &|_| {},
        )
        .unwrap();
        assert_eq!(fallback.backend, Backend::Cpu);
        assert_eq!(fallback.minimum, reference.minimum);
        assert_eq!(fallback.curve, reference.curve);
        assert!(progress.lock().unwrap().windows(2).all(|p| p[1] >= p[0]));

        assert!(
            scan_with_backend(Backend::Gpu, &of, &source, grid, true, 0, &cancel, &|_| {
                cancel.store(true, Relaxed)
            })
            .is_none()
        );
        let context = Context::get(0).unwrap();
        let input = GpuInput::new(&of, &packed, grid).unwrap();
        let session = Session::new(context.clone(), &input).unwrap();
        context.failed.store(true, Relaxed);
        assert!(session.bounds(0, 100).is_err());
        context.failed.store(false, Relaxed);
    }

    #[test]
    #[ignore = "requires an actual compute GPU"]
    #[cfg(target_pointer_width = "64")]
    fn deep_scan_gpu_absolute_times_and_dynamic_range_are_bounded() {
        let _guard = GPU_TEST.lock().unwrap();
        let context = Context::get(0).unwrap();
        for magnitude in [1e-15, 1.0, 1e10] {
            let source: BTreeMap<_, _> = (0..9000)
                .map(|i| {
                    let t = 7_200_000_123usize + i * 499 + i % 3;
                    (
                        t,
                        TimeIMU {
                            timestamp_ms: t as f64 / 1000.0,
                            gyro: Some([
                                magnitude * (i as f64 * 0.03).sin(),
                                magnitude * 0.37,
                                -magnitude,
                            ]),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let packed = PackedGyro::new(&source).unwrap();
            let of: Vec<_> = (0..35)
                .map(|i| TimeIMU {
                    timestamp_ms: (7_200_000_123usize + 600000 + i * 9999) as f64 / 1000.0
                        + 0.000000001,
                    gyro: Some([
                        magnitude * 0.1,
                        magnitude * 0.37 + magnitude * 1e-10,
                        -magnitude,
                    ]),
                    ..Default::default()
                })
                .collect();
            let grid = Grid {
                initial: 0.12300001,
                radius: 4500.75,
            };
            let session =
                Session::new(context.clone(), &GpuInput::new(&of, &packed, grid).unwrap()).unwrap();
            let bounds = session.bounds(0, grid.steps()).unwrap();
            for (j, &lower) in bounds.iter().enumerate() {
                let exact = super::super::super::calculate_cost(grid.offset(j), &of, &source);
                assert!(
                    lower as f64 <= exact,
                    "magnitude={magnitude} j={j} lower={lower} exact={exact}"
                );
            }
        }
    }
}
