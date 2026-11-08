// SPDX-License-Identifier: GPL-3.0-or-later
use crate::stabilization::{KernelParams, distortion_models::DistortionModel};

#[test]
#[ignore = "requires OpenCL; compares all interpolation phases and crop boundaries on the selected device"]
fn lanczos_interior_opencl_matches_loop_reference() {
    const WIDTH: i32 = 64;
    const HEIGHT: i32 = 48;
    let coordinates: Vec<ocl::prm::Float2> = (0..32)
        .flat_map(|x| {
            (0..32).flat_map(move |y| {
                [
                    (24.0, 20.0),
                    (4.0, 20.0),
                    (48.0, 20.0),
                    (24.0, 3.0),
                    (24.0, 39.0),
                ]
                .map(|(a, b)| ocl::prm::Float2::new(a + x as f32 / 32.0, b + y as f32 / 32.0))
            })
        })
        .collect();
    for (ty, convert, bpp, limit) in [
        ("uchar4", "convert_float4", 4, 255.0),
        ("ushort4", "convert_float4", 8, 65535.0),
        ("float4", "convert_float4", 16, 65504.0),
    ] {
        for flags in [0, 1, 8, 9] {
            let p = KernelParams {
                width: WIDTH,
                height: HEIGHT,
                output_width: WIDTH,
                output_height: HEIGHT,
                // Padding is deliberately different from the active row width.
                stride: (WIDTH + 4) * bpp,
                source_rect: [4, 3, 44, 36],
                bytes_per_pixel: bpp,
                pix_element_count: 4,
                interpolation: 8,
                flags,
                canvas_scale: 1.0,
                max_pixel_value: limit,
                pixel_value_limit: limit,
                ..Default::default()
            };
            let input: Vec<u8> = (0..(WIDTH + 4) * HEIGHT * 4)
                .flat_map(|i| {
                    let n = (i * 179 + i / 71) % 65536;
                    match bpp {
                        4 => vec![n as u8],
                        8 => (n as u16).to_ne_bytes().to_vec(),
                        _ => (n as f32 / 8192.0 - 2.0).to_ne_bytes().to_vec(),
                    }
                })
                .collect();
            let mut source = include_str!("opencl_undistort.cl").replace("\r\n", "\n");
            source.truncate(source.find("float2 rotate_and_distort").unwrap());
            let lens = format!(
                "{}\nfloat2 digital_undistort_point(float2 uv, __global KernelParams *p) {{ return uv; }}\nfloat2 digital_distort_point(float2 uv, __global KernelParams *p) {{ return uv; }}",
                DistortionModel::default().opencl_functions()
            );
            source = source
                .replace("LENS_MODEL_FUNCTIONS;", &lens)
                .replace("EXTENSIONS;", "")
                .replace("DATA_CONVERTF", convert)
                .replace("DATA_TYPEF", "float4")
                .replace(
                    "DATA_CONVERT",
                    &format!("convert_{}{}", ty, if bpp == 16 { "" } else { "_sat" }),
                )
                .replace("DATA_TYPE", ty)
                .replace("PIXEL_BYTES", &bpp.to_string())
                .replace("INTERPOLATION", "8");
            source.push_str(r#"
__kernel void compare_main(__global const uchar *pixels, __global KernelParams *params,
    __global const uchar *drawing, __global const float2 *coordinates, __global float4 *result) {
    int i = get_global_id(0);
    result[i] = sample_input_at(coordinates[i], (float4)(1.0f, 0.0f, 0.0f, 1.0f), pixels, params, drawing, (float4)(0.17f, 0.29f, 0.43f, 0.61f));
}
"#);
            let mut outputs = Vec::new();
            for reference in [true, false] {
                let code = if reference {
                    source.replace("#       if 8 == 8", "#       if 0")
                } else {
                    source.clone()
                };
                let pq = ocl::ProQue::builder()
                    .src(code)
                    .dims(coordinates.len())
                    .build()
                    .unwrap();
                let upload = |bytes: &[u8]| {
                    ocl::Buffer::<u8>::builder()
                        .queue(pq.queue().clone())
                        .len(bytes.len())
                        .copy_host_slice(bytes)
                        .build()
                        .unwrap()
                };
                let pixels = upload(&input);
                let params = upload(bytemuck::bytes_of(&p));
                let drawing = upload(&vec![0b00101110; (WIDTH * HEIGHT) as usize]);
                let points = ocl::Buffer::<ocl::prm::Float2>::builder()
                    .queue(pq.queue().clone())
                    .len(coordinates.len())
                    .copy_host_slice(&coordinates)
                    .build()
                    .unwrap();
                let result = pq.create_buffer::<ocl::prm::Float4>().unwrap();
                let kernel = pq
                    .kernel_builder("compare_main")
                    .arg(&pixels)
                    .arg(&params)
                    .arg(&drawing)
                    .arg(&points)
                    .arg(&result)
                    .build()
                    .unwrap();
                unsafe {
                    kernel.enq().unwrap();
                }
                let mut output = vec![ocl::prm::Float4::default(); coordinates.len()];
                result.read(&mut output).enq().unwrap();
                pq.queue().finish().unwrap();
                let bits: Vec<u32> = output
                    .iter()
                    .flat_map(|v| (0..4).map(|c| v[c].to_bits()))
                    .collect();
                outputs.push(bits);
                if !reference {
                    println!(
                        "{} {ty} flags={flags}: {} samples",
                        pq.device().name().unwrap(),
                        coordinates.len()
                    );
                }
            }
            assert!(
                outputs[0] == outputs[1],
                "{ty}, flags={flags}: OpenCL sampler changed pixels"
            );
        }
    }
}
