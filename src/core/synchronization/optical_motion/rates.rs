// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use nalgebra::Vector3;
use rayon::prelude::*;
use crate::gyro_source::{ GyroSource, TimeQuat };
use super::cost::pair_rotation_rate;
use super::tracks::{ PairData, WindowTracks };

/// Converts quaternion-frame rad/s to chart degrees per second as [-y, x, -z].
/// This convention is determined by chart_conversion_matches_integrated_gyro.
pub fn quat_rate_to_chart_dps(rate: Vector3<f64>) -> [f64; 3] {
    [-rate.y.to_degrees(), rate.x.to_degrees(), -rate.z.to_degrees()]
}

/// Returns the gyro rotation rate only when both frame midpoints are covered by the quaternion map.
pub fn pair_gyro_rate(pd: &PairData, quats: &TimeQuat, offset_ms: f64) -> Option<Vector3<f64>> {
    let dt = (pd.b.mid_ms - pd.a.mid_ms) / 1000.0;
    if quats.len() < 2 || dt <= 0.0 { return None; }
    let start_ms = *quats.keys().next()? as f64 / 1000.0;
    let end_ms = *quats.keys().next_back()? as f64 / 1000.0;
    let a_ms = pd.a.mid_ms - offset_ms;
    let b_ms = pd.b.mid_ms - offset_ms;
    if !(start_ms..=end_ms).contains(&a_ms) || !(start_ms..=end_ms).contains(&b_ms) { return None; }
    let qa = GyroSource::clamped_quat_at_gyro_timestamp(quats, a_ms);
    let qb = GyroSource::clamped_quat_at_gyro_timestamp(quats, b_ms);
    Some((qb.inverse() * qa).scaled_axis() / dt)
}

/// Collects fitted rates at each frame pair's midpoint in microseconds.
pub fn rate_samples(windows: &[WindowTracks]) -> BTreeMap<i64, [f64; 3]> {
    let pairs: Vec<_> = windows.iter().flat_map(|window| window.pairs.iter()).collect();
    // Indexed parallel collection preserves pair order, so later pairs still replace duplicate keys.
    let samples: Vec<_> = pairs.par_iter().map(|pd| {
        let rate = pair_rotation_rate(pd)?;
        let key = ((pd.a.mid_ms + pd.b.mid_ms) / 2.0 * 1000.0).round() as i64;
        Some((key, quat_rate_to_chart_dps(rate)))
    }).collect();
    samples.into_iter().flatten().collect()
}

/// Correlates paired samples per axis, omitting quiet gyro axes and undefined correlations.
pub fn axis_pearson(fit: &[[f64; 3]], gyro: &[[f64; 3]], min_rms_dps: f64) -> [Option<f64>; 3] {
    let n = fit.len().min(gyro.len());
    if n < 3 { return [None; 3]; }
    std::array::from_fn(|axis| {
        let rms = (gyro[..n].iter().map(|v| v[axis] * v[axis]).sum::<f64>() / n as f64).sqrt();
        if rms < min_rms_dps { return None; }
        let mean_fit = fit[..n].iter().map(|v| v[axis]).sum::<f64>() / n as f64;
        let mean_gyro = gyro[..n].iter().map(|v| v[axis]).sum::<f64>() / n as f64;
        let (mut covariance, mut fit_variance, mut gyro_variance) = (0.0, 0.0, 0.0);
        for (f, g) in fit[..n].iter().zip(&gyro[..n]) {
            let df = f[axis] - mean_fit;
            let dg = g[axis] - mean_gyro;
            covariance += df * dg;
            fit_variance += df * df;
            gyro_variance += dg * dg;
        }
        let denominator = (fit_variance * gyro_variance).sqrt();
        if denominator <= 0.0 { return None; }
        let r = covariance / denominator;
        r.is_finite().then(|| r.clamp(-1.0, 1.0))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gyro_source::{ TimeIMU, TimeQuat };
    use crate::imu_integration::{ GyroIntegrator, SimpleGyroIntegrator };
    use crate::synchronization::optical_motion::cost::pair_rotation_rate;
    use crate::synchronization::optical_motion::testutil::{ synth_window, SynthSpec };
    #[test]
    fn chart_conversion_matches_integrated_gyro() {
        // The chart draws GyroSource::raw_imu, the integrator's input: the estimate must overlay it (spec §5.4-2)
        let input = [10.0, -20.0, 30.0];
        let imu: Vec<TimeIMU> = (0..=1000).map(|i| TimeIMU { timestamp_ms: i as f64, gyro: Some(input), accl: None, magn: None }).collect();
        let quats = SimpleGyroIntegrator::integrate(&imu, 1000.0);
        // TimeQuat keys are microseconds: the first samples at or after 300 ms and 310 ms
        let (ka, qa) = quats.range(300_000..).next().unwrap();
        let (kb, qb) = quats.range(310_000..).next().unwrap();
        let rate = (qb.inverse() * qa).scaled_axis() / ((kb - ka) as f64 / 1e6);
        let got = quat_rate_to_chart_dps(rate);
        for k in 0..3 { assert!((got[k] - input[k]).abs() <= 0.005 * input[k].abs(), "axis {k}: {got:?}"); }
    }

    #[test]
    fn rate_samples_key_pairs_at_their_midpoints() {
        let (window, _) = synth_window(&SynthSpec::default());
        let s = rate_samples(std::slice::from_ref(&window));
        let pd = &window.pairs[0];
        let key = ((pd.a.mid_ms + pd.b.mid_ms) / 2.0 * 1000.0).round() as i64;
        assert_eq!(s.get(&key).copied(), Some(quat_rate_to_chart_dps(pair_rotation_rate(pd).unwrap())));
        assert_eq!(s.len(), window.pairs.iter().filter(|p| pair_rotation_rate(p).is_some()).count());
    }

    #[test]
    fn pair_gyro_rate_none_outside_gyro_data() {
        let spec = SynthSpec::default();
        let (window, quats) = synth_window(&spec);
        let pd = &window.pairs[0];
        assert!(pair_gyro_rate(pd, &quats, spec.true_offset_ms).is_some());
        // Gyro data that ends 1 ms before frame b's gyro time
        let end_us = ((pd.b.mid_ms - spec.true_offset_ms - 1.0) * 1000.0) as i64;
        let short: TimeQuat = quats.range(..=end_us).map(|(k, v)| (*k, *v)).collect();
        assert!(pair_gyro_rate(pd, &short, spec.true_offset_ms).is_none());
    }

    #[test]
    fn axis_pearson_skips_quiet_axes() {
        let fit = vec![[1.0, 0.0, 5.0], [2.0, 0.1, -5.0], [3.0, -0.1, 5.0], [4.0, 0.0, -5.0]];
        let gyro = vec![[1.1, 0.0, 5.0], [2.0, 0.1, -5.0], [2.9, -0.1, 5.0], [4.2, 0.0, -5.0]];
        let r = axis_pearson(&fit, &gyro, 2.0);
        assert!(r[0].unwrap() > 0.99);
        assert!(r[1].is_none());      // RMS 0.07 °/s < 2
        assert!(r[2].unwrap() > 0.99);
    }
    #[test]
    fn overlapping_windows_keep_the_last_pair_at_each_key() {
        let (first, _) = synth_window(&SynthSpec::default());
        let mut last = WindowTracks { pairs: first.pairs.clone(), focal_px: first.focal_px };
        last.pairs[0].a.mid_ms -= 1.0;
        last.pairs[0].b.mid_ms += 1.0;
        let pd = &first.pairs[0];
        let key = ((pd.a.mid_ms + pd.b.mid_ms) / 2.0 * 1000.0).round() as i64;
        let expected = rate_samples(std::slice::from_ref(&last));
        assert_ne!(rate_samples(std::slice::from_ref(&first))[&key], expected[&key]);
        assert_eq!(rate_samples(&[first, last]), expected);
    }

}
