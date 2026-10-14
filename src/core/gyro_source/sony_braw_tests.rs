// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

#[test]
#[ignore = "requires SONY_BRAW_SAMPLE pointing to Video Assist_0210.braw"]
fn sony_braw_real_clip_metadata_reaches_display_and_projection() {
    let path = std::env::var("SONY_BRAW_SAMPLE").expect("SONY_BRAW_SAMPLE");
    let file = std::fs::File::open(&path).unwrap();
    let size = file.metadata().unwrap().len() as usize;
    let mut stream = std::io::BufReader::new(file);
    let md = GyroSource::parse_telemetry_file(
        &mut stream, size, &path, &FileLoadOptions::default(), (4240, 2392), 50.0,
        |_| {}, Arc::new(AtomicBool::new(false)),
    ).unwrap();
    let camera = md.camera_identifier.as_ref().unwrap();
    assert_eq!(camera.brand, "Sony");
    assert_eq!(camera.focal_length, Some(57.0));
    assert!(!camera.lens_info.is_empty());
    assert!(camera.lens_model.is_empty());
    assert_eq!(md.lens_positions.len(), 495);
    assert_eq!(md.lens_params.len(), 495);
    assert!((md.unit_pixel_focal_length.unwrap() - 4240.0 / 35.6).abs() < 1e-8);
    for lens in md.lens_params.values() {
        let focal = lens.focal_length.unwrap();
        let (fx, fy) = lens.pixel_focal_length.unwrap();
        assert!((fx as f64 - focal as f64 * 4240.0 / 35.6).abs() < 0.001);
        assert_eq!(fx, fy);
        assert_eq!(lens.iris_fstop, Some(4.0));
        assert!(lens.focus_distance.unwrap() > 0.0);
        assert!(lens.distortion_coefficients.is_empty());
    }
    assert_eq!(md.lens_params.first_key_value().unwrap().1.focal_length, Some(57.0));
    assert_eq!(md.lens_params.last_key_value().unwrap().1.focal_length, Some(54.0));
    let settings = &md.additional_data["recording_settings"];
    assert_eq!(settings["Focal length"], "57.00 mm");
    assert_eq!(settings["Focal length (35mm equivalent)"], "≈57.64 mm");
    assert_eq!(settings["Iris"], "f/4.0");
    assert_eq!(settings["Focus distance"], "1.255 m");
    assert_eq!(settings["ISO"], "800");
    assert_eq!(settings["Shutter speed"], "1/645 s");
    assert_eq!(settings["White balance"], "4200K");
    assert_eq!(settings["Gamma equation"], "S-Log3");
    assert_eq!(settings["Color primaries"], "S-Gamut3.Cine");
    assert_eq!(md.additional_data["focal_projection_estimated"], true);
    assert_eq!(md.additional_data["focal_length_35mm_estimated"], true);
    assert!(md.raw_imu.is_empty());
    assert!(md.quaternions.is_empty());
    assert!(md.camera_stab_data.is_empty());
    assert!(md.mesh_correction.is_empty());
    assert!(md.lens_breathing.is_empty());
    assert!(!md.keep_video_gyro);
    let expected_readout = std::env::var("SONY_BRAW_EXPECT_READOUT").ok().map(|v| v.parse::<f64>().unwrap());
    assert_eq!(md.frame_readout_time, expected_readout);
    if expected_readout.is_some() {
        assert_eq!(md.additional_data["readout_estimated"], true);
        assert_eq!(md.additional_data["sensor_line_time"], 0.0);
        assert_eq!(md.frame_readout_direction, ReadoutDirection::TopToBottom);
    }

    let encoded = serde_json::to_vec(&md).unwrap();
    let restored: FileMetadata = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored.lens_params.len(), md.lens_params.len());
    assert!((restored.unit_pixel_focal_length.unwrap() - md.unit_pixel_focal_length.unwrap()).abs() < 1e-10);
    assert_eq!(restored.additional_data["recording_settings"], *settings);
}
