// SPDX-License-Identifier: GPL-3.0-or-later
//! Batched optical fitting on the tracking device. Large inputs stay resident across robust iterations.
use super::{
    gpu_klt::{GpuKlt, Shared},
    gpu_tracker::KltDevice,
};
use std::sync::{Arc, Mutex, PoisonError, atomic::Ordering::Relaxed, mpsc};
use std::time::Duration;
use wgpu::util::DeviceExt;

struct Pipelines {
    shared: Arc<Shared>,
    normal: wgpu::ComputePipeline,
    layer: wgpu::ComputePipeline,
    layer_points: usize,
    precision: Precision,
}
static PIPELINES: Mutex<Option<Arc<Pipelines>>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Precision { F64, DoubleF32 }
impl Precision {
    fn selected() -> Self {
        static VALUE: std::sync::OnceLock<Precision> = std::sync::OnceLock::new();
        *VALUE.get_or_init(|| {
            match std::env::var("GYROFLOW_OPTICAL_FIT_PRECISION").as_deref() {
                Ok("df32") | Err(_) => Self::DoubleF32,
                Ok("f64") => Self::F64,
                Ok(value) => { log::warn!("Unknown optical fit precision {value}, using double-f32"); Self::DoubleF32 }
            }
        })
    }
    fn encode(self, values: &[f64]) -> Result<Vec<u8>, String> {
        if self == Self::F64 { return Ok(bytemuck::cast_slice(values).to_vec()); }
        let mut pairs = Vec::with_capacity(values.len());
        for &value in values {
            let hi = value as f32;
            if !hi.is_finite() || (value != 0.0 && hi.abs() < f32::MIN_POSITIVE) {
                return Err("Optical fit input exceeds the double-f32 exponent range".into());
            }
            pairs.push([hi, (value - hi as f64) as f32]);
        }
        Ok(bytemuck::cast_slice(&pairs).to_vec())
    }
    fn accepts(self, value: f64) -> bool {
        value.is_finite() && (self == Self::F64 || {
            let hi = value as f32;
            hi.is_finite() && (value == 0.0 || hi.abs() >= f32::MIN_POSITIVE)
        })
    }
    fn decode(self, bytes: &[u8]) -> Vec<f64> {
        match self {
            Self::F64 => bytemuck::cast_slice::<u8, f64>(bytes).to_vec(),
            Self::DoubleF32 => bytemuck::cast_slice::<u8, [f32; 2]>(bytes).iter().map(|p| p[0] as f64 + p[1] as f64).collect(),
        }
    }
}

fn enabled(device: i32) -> Option<Option<usize>> {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        let raw=std::env::var("GYROFLOW_OPTICAL_FIT_GPU").ok();
        let value=match raw.as_deref().map(str::trim) {None|Some("1"|"true"|"on")=>true,Some("0"|"false"|"off")=>false,Some(v)=>{log::warn!("GYROFLOW_OPTICAL_FIT_GPU={v} invalid, using true");true}};
        log::info!(target:"lifecycle","optical_fit_gpu resolved enabled={value} source={}",if raw.is_some(){"env"}else{"default"});value
    }) {return None;}
    match KltDevice::for_processing_device(device) {
        KltDevice::Cpu => None,
        KltDevice::Gpu(selected) => Some(selected),
    }
}

impl Pipelines {
    fn get(selected: Option<usize>) -> Result<Arc<Self>, String> {
        let shared = GpuKlt::context(selected)?;
        let precision = Precision::selected();
        if precision == Precision::F64 && !shared
            .device
            .features()
            .contains(wgpu::Features::SHADER_F64)
        {
            return Err("Selected GPU backend has no f64 fitting support".into());
        }
        let mut cache = PIPELINES.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = cache
            .as_ref()
            .filter(|p| Arc::ptr_eq(&p.shared, &shared) && !shared.failed.load(Relaxed))
        {
            return Ok(p.clone());
        }
        let _resources = shared
            .resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let device = &shared.device;
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::Internal,
            wgpu::ErrorFilter::OutOfMemory,
        ]
        .map(|f| device.push_error_scope(f));
        let normal_source = match precision {
            Precision::F64 => include_str!("gpu_normal.wgsl").to_owned(),
            Precision::DoubleF32 => format!("{}\n{}", include_str!("gpu_double_f32.wgsl"), include_str!("gpu_normal_df32.wgsl")),
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("optical fitting"),
            source: wgpu::ShaderSource::Wgsl(normal_source.into()),
        });
        let normal = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("optical normal matrix"),
            layout: None,
            module: &shader,
            entry_point: Some("normal_matrix"),
            compilation_options: Default::default(),
            cache: None,
        });
        let memory = device.limits().max_compute_workgroup_storage_size;
        let layer_points = if memory >= 24576 {
            2048
        } else if memory >= 16384 {
            1024
        } else {
            512
        };
        let layer_body = match precision {
            Precision::F64 => include_str!("gpu_layer.wgsl").to_owned(),
            Precision::DoubleF32 => format!("{}\n{}", include_str!("gpu_double_f32.wgsl"), include_str!("gpu_layer_df32.wgsl")),
        };
        let layer_source = format!(
            "const MAX_POINTS:u32={layer_points}u;\n{}",
            layer_body
        );
        let layer_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("optical layer fit"),
            source: wgpu::ShaderSource::Wgsl(layer_source.into()),
        });
        let layer = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("optical layer fit"),
            layout: None,
            module: &layer_shader,
            entry_point: Some("fit_layers"),
            compilation_options: Default::default(),
            cache: None,
        });
        let errors: Vec<_> = scopes
            .into_iter()
            .rev()
            .filter_map(|s| pollster::block_on(s.pop()))
            .collect();
        if let Some(e) = errors.first() {
            return Err(e.to_string());
        }
        let p = Arc::new(Self {
            shared: shared.clone(),
            normal,
            layer,
            layer_points,
            precision,
        });
        log::info!(target:"sync","Optical fitting: GPU {} ({precision:?})",shared.name);
        *cache = Some(p.clone());
        Ok(p)
    }
}

fn storage(device: &wgpu::Device, bytes: u64, extra: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes.max(8),
        usage: wgpu::BufferUsages::STORAGE | extra,
        mapped_at_creation: false,
    })
}
fn readback(
    shared: &Shared,
    precision: Precision,
    buffer: &wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
) -> Result<Vec<f64>, String> {
    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shared.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(10)),
        })
    }))
    .map_err(|_| "GPU fitting device lost".to_string())
    .and_then(|r| r.map_err(|e| e.to_string()))
    .and_then(|_| {
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    });
    if let Err(e) = waited {
        shared.failed.store(true, Relaxed);
        return Err(e);
    }
    if shared.failed.load(Relaxed) {
        buffer.unmap();
        return Err("GPU fitting device failed".into());
    }
    let out = precision.decode(&buffer.slice(..).get_mapped_range());
    buffer.unmap();
    Ok(out)
}

struct NormalBlock {
    first: usize,
    rows: usize,
    weights: wgpu::Buffer,
    group: wgpu::BindGroup,
}
pub(crate) struct GpuNormal {
    pipelines: Arc<Pipelines>,
    result: wgpu::Buffer,
    mapped: wgpu::Buffer,
    blocks: Vec<NormalBlock>,
    rows: usize,
    columns: usize,
}
impl GpuNormal {
    pub fn for_device(device: i32, x: &[f64], y: &[f64], columns: usize) -> Option<Self> {
        let selected = enabled(device)?;
        if y.len() < 256 || columns < 16 {
            return None;
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::new(selected, x, y, columns)
        })) {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                log::warn!(target:"sync","Optical normal matrix: CPU fallback, {e}");
                None
            }
            Err(_) => {
                log::warn!(target:"sync","Optical normal matrix: CPU fallback after initialization panic");
                None
            }
        }
    }
    fn new(selected: Option<usize>, x: &[f64], y: &[f64], columns: usize) -> Result<Self, String> {
        if y.is_empty() || columns == 0 || x.len() != y.len() * columns {
            return Err("Invalid normal matrix shape".into());
        }
        let p = Pipelines::get(selected)?;
        let device = &p.shared.device;
        let _resources = p
            .shared
            .resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let rows = y.len();
        let augmented = columns + 1;
        let bytes = rows
            .checked_mul(augmented)
            .and_then(|n| n.checked_mul(8))
            .ok_or("Normal matrix size overflow")?;
        if bytes > 512 * 1024 * 1024 {
            return Err("Normal matrix exceeds the GPU resident memory budget".into());
        }
        if x.iter().chain(y).any(|v| !p.precision.accepts(*v)) {
            return Err("Invalid normal matrix input".into());
        }
        let block_rows =
            (device.limits().max_storage_buffer_binding_size as usize / (augmented * 8)).min(16384);
        if block_rows == 0 {
            return Err("Normal matrix row exceeds the GPU buffer limit".into());
        }
        let block_count = rows.div_ceil(block_rows);
        let result_bytes = (block_count * augmented * augmented * 8) as u64;
        if result_bytes > device.limits().max_storage_buffer_binding_size as u64 {
            return Err("Normal matrix output exceeds the GPU buffer limit".into());
        }
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::OutOfMemory,
        ]
        .map(|f| device.push_error_scope(f));
        let result = storage(device, result_bytes, wgpu::BufferUsages::COPY_SRC);
        let mapped = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: result_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut blocks = Vec::with_capacity(block_count);
        for (index, first) in (0..rows).step_by(block_rows).enumerate() {
            let count = (rows - first).min(block_rows);
            let mut packed = Vec::with_capacity(count * augmented);
            for c in 0..augmented {
                for r in first..first + count {
                    packed.push(if c == columns {
                        y[r]
                    } else {
                        x[r * columns + c]
                    });
                }
            }
            let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("optical design matrix block"),
                contents: &p.precision.encode(&packed)?,
                usage: wgpu::BufferUsages::STORAGE,
            });
            let weights = storage(device, (count * 8) as u64, wgpu::BufferUsages::COPY_DST);
            let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&[
                    count as u32,
                    augmented as u32,
                    (index * augmented * augmented) as u32,
                    1,
                ]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &p.normal.get_bind_group_layout(0),
                entries: &[&params, &input, &weights, &result]
                    .iter()
                    .enumerate()
                    .map(|(i, b)| wgpu::BindGroupEntry {
                        binding: i as u32,
                        resource: b.as_entire_binding(),
                    })
                    .collect::<Vec<_>>(),
            });
            blocks.push(NormalBlock {
                first,
                rows: count,
                weights,
                group,
            });
        }
        let errors: Vec<_> = scopes
            .into_iter()
            .rev()
            .filter_map(|s| pollster::block_on(s.pop()))
            .collect();
        if let Some(e) = errors.first() {
            return Err(e.to_string());
        }
        log::info!(target:"sync","Optical normal matrix: resident rows={rows} columns={columns} blocks={block_count} size={:.1} MB",bytes as f64/1e6);
        Ok(Self {
            pipelines: p.clone(),
            result,
            mapped,
            blocks,
            rows,
            columns,
        })
    }
    pub fn assemble(
        &mut self,
        weights: &[f64],
    ) -> Result<(nalgebra::DMatrix<f64>, nalgebra::DVector<f64>), String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.assemble_inner(weights)
        }))
        .map_err(|_| "GPU normal matrix panicked".to_string())?
    }
    fn assemble_inner(
        &mut self,
        weights: &[f64],
    ) -> Result<(nalgebra::DMatrix<f64>, nalgebra::DVector<f64>), String> {
        if weights.len() != self.rows || weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
            return Err("Invalid normal matrix weights".into());
        }
        let shared = &self.pipelines.shared;
        if shared.failed.load(Relaxed) {
            return Err("GPU fitting device failed".into());
        }
        let scale = weights.iter().copied().fold(0.0f64, f64::max).max(1e-300);
        let packed: Vec<_> = weights.iter().map(|w| *w / scale).collect();
        for block in &self.blocks {
            shared.queue.write_buffer(
                &block.weights,
                0,
                &self.pipelines.precision.encode(&packed[block.first..block.first + block.rows])?,
            );
        }
        let n = self.columns + 1;
        let mut enc = shared.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("optical normal matrix"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.normal);
            for block in &self.blocks {
                pass.set_bind_group(0, &block.group, &[]);
                pass.dispatch_workgroups(n as u32, n as u32, 1);
            }
        }
        enc.copy_buffer_to_buffer(
            &self.result,
            0,
            &self.mapped,
            0,
            (self.blocks.len() * n * n * 8) as u64,
        );
        let values = readback(
            shared,
            self.pipelines.precision,
            &self.mapped,
            shared.queue.submit(Some(enc.finish())),
        )?;
        let total = |index: usize| {
            values
                .chunks_exact(n * n)
                .map(|block| block[index])
                .sum::<f64>()
                * scale
        };
        let h = nalgebra::DMatrix::from_fn(self.columns, self.columns, |i, j| {
            total(i.max(j) * n + i.min(j))
        });
        let g = nalgebra::DVector::from_fn(self.columns, |i, _| total(self.columns * n + i));
        if !h.iter().chain(g.iter()).all(|v| v.is_finite()) {
            return Err("Non-finite GPU normal matrix".into());
        }
        Ok((h, g))
    }
}

/// A prepared visible point: x, y, observed dx/dy, relative inverse depth and cell weight.
pub(crate) trait LayerData {
    fn len(&self) -> usize;
    fn point(&self, index: usize) -> [f64; 6];
}
impl<T: LayerData> LayerData for &T {
    fn len(&self) -> usize {
        T::len(*self)
    }
    fn point(&self, index: usize) -> [f64; 6] {
        T::point(*self, index)
    }
}

struct LayerBuffers {
    input: wgpu::Buffer,
    jobs: wgpu::Buffer,
    output: wgpu::Buffer,
    mapped: wgpu::Buffer,
    params: wgpu::Buffer,
    group: wgpu::BindGroup,
    input_capacity: usize,
    work_capacity: usize,
}
pub(crate) struct GpuLayers {
    selected: Option<usize>,
    pipelines: Option<Arc<Pipelines>>,
    buffers: Option<LayerBuffers>,
    pub pairs: usize,
    pub batches: usize,
    #[cfg(test)]
    fail_next: bool,
}
impl GpuLayers {
    pub const BATCH: usize = 64;
    pub fn for_device(device: i32) -> Option<Self> {
        Some(Self {
            selected: enabled(device)?,
            pipelines: None,
            buffers: None,
            pairs: 0,
            batches: 0,
            #[cfg(test)]
            fail_next: false,
        })
    }
    #[cfg(test)]
    pub(crate) fn for_test() -> Option<Self> {
        let adapter = crate::gpu::wgpu::WgpuWrapper::compute_adapter(None)?;
        if Precision::selected() == Precision::F64 && !adapter.features().contains(wgpu::Features::SHADER_F64) {
            return None;
        }
        Some(Self {
            selected: None,
            pipelines: None,
            buffers: None,
            pairs: 0,
            batches: 0,
            fail_next: false,
        })
    }
    #[cfg(test)]
    pub(crate) fn failure_fixture() -> Self {
        Self {
            selected: None,
            pipelines: None,
            buffers: None,
            pairs: 0,
            batches: 0,
            fail_next: true,
        }
    }
    pub fn fit_batch<T: LayerData>(
        &mut self,
        inputs: &[T],
    ) -> Result<Vec<Option<[f64; 3]>>, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.fit_inner(inputs)))
            .map_err(|_| "GPU layer fit panicked".to_string())?
    }
    fn fit_inner<T: LayerData>(&mut self, inputs: &[T]) -> Result<Vec<Option<[f64; 3]>>, String> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next) {
            return Err("Injected fitting failure".into());
        }
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::get(self.selected)?);
        }
        let p = self.pipelines.as_ref().unwrap();
        let shared = &p.shared;
        if shared.failed.load(Relaxed) {
            return Err("GPU fitting device failed".into());
        }
        if inputs.len() > Self::BATCH {
            return Err("GPU layer batch is too large".into());
        }
        let mut output = vec![None; inputs.len()];
        let mut packed = Vec::<f64>::new();
        let mut jobs = Vec::<[u32; 4]>::new();
        let mut indices = Vec::new();
        let mut work_count = 0;
        for (index, input) in inputs.iter().enumerate() {
            let n = input.len();
            if n < 12 || n > p.layer_points {
                continue;
            }
            let start = packed.len();
            for i in 0..n {
                packed.extend(input.point(i));
            }
            if packed[start..].iter().any(|v| !v.is_finite()) {
                packed.truncate(start);
                continue;
            }
            jobs.push([start as u32, n as u32, work_count as u32, 0]);
            indices.push(index);
            work_count += n * 4;
        }
        if jobs.is_empty() {
            return Ok(output);
        }
        if self
            .buffers
            .as_ref()
            .is_none_or(|b| b.input_capacity < packed.len() || b.work_capacity < work_count)
        {
            let device = &shared.device;
            let _resources = shared
                .resources
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let scopes = [
                wgpu::ErrorFilter::Validation,
                wgpu::ErrorFilter::OutOfMemory,
            ]
            .map(|f| device.push_error_scope(f));
            let input_capacity = packed.len().next_power_of_two();
            let work_capacity = work_count.next_power_of_two();
            let input = storage(
                device,
                (input_capacity * 8) as u64,
                wgpu::BufferUsages::COPY_DST,
            );
            let work = storage(
                device,
                (work_capacity * 8) as u64,
                wgpu::BufferUsages::empty(),
            );
            let jobs_buffer = storage(
                device,
                (Self::BATCH * 16) as u64,
                wgpu::BufferUsages::COPY_DST,
            );
            let output_buffer = storage(
                device,
                (Self::BATCH * 32) as u64,
                wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            );
            let mapped = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (Self::BATCH * 32) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let params = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &p.layer.get_bind_group_layout(0),
                entries: &[&params, &input, &jobs_buffer, &work, &output_buffer]
                    .iter()
                    .enumerate()
                    .map(|(i, b)| wgpu::BindGroupEntry {
                        binding: i as u32,
                        resource: b.as_entire_binding(),
                    })
                    .collect::<Vec<_>>(),
            });
            let errors: Vec<_> = scopes
                .into_iter()
                .rev()
                .filter_map(|s| pollster::block_on(s.pop()))
                .collect();
            if let Some(e) = errors.first() {
                return Err(e.to_string());
            }
            self.buffers = Some(LayerBuffers {
                input,
                jobs: jobs_buffer,
                output: output_buffer,
                mapped,
                params,
                group,
                input_capacity,
                work_capacity,
            });
        }
        let b = self.buffers.as_ref().unwrap();
        shared
            .queue
            .write_buffer(&b.input, 0, &p.precision.encode(&packed)?);
        shared
            .queue
            .write_buffer(&b.jobs, 0, bytemuck::cast_slice(&jobs));
        shared.queue.write_buffer(
            &b.params,
            0,
            bytemuck::cast_slice(&[jobs.len() as u32, 1, 0, 0]),
        );
        let mut enc = shared.device.create_command_encoder(&Default::default());
        enc.clear_buffer(&b.output, 0, None);
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("optical layer fit"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&p.layer);
            pass.set_bind_group(0, &b.group, &[]);
            pass.dispatch_workgroups(jobs.len() as u32, 1, 1);
        }
        enc.copy_buffer_to_buffer(&b.output, 0, &b.mapped, 0, (Self::BATCH * 32) as u64);
        let values = readback(shared, p.precision, &b.mapped, shared.queue.submit(Some(enc.finish())))?;
        for (i, index) in indices.into_iter().enumerate() {
            let result = [values[i * 4], values[i * 4 + 1], values[i * 4 + 2]];
            if values[i * 4 + 3] == 1.0 && result.iter().all(|v| v.is_finite()) {
                output[index] = Some(result);
                self.pairs += 1;
            }
        }
        self.batches += 1;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn double_f32_keeps_input_low_bits_and_rejects_unsupported_exponents() {
        let input = [0.0, -0.0, 1.0 + 1e-10, -3.141592653589793, 1e-20, 1e20];
        let output = Precision::DoubleF32.decode(&Precision::DoubleF32.encode(&input).unwrap());
        for (&a, b) in input.iter().zip(output) { assert!((a-b).abs() <= a.abs()*1e-14); }
        for value in [f64::NAN, f64::INFINITY, 1e100, 1e-100] {
            assert!(!Precision::DoubleF32.accepts(value));
            assert!(Precision::DoubleF32.encode(&[value]).is_err());
        }
    }
    struct Layer(Vec<[f64; 6]>);
    impl LayerData for Layer {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn point(&self, i: usize) -> [f64; 6] {
            self.0[i]
        }
    }
    fn layer(n: usize, phase: f64) -> Layer {
        Layer(
            (0..n)
                .map(|i| {
                    let x = ((i * 71 % 1009) as f64 / 1009.0 - 0.5) * 1.6;
                    let y = ((i * 53 % 1013) as f64 / 1013.0 - 0.5) * 1.2;
                    let beta = 0.5 + (i * 37 % 503) as f64 / 250.0;
                    let noise = (i as f64 * 0.31 + phase).sin() * 1e-6;
                    let dx =
                        (0.002 + x * 0.0004) * beta + noise + if i % 37 == 0 { 0.01 } else { 0.0 };
                    let dy = (-0.001 + y * 0.0004) * beta - noise;
                    [x, y, dx, dy, beta, 1.0 / (1 + i % 7) as f64]
                })
                .collect(),
        )
    }
    fn quantile(mut values: Vec<(f64, f64)>) -> f64 {
        values.sort_by(|a, b| a.0.total_cmp(&b.0));
        let cutoff = values.iter().map(|v| v.1).sum::<f64>() * 0.5;
        let mut sum = 0.0;
        for (v, w) in values {
            sum += w;
            if sum >= cutoff {
                return v;
            }
        }
        unreachable!()
    }
    fn reference(input: &Layer) -> [f64; 3] {
        use nalgebra::{Matrix3, Vector3};
        let mut weights = vec![[1.0; 2]; input.len()];
        let mut solution = Vector3::zeros();
        for _ in 0..6 {
            let mut h = Matrix3::zeros();
            let mut g = Vector3::zeros();
            for (i, p) in input.0.iter().enumerate() {
                for axis in 0..2 {
                    let mut row = Vector3::zeros();
                    row[axis] = p[4];
                    row.z = p[4] * p[axis];
                    let w = weights[i][axis] * p[5];
                    h += row * row.transpose() * w;
                    g += row * (p[axis + 2] * w);
                }
            }
            solution = h.cholesky().unwrap().solve(&g);
            let residual: Vec<_> = input
                .0
                .iter()
                .flat_map(|p| {
                    [0, 1].map(|axis| {
                        (
                            p[axis + 2] - (solution[axis] + p[axis] * solution.z) * p[4],
                            p[5],
                        )
                    })
                })
                .collect();
            let centre = quantile(residual.clone());
            let scale = (2.5
                * 1.4826
                * quantile(
                    residual
                        .iter()
                        .map(|(v, w)| ((v - centre).abs(), *w))
                        .collect(),
                ))
            .max(1e-9);
            for (i, w) in weights.iter_mut().enumerate() {
                for axis in 0..2 {
                    w[axis] = 1.0 / (1.0 + (residual[2 * i + axis].0 / scale).powi(2));
                }
            }
        }
        [solution.x, solution.y, solution.z]
    }
    #[test]
    fn batched_layers_match_cpu_robust_fit_and_keep_job_order() {
        if GpuLayers::for_test().is_none() {
            return;
        }
        let p = Pipelines::get(None).unwrap();
        let inputs = vec![
            layer(128, 0.0),
            layer(1001, 0.7),
            layer(p.layer_points, 1.7),
            Layer(Vec::new()),
            layer(p.layer_points + 1, 0.0),
        ];
        let mut gpu = GpuLayers {
            selected: None,
            pipelines: Some(p),
            buffers: None,
            pairs: 0,
            batches: 0,
            fail_next: false,
        };
        for _ in 0..2 {
            let results = gpu.fit_batch(&inputs.iter().collect::<Vec<_>>()).unwrap();
            for i in 0..3 {
                let got = results[i].unwrap();
                let want = reference(&inputs[i]);
                let error = got
                    .iter()
                    .zip(want)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f64, f64::max);
                eprintln!("layer {} points: max difference={error:e}", inputs[i].len());
                assert!(error < 2e-10, "{got:?} vs {want:?}");
            }
            assert!(results[3].is_none() && results[4].is_none());
        }
        assert_eq!(gpu.pairs, 6);
        assert_eq!(gpu.batches, 2);
    }

    #[test]
    #[ignore]
    fn fitting_speed() {
        let mut gpu = GpuLayers {
            selected: None,
            pipelines: None,
            buffers: None,
            pairs: 0,
            batches: 0,
            fail_next: false,
        };
        let inputs: Vec<_> = (0..64).map(|i| layer(1500, i as f64 * 0.1)).collect();
        gpu.fit_batch(&inputs).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..3 {
            for input in &inputs {
                std::hint::black_box(reference(input));
            }
        }
        let cpu = t.elapsed().as_secs_f64() / 3.0;
        let t = std::time::Instant::now();
        for _ in 0..3 {
            std::hint::black_box(gpu.fit_batch(&inputs).unwrap());
        }
        let accelerated = t.elapsed().as_secs_f64() / 3.0;
        eprintln!(
            "64 layer fits: CPU={cpu:.6}s GPU={accelerated:.6}s ratio={:.2}",
            cpu / accelerated
        );
        let (rows, cols) = (10000, 135);
        let x: Vec<_> = (0..rows * cols).map(|i| (i as f64 * 0.13).sin()).collect();
        let y: Vec<_> = (0..rows).map(|i| (i as f64 * 0.03).cos()).collect();
        let weights = vec![1.0; rows];
        let mut normal = GpuNormal::new(None, &x, &y, cols).unwrap();
        normal.assemble(&weights).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(normal.assemble(&weights).unwrap());
        }
        let accelerated = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            let mut h = nalgebra::DMatrix::zeros(cols, cols);
            let mut g = nalgebra::DVector::zeros(cols);
            for (i, row) in x.chunks_exact(cols).enumerate() {
                let r = nalgebra::DVector::from_column_slice(row);
                h.ger(1.0, &r, &r, 1.0);
                g.axpy(y[i], &r, 1.0);
            }
            std::hint::black_box((h, g));
        }
        let cpu = t.elapsed().as_secs_f64();
        eprintln!(
            "10 normal assemblies {rows}x{cols}: CPU={cpu:.6}s GPU={accelerated:.6}s ratio={:.2}",
            cpu / accelerated
        );
    }
    #[test]
    fn gpu_normal_matrix_matches_f64_and_reuses_weights() {
        if GpuLayers::for_test().is_none() {
            return;
        }
        for (rows, columns) in [(1031, 7), (20003, 7), (1031, 135)] {
            let x: Vec<_> = (0..rows * columns)
                .map(|i| {
                    ((i * 71 % 10007) as f64 * 0.003).sin()
                        * 10f64.powi((i % columns % 9) as i32 - 4)
                })
                .collect();
            let y: Vec<_> = (0..rows).map(|i| (i as f64 * 0.017).cos() * 0.01).collect();
            let mut gpu = GpuNormal::new(None, &x, &y, columns).unwrap();
            for phase in [0.0, 1.0] {
                let weights: Vec<_> = (0..rows)
                    .map(|i| ((i as f64 * 0.1 + phase).sin().abs() + 0.01) * 1e8)
                    .collect();
                let mut invalid = weights.clone();
                invalid[0] = f64::NAN;
                assert!(gpu.assemble(&invalid).is_err());
                let (h, g) = gpu.assemble(&weights).unwrap();
                let mut expected = nalgebra::DMatrix::<f64>::zeros(columns, columns);
                let mut rhs = nalgebra::DVector::<f64>::zeros(columns);
                for (r, w) in weights.iter().enumerate() {
                    let row =
                        nalgebra::DVector::from_column_slice(&x[r * columns..(r + 1) * columns]);
                    expected.ger(*w, &row, &row, 1.0);
                    rhs.axpy(*w * y[r], &row, 1.0);
                }
                let err = (&h - &expected).norm() / expected.norm();
                let rhs_err = (&g - &rhs).norm() / rhs.norm();
                eprintln!("normal {rows}x{columns}: relative matrix={err:e} rhs={rhs_err:e}");
                assert!(err < 2e-11 && rhs_err < 2e-11);
            }
        }
    }
}
