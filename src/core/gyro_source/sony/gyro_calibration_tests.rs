// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use telemetry_parser::tags_impl::{TagDescription, ValueType, Vector3};
use telemetry_parser::util::SampleInfo;

fn insert(gyro: &mut TagMap, id: TagId, value: TagValue) {
    gyro.insert(id.clone(), TagDescription {
        group: GroupId::Gyroscope, id, native_id: None,
        description: String::new(), value,
    });
}

fn packet(timestamp_ms: f64, bias: [i16; 3], flags: u16, scale: f32, radians: bool) -> SampleInfo {
    let mut gyro = TagMap::new();
    insert(&mut gyro, TagId::Data, TagValue::Vec_Vector3_i16(ValueType::new_parsed(
        |v| format!("{v:?}"), vec![Vector3 { x: 38, y: 29, z: -1 }], Vec::new(),
    )));
    insert(&mut gyro, TagId::Unknown(0xe43d), TagValue::Vector3_i16(ValueType::new_parsed(
        |v| format!("{v:?}"), Vector3 { x: bias[0], y: bias[1], z: bias[2] }, Vec::new(),
    )));
    insert(&mut gyro, TagId::Unknown(0xe43e), TagValue::u16(ValueType::new_parsed(
        |v| v.to_string(), flags, Vec::new(),
    )));
    insert(&mut gyro, TagId::Scale, TagValue::f32(ValueType::new_parsed(
        |v| v.to_string(), scale, Vec::new(),
    )));
    insert(&mut gyro, TagId::Unknown(0xe438), TagValue::bool(ValueType::new_parsed(
        |v| v.to_string(), radians, Vec::new(),
    )));
    insert(&mut gyro, TagId::Frequency, TagValue::i32(ValueType::new_parsed(
        |v| v.to_string(), 2000, Vec::new(),
    )));
    insert(&mut gyro, TagId::TimeOffset, TagValue::f64(ValueType::new_parsed(
        |v| v.to_string(), -3.0, Vec::new(),
    )));
    SampleInfo { timestamp_ms, tag_map: Some([(GroupId::Gyroscope, gyro)].into()), ..Default::default() }
}

fn imu() -> TimeIMU {
    TimeIMU { timestamp_ms: 7.0, gyro: Some([38.0, 29.0, -1.0]),
        accl: Some([1.0, 2.0, 3.0]), magn: Some([4.0, 5.0, 6.0]) }
}

#[test]
fn factory_offset_uses_raw_axes_and_is_not_applied_twice() {
    let packets = [packet(0.0, [-17, 25, -18], 0xc210, 57.14, false)];
    let mut data = [imu()];
    assert_eq!(calibrate_gyro_from_packets(&mut data, &packets), 1);
    let scale = 57.14f32 as f64;
    assert_eq!(data[0].gyro, Some([55.0 / scale, 4.0 / scale, 17.0 / scale]));
    assert_eq!(data[0].timestamp_ms, 7.0);
    assert_eq!(data[0].accl, imu().accl);
    assert_eq!(data[0].magn, imu().magn);
    let once = data[0].gyro;
    assert_eq!(calibrate_gyro_from_packets(&mut data, &packets), 1);
    assert_eq!(data[0].gyro, once);
}

#[test]
fn factory_offset_handles_packet_changes_and_recorded_radian_units() {
    let packets = [
        packet(20.0, [-17, 25, -18], 0x8000, 2.0, false),
        packet(0.0, [30, 20, -5], 0x8000, 4.0, true),
    ];
    let mut data = vec![imu(), imu()];
    assert_eq!(calibrate_gyro_from_packets(&mut data, &packets), 2);
    let radians = data[1].gyro.unwrap();
    assert!((radians[0] - 2.0f64.to_degrees()).abs() < 1e-12);
    assert!((radians[1] - 2.25f64.to_degrees()).abs() < 1e-12);
    assert!((radians[2] - 1.0f64.to_degrees()).abs() < 1e-12);
    assert!(retime_imu_from_packets(&mut data, &packets));
    assert_eq!(data[0].timestamp_ms, -3.0);
    assert_eq!(data[0].gyro, Some(radians));
    assert_eq!(data[1].gyro, Some([27.5, 2.0, 8.5]));
}

#[test]
fn factory_offset_leaves_invalid_or_missing_calibration_unchanged() {
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let mut data = [imu()];
        assert_eq!(calibrate_gyro_from_packets(&mut data, &[packet(0.0, [1; 3], 0x8000, scale, false)]), 0);
        assert_eq!(data[0].gyro, imu().gyro);
    }
    for missing in [TagId::Unknown(0xe43d), TagId::Unknown(0xe43e), TagId::Scale] {
        let mut sample = packet(0.0, [1; 3], 0x8000, 1.0, false);
        sample.tag_map.as_mut().unwrap().get_mut(&GroupId::Gyroscope).unwrap().remove(&missing);
        let mut data = [imu()];
        assert_eq!(calibrate_gyro_from_packets(&mut data, &[sample]), 0);
        assert_eq!(data[0].gyro, imu().gyro);
    }
    let packets = [packet(0.0, [1; 3], 0x7fff, 1.0, false), packet(1.0, [1; 3], 0x8000, 1.0, false)];
    let mut data = [imu(), imu()];
    assert_eq!(calibrate_gyro_from_packets(&mut data, &packets), 1);
    assert_eq!(data[0].gyro, imu().gyro);
    assert_eq!(data[1].gyro, Some([37.0, 28.0, -2.0]));
}

#[test]
fn factory_offset_rejects_unmatched_or_unsupported_sample_lists_without_partial_changes() {
    let sample = packet(0.0, [1; 3], 0x8000, 1.0, false);
    let mut data = [imu(), imu()];
    assert_eq!(calibrate_gyro_from_packets(&mut data, &[sample.clone()]), 0);
    assert_eq!(data[0].gyro, imu().gyro);
    let mut unsupported = sample.clone();
    insert(unsupported.tag_map.as_mut().unwrap().get_mut(&GroupId::Gyroscope).unwrap(),
        TagId::Data, TagValue::Vec_Vector3_f32(ValueType::new_parsed(
            |v| format!("{v:?}"), vec![Vector3 { x: 1.0, y: 2.0, z: 3.0 }], Vec::new(),
        )));
    assert_eq!(calibrate_gyro_from_packets(&mut data, &[sample.clone(), unsupported]), 0);
    assert_eq!(data[0].gyro, imu().gyro);
    data[1].gyro = None;
    assert_eq!(calibrate_gyro_from_packets(&mut data, &[sample.clone(), sample]), 0);
    assert_eq!(data[0].gyro, imu().gyro);
}
