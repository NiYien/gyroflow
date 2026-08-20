// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#[cfg(feature = "use-opencl")]
pub mod opencl;
pub mod wgpu;

pub mod wgpu_interop;
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod wgpu_interop_cuda;
#[cfg(target_os = "windows")]
pub mod wgpu_interop_directx;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod wgpu_interop_metal;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub mod wgpu_interop_vulkan;

pub mod drawing;
use serde::Serialize;
use std::collections::HashMap;
use std::hash::Hasher;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessingDeviceType {
    Discrete,
    Integrated,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessingDeviceInfo {
    pub list_name: String,
    pub display_name: String,
    pub vendor: String,
    pub device_type: ProcessingDeviceType,
    pub backend: String,
    pub physical_id: String,
    pub raw_index: usize,
    pub simple_preferred: bool,
    pub simple_priority: u8,
}

impl ProcessingDeviceInfo {
    pub fn new(
        list_name: String,
        display_name: String,
        vendor_hint: String,
        device_type: ProcessingDeviceType,
        backend: &str,
    ) -> Self {
        let display_name = clean_device_name(&display_name);
        let vendor = detect_vendor(&vendor_hint, &display_name).to_string();
        let physical_id = format!("{}:{}", vendor, normalize_device_name(&display_name));
        let simple_priority = device_priority(&vendor, device_type);
        Self {
            list_name,
            display_name,
            vendor,
            device_type,
            backend: backend.to_string(),
            physical_id,
            raw_index: 0,
            simple_preferred: false,
            simple_priority,
        }
    }
}

fn clean_device_name(name: &str) -> String {
    const COMPUTE_ENGINE: &str = " compute engine";
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with(COMPUTE_ENGINE) {
        trimmed[..trimmed.len() - COMPUTE_ENGINE.len()].trim_end().to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_device_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace("(r)", "")
        .replace("(tm)", "")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn detect_vendor<'a>(vendor_hint: &'a str, name: &'a str) -> &'static str {
    let haystack = format!("{vendor_hint} {name}").to_ascii_lowercase();
    if haystack.contains("nvidia") || haystack.contains("geforce") || haystack.contains("quadro") {
        "nvidia"
    } else if haystack.contains("intel") {
        "intel"
    } else if haystack.contains("advanced micro devices") || haystack.contains("radeon") || haystack.contains("amd") {
        "amd"
    } else if haystack.contains("apple") {
        "apple"
    } else {
        "other"
    }
}

fn device_priority(vendor: &str, device_type: ProcessingDeviceType) -> u8 {
    match (vendor, device_type) {
        ("nvidia", _) => 0,
        ("intel", ProcessingDeviceType::Discrete) => 1,
        ("amd", ProcessingDeviceType::Discrete) => 2,
        ("intel", ProcessingDeviceType::Integrated) => 3,
        ("amd", ProcessingDeviceType::Integrated) => 4,
        (_, ProcessingDeviceType::Discrete) => 5,
        (_, ProcessingDeviceType::Integrated) => 6,
        _ => 7,
    }
}

pub fn prepare_processing_device_infos(mut devices: Vec<ProcessingDeviceInfo>) -> Vec<ProcessingDeviceInfo> {
    let mut physical_types = HashMap::<String, ProcessingDeviceType>::new();
    for (raw_index, device) in devices.iter_mut().enumerate() {
        device.raw_index = raw_index;
        physical_types
            .entry(device.physical_id.clone())
            .and_modify(|current| {
                if device.device_type == ProcessingDeviceType::Discrete
                    || (*current == ProcessingDeviceType::Unknown
                        && device.device_type == ProcessingDeviceType::Integrated)
                {
                    *current = device.device_type;
                }
            })
            .or_insert(device.device_type);
    }

    let mut preferred = HashMap::<String, usize>::new();
    for device in &mut devices {
        device.device_type = physical_types[&device.physical_id];
        device.simple_priority = device_priority(&device.vendor, device.device_type);
        if !preferred.contains_key(&device.physical_id) {
            device.simple_preferred = true;
            preferred.insert(device.physical_id.clone(), device.raw_index);
        }
    }
    devices
}

pub fn select_processing_device<'a>(
    devices: &'a [ProcessingDeviceInfo],
    saved_physical_id: &str,
    saved_list_name: &str,
) -> Option<(&'a ProcessingDeviceInfo, &'static str)> {
    if let Some(device) = devices.iter().find(|device| device.list_name == saved_list_name) {
        return Some((device, "saved_backend"));
    }
    if let Some(device) = devices
        .iter()
        .find(|device| device.simple_preferred && device.physical_id == saved_physical_id)
    {
        return Some((device, "saved_physical"));
    }
    devices
        .iter()
        .filter(|device| device.simple_preferred)
        .min_by_key(|device| (device.simple_priority, device.raw_index))
        .map(|device| (device, "auto_priority"))
}

// Output-stage post-affine for the openfx-output-adjust-affine capability:
// composed into the stabilization kernel's existing inverse mapping so the
// transform stays a single Lanczos sample. See `openfx-output-adjust` spec.
#[derive(Debug, Copy, Clone)]
pub struct PostAffine {
    pub rotation_deg: f32,
    pub zoom: f32,
    pub offset_norm: [f32; 2],
}
impl Default for PostAffine {
    fn default() -> Self { Self { rotation_deg: 0.0, zoom: 1.0, offset_norm: [0.0, 0.0] } }
}

#[derive(Debug, Default)]
pub struct BufferDescription<'a> {
    pub size: (usize, usize, usize), // width, height, stride
    pub rect: Option<(usize, usize, usize, usize)>, // x, y, width, height
    pub rotation: Option<f32>,       // pixels rotation in degrees
    pub data: BufferSource<'a>,
    pub texture_copy: bool,
    pub post_affine: Option<PostAffine>,
    // openfx-output-adjust-flip: per-pass mirror toggles. Map onto KernelParamsFlags::FLIP_H /
    // FLIP_V in `Stabilization::get_kernel_flags`. Default `false` keeps every existing
    // construction site at identity behavior.
    pub flip_h: bool,
    pub flip_v: bool,
}
pub struct Buffers<'a> {
    pub input: BufferDescription<'a>,
    pub output: BufferDescription<'a>,
}

#[derive(Debug, Default)]
pub enum BufferSource<'a> {
    #[default]
    None,
    Cpu {
        buffer: &'a mut [u8],
    },
    #[cfg(feature = "use-opencl")]
    OpenCL {
        texture: ocl::ffi::cl_mem,
        queue: ocl::ffi::cl_command_queue,
    },
    #[cfg(target_os = "windows")]
    DirectX11 {
        texture: *mut std::ffi::c_void,        // ID3D11Texture2D*
        device: *mut std::ffi::c_void,         // ID3D11Device*
        device_context: *mut std::ffi::c_void, // ID3D11DeviceContext*
    },
    OpenGL {
        texture: u32,                   // GLuint
        context: *mut std::ffi::c_void, // OpenGL context pointer
    },
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    Vulkan {
        texture: u64,
        device: u64,
        physical_device: u64,
        instance: u64,
    },
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Metal {
        texture: *mut std::ffi::c_void,
        command_queue: *mut std::ffi::c_void,
    },
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    MetalBuffer {
        buffer: *mut std::ffi::c_void,
        command_queue: *mut std::ffi::c_void,
    },
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    CUDABuffer {
        buffer: *mut std::ffi::c_void, // Cudeviceptr
    },
}
impl<'a> BufferDescription<'a> {
    pub fn get_checksum(&self) -> u32 {
        let mut hasher = crc32fast::Hasher::new();
        hasher.write_usize(self.size.0);
        hasher.write_usize(self.size.1);
        hasher.write_usize(self.size.2);
        if let Some(r) = self.rect {
            hasher.write_usize(r.0);
            hasher.write_usize(r.1);
            hasher.write_usize(r.2);
            hasher.write_usize(r.3);
        }
        hasher.write_u32(self.rotation.unwrap_or_default().to_bits());
        let pa = self.post_affine.unwrap_or_default();
        hasher.write_u32(pa.rotation_deg.to_bits());
        hasher.write_u32(pa.zoom.to_bits());
        hasher.write_u32(pa.offset_norm[0].to_bits());
        hasher.write_u32(pa.offset_norm[1].to_bits());
        hasher.write_u8(self.flip_h as u8);
        hasher.write_u8(self.flip_v as u8);
        match &self.data {
            BufferSource::None => {}
            BufferSource::Cpu { .. } => {}
            #[cfg(feature = "use-opencl")]
            BufferSource::OpenCL { texture: _, queue } => {
                // if !self.texture_copy {
                //     hasher.write_u64(*texture as u64);
                // }
                hasher.write_u64(*queue as u64);
            }
            BufferSource::OpenGL { texture, context } => {
                if !self.texture_copy {
                    hasher.write_u32(*texture);
                }
                hasher.write_u64(*context as u64);
            }
            #[cfg(target_os = "windows")]
            BufferSource::DirectX11 {
                texture,
                device,
                device_context,
            } => {
                if !self.texture_copy {
                    hasher.write_u64(*texture as u64);
                }
                hasher.write_u64(*device as u64);
                hasher.write_u64(*device_context as u64);
            }
            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            BufferSource::Vulkan {
                texture,
                instance,
                device,
                physical_device,
            } => {
                if !self.texture_copy {
                    hasher.write_u64(*texture);
                }
                hasher.write_u64(*instance);
                hasher.write_u64(*device);
                hasher.write_u64(*physical_device);
            }
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            BufferSource::CUDABuffer { buffer } => {
                if !self.texture_copy {
                    hasher.write_u64(*buffer as u64);
                }
                hasher.write_i32(wgpu_interop_cuda::get_current_cuda_device());
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            BufferSource::Metal {
                texture,
                command_queue,
            } => {
                if !self.texture_copy {
                    hasher.write_u64(*texture as u64);
                }
                hasher.write_u64(*command_queue as u64);
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            BufferSource::MetalBuffer {
                buffer,
                command_queue,
            } => {
                if !self.texture_copy {
                    hasher.write_u64(*buffer as u64);
                }
                hasher.write_u64(*command_queue as u64);
            }
        }
        hasher.finalize()
    }

    /// Human-readable identity of the data source for backend-rebuild diagnostics.
    /// Mirrors exactly which handles `get_checksum` folds into the hash (per-variant,
    /// including the `texture_copy` gating), so a difference in this string explains
    /// a difference in the checksum — and equal strings mean the source did not
    /// contribute to a hash change.
    pub fn source_diag(&self) -> String {
        match &self.data {
            BufferSource::None => "none".to_string(),
            BufferSource::Cpu { .. } => "cpu".to_string(),
            #[cfg(feature = "use-opencl")]
            BufferSource::OpenCL { texture: _, queue } => format!("opencl(queue={:?})", *queue),
            BufferSource::OpenGL { texture, context } => {
                if !self.texture_copy {
                    format!("opengl(tex={texture},ctx={:?})", *context)
                } else {
                    format!("opengl(ctx={:?})", *context)
                }
            }
            #[cfg(target_os = "windows")]
            BufferSource::DirectX11 { texture, device, device_context } => {
                if !self.texture_copy {
                    format!("d3d11(tex={:?},dev={:?},ctx={:?})", *texture, *device, *device_context)
                } else {
                    format!("d3d11(dev={:?},ctx={:?})", *device, *device_context)
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            BufferSource::Vulkan { texture, instance, device, physical_device } => {
                if !self.texture_copy {
                    format!("vulkan(tex={texture:#x},inst={instance:#x},dev={device:#x},pdev={physical_device:#x})")
                } else {
                    format!("vulkan(inst={instance:#x},dev={device:#x},pdev={physical_device:#x})")
                }
            }
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            BufferSource::CUDABuffer { buffer } => {
                let dev = wgpu_interop_cuda::get_current_cuda_device();
                if !self.texture_copy {
                    format!("cuda(buf={:?},dev={dev})", *buffer)
                } else {
                    format!("cuda(dev={dev})")
                }
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            BufferSource::Metal { texture, command_queue } => {
                if !self.texture_copy {
                    format!("metal(tex={:?},queue={:?})", *texture, *command_queue)
                } else {
                    format!("metal(queue={:?})", *command_queue)
                }
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            BufferSource::MetalBuffer { buffer, command_queue } => {
                if !self.texture_copy {
                    format!("metalbuf(buf={:?},queue={:?})", *buffer, *command_queue)
                } else {
                    format!("metalbuf(queue={:?})", *command_queue)
                }
            }
        }
    }
}
impl<'a> Buffers<'a> {
    pub fn get_checksum(&self) -> u32 {
        let mut hasher = crc32fast::Hasher::new();
        hasher.write_u32(self.input.get_checksum());
        hasher.write_u32(self.output.get_checksum());
        hasher.finalize()
    }
}

pub fn initialize_contexts() -> Option<(String, String)> {
    #[cfg(feature = "use-opencl")]
    if std::env::var("NO_OPENCL").unwrap_or_default().is_empty() {
        let cl = std::panic::catch_unwind(|| opencl::OclWrapper::initialize_context(None));
        match cl {
            Ok(Ok(names)) => {
                return Some(names);
            }
            Ok(Err(e)) => {
                log::error!("OpenCL error init: {:?}", e);
            }
            Err(e) => {
                if let Some(s) = e.downcast_ref::<&str>() {
                    log::error!("Failed to initialize OpenCL {}", s);
                } else if let Some(s) = e.downcast_ref::<String>() {
                    log::error!("Failed to initialize OpenCL {}", s);
                } else {
                    log::error!("Failed to initialize OpenCL {:?}", e);
                }
            }
        }
    }

    if std::env::var("NO_WGPU").unwrap_or_default().is_empty() {
        let wgpu = std::panic::catch_unwind(|| wgpu::WgpuWrapper::initialize_context());
        match wgpu {
            Ok(Some(names)) => {
                return Some(names);
            }
            Ok(None) => {
                log::error!("wgpu init error");
            }
            Err(e) => {
                if let Some(s) = e.downcast_ref::<&str>() {
                    log::error!("Failed to initialize wgpu {}", s);
                } else if let Some(s) = e.downcast_ref::<String>() {
                    log::error!("Failed to initialize wgpu {}", s);
                } else {
                    log::error!("Failed to initialize wgpu {:?}", e);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod processing_device_tests {
    use super::*;

    fn device(
        list_name: &str,
        display_name: &str,
        vendor: &str,
        device_type: ProcessingDeviceType,
        backend: &str,
    ) -> ProcessingDeviceInfo {
        ProcessingDeviceInfo::new(
            list_name.to_string(),
            display_name.to_string(),
            vendor.to_string(),
            device_type,
            backend,
        )
    }

    #[test]
    fn simple_priority_matches_product_order() {
        let devices = prepare_processing_device_infos(vec![
            device("nvidia", "NVIDIA RTX", "NVIDIA", ProcessingDeviceType::Unknown, "wgpu"),
            device("intel-d", "Intel Arc", "Intel", ProcessingDeviceType::Discrete, "wgpu"),
            device("amd-d", "AMD Radeon Pro", "AMD", ProcessingDeviceType::Discrete, "wgpu"),
            device("intel-i", "Intel UHD", "Intel", ProcessingDeviceType::Integrated, "wgpu"),
            device("amd-i", "AMD Radeon 780M", "AMD", ProcessingDeviceType::Integrated, "wgpu"),
        ]);
        let priorities: Vec<u8> = devices.iter().map(|device| device.simple_priority).collect();
        assert_eq!(priorities, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn duplicate_backends_collapse_to_one_physical_gpu() {
        let devices = prepare_processing_device_infos(vec![
            device(
                "[OpenCL] Apple AMD Radeon Pro 5500M Compute Engine: OpenCL 1.2",
                "AMD Radeon Pro 5500M Compute Engine",
                "AMD",
                ProcessingDeviceType::Integrated,
                "opencl",
            ),
            device(
                "[wgpu] AMD Radeon Pro 5500M (Metal)",
                "AMD Radeon Pro 5500M",
                "AMD",
                ProcessingDeviceType::Discrete,
                "wgpu",
            ),
        ]);
        assert_eq!(devices[0].physical_id, devices[1].physical_id);
        assert!(devices[0].simple_preferred);
        assert!(!devices[1].simple_preferred);
        assert_eq!(devices[0].device_type, ProcessingDeviceType::Discrete);
        assert_eq!(devices[0].simple_priority, 2);
    }

    #[test]
    fn saved_backend_wins_and_auto_prefers_discrete_amd_over_integrated_intel() {
        let devices = prepare_processing_device_infos(vec![
            device(
                "intel",
                "Intel(R) UHD Graphics 630",
                "Intel",
                ProcessingDeviceType::Integrated,
                "opencl",
            ),
            device(
                "amd",
                "AMD Radeon Pro 5500M",
                "AMD",
                ProcessingDeviceType::Discrete,
                "opencl",
            ),
        ]);
        let (auto, auto_reason) = select_processing_device(&devices, "", "").unwrap();
        assert_eq!(auto.list_name, "amd");
        assert_eq!(auto_reason, "auto_priority");

        let (saved, saved_reason) = select_processing_device(&devices, "", "intel").unwrap();
        assert_eq!(saved.list_name, "intel");
        assert_eq!(saved_reason, "saved_backend");
    }
}
