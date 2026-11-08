// SPDX-License-Identifier: GPL-3.0-or-later
use super::{DistortionModel, KernelParams, tests::assemble_kernel_variant};
use wgpu::util::DeviceExt;

#[test]
#[ignore = "requires a real WGPU device; compares the production sampler to its loop reference"]
fn lanczos_interior_gpu_matches_loop_reference() {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    println!("Sampler device: {:?}", adapter.get_info());
    for format in [
        wgpu::TextureFormat::Rgba8Uint,
        wgpu::TextureFormat::Rgba16Uint,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::Rgba32Float,
    ] {
        for flags in [0, 1, 8, 9] {
            compare_sampler(&device, &queue, format, true, flags);
        }
    }
    // CPU-upload and direct-buffer paths use scalar storage instead of textures.
    for format in [
        wgpu::TextureFormat::Rgba16Uint,
        wgpu::TextureFormat::Rgba32Float,
    ] {
        compare_sampler(&device, &queue, format, false, 9);
    }
}

fn compare_sampler(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    textures: bool,
    flags: i32,
) {
    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 48;
    let floating = matches!(
        format,
        wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float
    );
    let scalar = if floating { "f32" } else { "u32" };
    let packed = !textures && !floating;
    let bpp = match format {
        wgpu::TextureFormat::Rgba8Uint => 4,
        wgpu::TextureFormat::Rgba16Uint | wgpu::TextureFormat::Rgba16Float => 8,
        _ => 16,
    };
    let limit = if floating {
        65504.0
    } else if bpp == 4 {
        255.0
    } else {
        65535.0
    };
    let input: Vec<u8> = (0..WIDTH * HEIGHT * 4)
        .flat_map(|i| {
            let n = i.wrapping_mul(179).wrapping_add(i / 71) % 65536;
            match format {
                wgpu::TextureFormat::Rgba8Uint => vec![n as u8],
                wgpu::TextureFormat::Rgba16Uint => (n as u16).to_ne_bytes().to_vec(),
                // Include negative and HDR channels which an 8-bit detour would destroy.
                wgpu::TextureFormat::Rgba16Float => half::f16::from_f32(n as f32 / 8192.0 - 2.0)
                    .to_ne_bytes()
                    .to_vec(),
                _ => (n as f32 / 8192.0 - 2.0).to_ne_bytes().to_vec(),
            }
        })
        .collect();
    let mut cases = Vec::<[f32; 2]>::new();
    for x in 0..32 {
        for y in 0..32 {
            // Every interpolation phase in the interior and across all four crop edges.
            for (base_x, base_y) in [
                (24.0, 20.0),
                (4.0, 20.0),
                (48.0, 20.0),
                (24.0, 3.0),
                (24.0, 39.0),
            ] {
                cases.push([base_x + x as f32 / 32.0, base_y + y as f32 / 32.0]);
            }
        }
    }
    let p = KernelParams {
        width: WIDTH as i32,
        height: HEIGHT as i32,
        stride: (WIDTH * bpp) as i32,
        output_width: WIDTH as i32,
        output_height: HEIGHT as i32,
        bytes_per_pixel: bpp as i32,
        pix_element_count: 4,
        interpolation: 8,
        source_rect: [4, 3, 44, 36],
        flags,
        canvas_scale: 1.0,
        max_pixel_value: limit,
        pixel_value_limit: limit,
        background: [0.17, 0.29, 0.43, 0.61],
        ..Default::default()
    };
    let buffer = |label, contents: &[u8], usage| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        })
    };
    let params = buffer(
        "sampler params",
        bytemuck::bytes_of(&p),
        wgpu::BufferUsages::UNIFORM,
    );
    let coeffs = buffer(
        "production coefficients",
        bytemuck::cast_slice(&crate::stabilization::COEFFS),
        wgpu::BufferUsages::STORAGE,
    );
    let drawing = buffer(
        "drawing overlay",
        &vec![0b00101110; (WIDTH * HEIGHT) as usize],
        wgpu::BufferUsages::STORAGE,
    );
    let coordinates = buffer(
        "all phases and crop edges",
        bytemuck::cast_slice(&cases),
        wgpu::BufferUsages::STORAGE,
    );
    let input_buffer = buffer("input pixels", &input, wgpu::BufferUsages::STORAGE);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("original pixel format"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &input,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(WIDTH * bpp),
            rows_per_image: None,
        },
        texture.size(),
    );
    let view = texture.create_view(&Default::default());
    let size = (cases.len() * 16) as u64;
    let result = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut source = assemble_kernel_variant(DistortionModel::default(), scalar, textures, packed);
    source.truncate(source.find("const GRID_SIZE:").unwrap());
    source.push_str(
        r#"
@group(0) @binding(7) var<storage, read> coordinates: array<vec2<f32>>;
@group(0) @binding(8) var<storage, read_write> results: array<vec4<f32>>;
@compute @workgroup_size(64)
fn compare_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= arrayLength(&coordinates)) { return; }
    results[id.x] = sample_input_at(coordinates[id.x], vec4<f32>(1.0, 0.0, 0.0, 1.0));
}
"#,
    );
    let mut outputs = Vec::new();
    for reference in [true, false] {
        let code = if reference {
            source.replace(
                "if (interpolation == 8u",
                "if (false && interpolation == 8u",
            )
        } else {
            source.clone()
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(code.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &shader,
            entry_point: Some("compare_main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[
                    ("100", 8.0),
                    ("101", 4.0),
                    ("102", bpp as f64),
                    ("103", flags as f64),
                ],
                ..Default::default()
            },
            cache: None,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: coeffs.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: drawing.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: if textures {
                        wgpu::BindingResource::TextureView(&view)
                    } else {
                        input_buffer.as_entire_binding()
                    },
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: coordinates.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: result.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups((cases.len() as u32).div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&result, 0, &readback, 0, size);
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        rx.recv().unwrap().unwrap();
        outputs.push(readback.slice(..).get_mapped_range().to_vec());
        readback.unmap();
    }
    assert!(
        outputs[0] == outputs[1],
        "{format:?}, textures={textures}, flags={flags}: sampler changed pixels"
    );
    println!(
        "{format:?}, textures={textures}, flags={flags}: {} samples bit-identical",
        cases.len()
    );
}
