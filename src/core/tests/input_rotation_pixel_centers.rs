// SPDX-License-Identifier: GPL-3.0-or-later
use gyroflow_core::{
    gpu::{BufferDescription, BufferSource, Buffers},
    stabilization::{distortion_models::DistortionModel, KernelParams, KernelParamsFlags, RGBA8, Stabilization},
};

fn render(rotation: i32, pixel_centers: bool) -> (Vec<u8>, Vec<u8>) {
    let (width, height) = (7usize, 5usize);
    let mut raw = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            raw[(y * width + x) * 4..(y * width + x + 1) * 4]
                .copy_from_slice(&[(x * 23 + 7) as u8, (y * 37 + 11) as u8, (x * 9 + y * 13 + 17) as u8, 255]);
        }
    }
    let sideways = rotation == 90 || rotation == 270;
    let (input_width, input_height) = if sideways { (height, width) } else { (width, height) };
    let mut input = vec![0u8; raw.len()];
    // Independent discrete-image quarter turns, with no renderer coordinate helpers.
    for y in 0..height {
        for x in 0..width {
            let (rx, ry) = match rotation {
                90 => (height - 1 - y, x),
                180 => (width - 1 - x, height - 1 - y),
                270 => (y, width - 1 - x),
                _ => (x, y),
            };
            input[(ry * input_width + rx) * 4..(ry * input_width + rx + 1) * 4]
                .copy_from_slice(&raw[(y * width + x) * 4..(y * width + x + 1) * 4]);
        }
    }
    let mut output = vec![173u8; raw.len()];
    let mut flags = KernelParamsFlags::HAS_SOURCE_RECT;
    flags.set(KernelParamsFlags::INPUT_ROTATION_PIXEL_CENTERS, pixel_centers);
    let params = KernelParams {
        width: width as i32, height: height as i32, stride: (input_width * 4) as i32,
        output_width: width as i32, output_height: height as i32, output_stride: (width * 4) as i32,
        matrix_count: 1, bytes_per_pixel: 4, pix_element_count: 4,
        f: [1.0, 1.0], input_horizontal_stretch: 1.0, input_vertical_stretch: 1.0,
        lens_correction_amount: 1.0, max_pixel_value: 255.0, pixel_value_limit: 255.0,
        input_rotation: rotation as f32, flags: flags.bits(),
        source_rect: [0, 0, input_width as i32, input_height as i32],
        output_rect: [0, 0, width as i32, height as i32],
        ..Default::default()
    };
    let mut buffers = Buffers {
        input: BufferDescription { size: (input_width, input_height, input_width * 4),
            data: BufferSource::Cpu { buffer: &mut input }, ..Default::default() },
        output: BufferDescription { size: (width, height, width * 4),
            data: BufferSource::Cpu { buffer: &mut output }, ..Default::default() },
    };
    let matrix = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    assert!(Stabilization::undistort_image_cpu::<2, RGBA8>(&mut buffers, &params,
        &DistortionModel::from_name("opencv_standard"), None, &[matrix], &[], &[]));
    (raw, output)
}

#[test]
fn quarter_turn_inputs_match_unrotated_pixels_when_opted_in() {
    for rotation in [0, 90, 180, 270] {
        let (raw, output) = render(rotation, true);
        assert_eq!(output, raw, "rotation={rotation}");
    }
}

#[test]
fn legacy_rotation_keeps_its_existing_pixel_origin() {
    let (raw, output) = render(90, false);
    assert_eq!(&output[..7 * 4], &[0; 7 * 4]);
    assert_eq!(&output[(2 * 7 + 3) * 4..(2 * 7 + 4) * 4], &raw[(7 + 3) * 4..(7 + 4) * 4]);
    let (raw, output) = render(0, false);
    assert_eq!(output, raw);
}
