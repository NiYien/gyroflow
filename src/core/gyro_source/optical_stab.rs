// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::{Arc, OnceLock};
use nalgebra::Vector3;
use super::{GyroSource, Quat64, TimeQuat};
use super::optical_correction::bspline_weights;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StabReconConfig {
    pub cutoff_hz: Option<f64>,
    pub max_deg: f64,
}

impl StabReconConfig {
    pub const DEFAULT: Self = Self { cutoff_hz: None, max_deg: 3.0 };

    pub fn resolved() -> Self {
        static RESOLVED: OnceLock<StabReconConfig> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let config = Self {
                cutoff_hz: resolve_number("GYROFLOW_STAB_RECON_CUTOFF_HZ", None, 50.0),
                max_deg: resolve_number("GYROFLOW_STAB_RECON_MAX_DEG", Some(Self::DEFAULT.max_deg), 30.0).unwrap(),
            };
            log::info!(target: "lifecycle", "stab_recon_config resolved cutoff_hz={:?} max_deg={}", config.cutoff_hz, config.max_deg);
            config
        })
    }
}

fn resolve_number(name: &str, default: Option<f64>, upper: f64) -> Option<f64> {
    match std::env::var(name) {
        Ok(raw) if !raw.is_empty() => {
            if let Ok(value) = raw.trim().parse::<f64>() {
                if value.is_finite() && value > 0.0 && value <= upper { return Some(value); }
            }
            log::warn!(target: "lifecycle", "{}={} invalid, falling back to {:?}", name, raw, default);
            default
        }
        _ => default,
    }
}

/// Logarithmic candidates: 0.05 * 40^(i/15), i = 0..16.
pub const CUTOFF_GRID_HZ: [f64; 16] = [
    0.05, 0.06394020196998705, 0.08176698855925471, 0.10456395525912732,
    0.13371680836098582, 0.1709975946676697, 0.21867241478865562, 0.2796391673370285,
    0.35760369676497206, 0.4573050519273263, 0.5848035476425731, 0.7478491389806213,
    0.9563524997900372, 1.2229874398215395, 1.563961278178932, 2.0,
];
pub const DEFAULT_CUTOFF_HZ: f64 = 0.3;

fn finite_vector(value: Vector3<f64>) -> Vector3<f64> {
    value.map(|component| if component.is_finite() { component } else { 0.0 })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PriorError { InvalidInput, Allocation, Cancelled }

/// A bounded-lifetime cache of low-pass output nodes, borrowing the original motion samples.
pub(crate) struct PriorSampler<'a> {
    quats: &'a TimeQuat,
    cutoff_hz: f64,
    sigma_us: f64,
    step_us: f64,
    nodes: std::collections::HashMap<i64, Quat64>,
    pub(crate) panels: usize,
    pub(crate) log_evaluations: usize,
    pub(crate) contributions: usize,
}

impl<'a> PriorSampler<'a> {
    pub(crate) fn new(quats: &'a TimeQuat, cutoff_hz: f64) -> Result<Self, PriorError> {
        let sigma_us=0.1325/cutoff_hz*1e6;
        let step_us=sigma_us/8.0;
        if !cutoff_hz.is_finite() || cutoff_hz<=0.0 || !sigma_us.is_finite() || step_us<=0.0 {return Err(PriorError::InvalidInput);}
        Ok(Self {quats,cutoff_hz,sigma_us,step_us,nodes:Default::default(),panels:0,log_evaluations:0,contributions:0})
    }

    pub(crate) fn cached_nodes(&self)->usize {self.nodes.len()}
    pub(crate) fn cutoff_hz(&self)->f64 {self.cutoff_hz}

    fn checkpoint(&mut self,cancel:&impl Fn()->bool)->Result<(),PriorError> {
        if self.contributions%1024==0 && cancel() {return Err(PriorError::Cancelled);}
        self.contributions=self.contributions.checked_add(1).ok_or(PriorError::Allocation)?;
        Ok(())
    }

    fn log(&mut self,q:Quat64)->Result<Vector3<f64>,PriorError> {
        self.log_evaluations=self.log_evaluations.checked_add(1).ok_or(PriorError::Allocation)?;
        let v=q.scaled_axis();
        if v.iter().all(|x|x.is_finite()) {Ok(v)} else {Err(PriorError::InvalidInput)}
    }

    fn integrate(&mut self,center:f64,a:f64,b:f64,mut value:impl FnMut(f64)->Result<Vector3<f64>,PriorError>,sum:&mut Vector3<f64>,mass:&mut f64,cancel:&impl Fn()->bool)->Result<usize,PriorError> {
        if b<=a {return Ok(0);}
        let lo=(a-center)/self.sigma_us;let hi=(b-center)/self.sigma_us;
        let count=((hi-lo)*8.0).ceil().max(1.0);
        if !count.is_finite() || count>usize::MAX as f64 {return Err(PriorError::Allocation);}
        let count=count as usize;
        self.panels=self.panels.checked_add(count).ok_or(PriorError::Allocation)?;
        let width=(hi-lo)/count as f64;
        let mut evaluations=0usize;
        for panel in 0..count {
            let middle=lo+(panel as f64+0.5)*width;
            for sign in [-1.0,1.0] {
                self.checkpoint(cancel)?;
                let u=middle+sign*width/(2.0*3.0f64.sqrt());
                let weight=0.5*width*(-0.5*u*u).exp();
                let v=value(center+u*self.sigma_us)?;
                if !v.iter().all(|x|x.is_finite()) {return Err(PriorError::InvalidInput);}
                *sum+=v*weight;*mass+=weight;
                evaluations=evaluations.checked_add(1).ok_or(PriorError::Allocation)?;
            }
        }
        Ok(evaluations)
    }

    fn constant(&mut self,center:f64,a:f64,b:f64,q:Quat64,reference:Quat64,sum:&mut Vector3<f64>,mass:&mut f64,cancel:&impl Fn()->bool)->Result<(),PriorError> {
        if b<=a {return Ok(());}
        let value=self.log(reference.inverse()*q)?;
        self.integrate(center,a,b,|_|Ok(value),sum,mass,cancel)?;
        Ok(())
    }

    fn source_segment(&mut self,center:f64,window:(f64,f64),source:(i64,Quat64,i64,Quat64),reference:Quat64,sum:&mut Vector3<f64>,mass:&mut f64,cancel:&impl Fn()->bool)->Result<(),PriorError> {
        let (t0,left,t1,mut right)=source;
        let start=t0 as f64;let end=t1 as f64;
        let (a,b)=(window.0.max(start),window.1.min(end));
        if b<=a {return Ok(());}
        let duration=(t1 as i128-t0 as i128) as f64;
        if duration<=0.0 {return Err(PriorError::InvalidInput);}
        if left.coords.dot(&right.coords)<0.0 {right=Quat64::new_unchecked(-right.into_inner());}
        if !left.coords.iter().chain(right.coords.iter()).all(|v|v.is_finite()) {return Err(PriorError::InvalidInput);}
        let dot=|q:Quat64|reference.coords.dot(&q.coords);
        // Four products and three additions have gamma_7 rounding error. A short-arc slerp is a
        // positive combination of its endpoints, so their propagated dot-error bounds cover this segment.
        let gamma=7.0*f64::EPSILON/(1.0-7.0*f64::EPSILON);
        let bound=|q:Quat64|gamma*reference.coords.iter().zip(q.coords.iter()).map(|(a,b)|(a*b).abs()).sum::<f64>();
        let (wa,wb)=(dot(left),dot(right));
        let whole_cut=wa.abs()<=bound(left) && wb.abs()<=bound(right);
        let midpoint=reference.inverse()*left.slerp(&right,0.5);
        let axis=midpoint.imag();
        let mut largest=0;for i in 1..3 {if axis[i].abs()>axis[largest].abs() {largest=i;}}
        let cut_sign=if axis[largest]<0.0 {-1.0} else {1.0};
        let mut cuts=[0.0;5];cuts[0]=a;let mut cut_count=1;
        if !whole_cut && wa.abs()>bound(left) && wb.abs()>bound(right) {
            // A root within endpoint dot rounding is the original source-node cut, not a new tiny panel.
            let cosine=left.coords.dot(&right.coords).clamp(0.0,1.0);
            let perpendicular=right.coords-left.coords*cosine;
            let sine=perpendicular.norm();
            if sine>0.0 {
                let angle=sine.atan2(cosine);
                let v=reference.coords.dot(&(perpendicular/sine));
                let root=(-wa).atan2(v);
                for root in [root-std::f64::consts::PI,root,root+std::f64::consts::PI] {
                    let time=start+duration*(root/angle);
                    if time>a && time<b {cuts[cut_count]=time;cut_count+=1;}
                }
            }
        }
        cuts[cut_count]=b;cut_count+=1;cuts[..cut_count].sort_by(f64::total_cmp);
        for span in cuts[..cut_count].windows(2) {
            let evaluated=self.integrate(center,span[0],span[1],|time| {
                let relative=reference.inverse()*left.slerp(&right,((time-start)/duration).clamp(0.0,1.0));
                if whole_cut {
                    // The sign is chosen once on the entire original segment, never on a clipped panel.
                    let vector=relative.imag()*cut_sign;let norm=vector.norm();
                    if norm==0.0 || !norm.is_finite() {return Err(PriorError::InvalidInput);}
                    Ok(vector*(std::f64::consts::PI/norm))
                } else {Ok(relative.scaled_axis())}
            },sum,mass,cancel)?;
            self.log_evaluations=self.log_evaluations.checked_add(evaluated).ok_or(PriorError::Allocation)?;
        }
        Ok(())
    }

    fn lowpass(&mut self,index:i64,cancel:&impl Fn()->bool)->Result<Quat64,PriorError> {
        if let Some(q)=self.nodes.get(&index) {return Ok(*q);}
        if cancel() {return Err(PriorError::Cancelled);}
        let center=index as f64*self.step_us;
        let (lo,hi)=(center-3.0*self.sigma_us,center+3.0*self.sigma_us);
        if !center.is_finite() || !lo.is_finite() || !hi.is_finite() || hi<=lo {return Err(PriorError::InvalidInput);}
        let reference=GyroSource::clamped_quat_at_gyro_timestamp(self.quats,center/1000.0);
        let quats=self.quats;
        let (&first_time,&first)=quats.first_key_value().ok_or(PriorError::InvalidInput)?;
        let (&last_time,&last)=quats.last_key_value().ok_or(PriorError::InvalidInput)?;
        let mut sum=Vector3::zeros();let mut mass=0.0;
        self.constant(center,lo,hi.min(first_time as f64),first,reference,&mut sum,&mut mass,cancel)?;
        let inside_lo=lo.max(first_time as f64);let inside_hi=hi.min(last_time as f64);
        if inside_hi>inside_lo {
            let (&mut_time,&mut_quat)=quats.range(..=(inside_lo.floor() as i64)).next_back().ok_or(PriorError::InvalidInput)?;
            let (mut previous_time,mut previous)=(mut_time,mut_quat);
            for (&time,&q) in quats.range((std::ops::Bound::Excluded(previous_time),std::ops::Bound::Unbounded)) {
                self.source_segment(center,(inside_lo,inside_hi),(previous_time,previous,time,q),reference,&mut sum,&mut mass,cancel)?;
                if time as f64>=inside_hi {break;}
                previous_time=time;previous=q;
            }
        }
        self.constant(center,lo.max(last_time as f64),hi,last,reference,&mut sum,&mut mass,cancel)?;
        if !mass.is_finite() || mass<=0.0 || !sum.iter().all(|v|v.is_finite()) {return Err(PriorError::InvalidInput);}
        let result=reference*Quat64::from_scaled_axis(sum/mass);
        self.nodes.try_reserve(1).map_err(|_|PriorError::Allocation)?;
        self.nodes.insert(index,result);
        Ok(result)
    }

    pub(crate) fn sample(&mut self,times_us:&[f64],cancel:&impl Fn()->bool)->Result<Vec<Vector3<f64>>,PriorError> {
        let mut values=Vec::new();values.try_reserve_exact(times_us.len()).map_err(|_|PriorError::Allocation)?;
        for (i,&time) in times_us.iter().enumerate() {
            if i%1024==0 && cancel() {return Err(PriorError::Cancelled);}
            let coordinate=time/self.step_us;let left=coordinate.floor();
            if self.quats.len()<=1 || !time.is_finite() || !coordinate.is_finite() || left<i64::MIN as f64 || left>=i64::MAX as f64-1.0 || (left+1.0)*self.step_us==left*self.step_us {
                values.push(Vector3::zeros());continue;
            }
            let a=self.lowpass(left as i64,cancel)?;let b=self.lowpass(left as i64+1,cancel)?;
            let lowpass=a.slerp(&b,coordinate-left);
            let q=GyroSource::clamped_quat_at_gyro_timestamp(self.quats,time/1000.0);
            let h=finite_vector((lowpass.inverse()*q).scaled_axis());
            values.push(Vector3::new(h.y,h.x,h.z));
        }
        Ok(values)
    }
}

/// High-frequency body rotation, expressed as normalized horizontal shift, vertical shift and roll.
pub fn prior(quats:&TimeQuat,cutoff_hz:f64,times_us:&[f64])->Vec<Vector3<f64>> {
    let result=PriorSampler::new(quats,cutoff_hz).and_then(|mut sampler|sampler.sample(times_us,&||false));
    match result {
        Ok(values)=>values,
        Err(error)=>{
            log::warn!("Optical stabilization prior unavailable: {error:?}");
            let mut zeros=Vec::new();
            if zeros.try_reserve_exact(times_us.len()).is_ok() {zeros.resize(times_us.len(),Vector3::zeros());}
            zeros
        }
    }
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OpticalStabReconstruction {
    pub enabled: bool,
    pub start_us: f64,
    pub spacing_us: f64,
    /// Control points of u = s - s0, with normalized shifts and roll in radians.
    #[serde(with = "super::optical_correction::quantized")]
    pub coeffs: Vec<[f32; 3]>,
    pub cutoff_hz: f64,
    pub quats_checksum: u64,
    pub context_checksum: u64,
    pub frames: usize,
    pub measured_frames: usize,
    pub max_deg: f64,
    #[serde(skip)]
    pub applies: bool,
    #[serde(skip)]
    table: Arc<Vec<(i64, [f64; 3])>>,
}

impl OpticalStabReconstruction {
    pub fn is_active(&self) -> bool {
        self.enabled && self.applies && !self.table.is_empty()
    }

    /// Whether the last rebuild produced a compensation table, whatever `enabled` and `applies` say
    pub fn has_table(&self) -> bool {
        !self.table.is_empty()
    }

    /// Rebuilds s = u + s0 over the spline support without deciding whether its analysis is current.
    pub fn rebuild(&mut self, quats: &TimeQuat, config: &StabReconConfig) {
        self.table = Arc::default();
        self.max_deg = 0.0;
        let result=PriorSampler::new(quats,self.cutoff_hz).and_then(|mut sampler|self.rebuild_with_prior(&mut sampler,config,&||false));
        if let Err(error)=result {log::warn!("Optical stabilization table unavailable: {error:?}");}
    }

    pub(crate) fn rebuild_with_prior(&mut self,sampler:&mut PriorSampler<'_>,config:&StabReconConfig,cancel:&impl Fn()->bool)->Result<(),PriorError> {
        if sampler.cutoff_hz().to_bits()!=self.cutoff_hz.to_bits() {return Err(PriorError::InvalidInput);}
        if self.coeffs.len() < 3 || !self.start_us.is_finite() || !self.spacing_us.is_finite() || self.spacing_us <= 0.0 {
            return Err(PriorError::InvalidInput);
        }
        let start = (self.start_us + self.spacing_us).ceil();
        let end = (self.start_us + (self.coeffs.len() - 2) as f64 * self.spacing_us).floor();
        if !start.is_finite() || !end.is_finite() || start < i64::MIN as f64 || end >= i64::MAX as f64 || end < start {
            return Err(PriorError::InvalidInput);
        }
        let start = start as i64;
        let end = end as i64;
        let count = (end as i128 - start as i128) / 1000 + 1;
        let Ok(count) = usize::try_from(count) else { return Err(PriorError::Allocation) };
        let mut times = Vec::new();
        let Some(capacity) = count.checked_add(1) else { return Err(PriorError::Allocation) };
        times.try_reserve_exact(capacity).map_err(|_|PriorError::Allocation)?;
        for i in 0..count { if i%1024==0 && cancel() {return Err(PriorError::Cancelled);} times.push((start as i128 + i as i128 * 1000) as f64); }
        if times.last().copied() != Some(end as f64) { times.push(end as f64); }
        let s0 = sampler.sample(&times,cancel)?;
        let limit = if config.max_deg.is_finite() && config.max_deg > 0.0 {
            config.max_deg.to_radians()
        } else { StabReconConfig::DEFAULT.max_deg.to_radians() };
        let soft = |value: f64| {
            let half = limit / 2.0;
            if value <= half { value } else { half + half * ((value - half) / half).tanh() }
        };
        let mut table = Vec::new();
        table.try_reserve_exact(times.len()).map_err(|_|PriorError::Allocation)?;
        let mut max_deg:f64=0.0;
        for (i,(time, prior)) in times.into_iter().zip(s0).enumerate() {
            if i%1024==0 && cancel() {return Err(PriorError::Cancelled);}
            let mut s = finite_vector(self.u_at(time) + prior);
            let norm = s.xy().norm();
            if norm > 0.0 {
                let scale = soft(norm) / norm;
                s.x *= scale;
                s.y *= scale;
            }
            s.z = s.z.signum() * soft(s.z.abs());
            max_deg = max_deg.max(s.xy().norm().max(s.z.abs()).to_degrees());
            table.push((time as i64, s.into()));
        }
        if cancel() {return Err(PriorError::Cancelled);}
        self.table = Arc::new(table);
        self.max_deg=max_deg;
        Ok(())
    }

    /// Evaluates the stored spline u independently of the prior and activation state.
    pub fn u_at(&self, t_us: f64) -> Vector3<f64> {
        if !t_us.is_finite() || !self.start_us.is_finite() || !self.spacing_us.is_finite() || self.spacing_us <= 0.0 || self.coeffs.is_empty() {
            return Vector3::zeros();
        }
        let coordinate = (t_us - self.start_us) / self.spacing_us;
        if !coordinate.is_finite() || coordinate < -2.0 || coordinate >= self.coeffs.len() as f64 + 1.0 {
            return Vector3::zeros();
        }
        let (segment, weights) = bspline_weights(coordinate);
        let mut value = Vector3::zeros();
        for (i, weight) in weights.iter().enumerate() {
            let index = segment + i as i64 - 1;
            if index >= 0 && (index as usize) < self.coeffs.len() {
                let coefficient = self.coeffs[index as usize];
                value += finite_vector(Vector3::new(coefficient[0] as f64, coefficient[1] as f64, coefficient[2] as f64)) * *weight;
            }
        }
        finite_vector(value)
    }

    /// Interpolates normalized s in gyro microseconds and returns zero outside the cached range.
    pub fn at(&self, t_us: f64) -> Vector3<f64> {
        if !t_us.is_finite() { return Vector3::zeros(); }
        let right = self.table.partition_point(|point| point.0 as f64 <= t_us);
        if right == 0 { return Vector3::zeros(); }
        let left = &self.table[right - 1];
        if t_us == left.0 as f64 { return Vector3::from(left.1); }
        if right == self.table.len() { return Vector3::zeros(); }
        let next = &self.table[right];
        let fraction = (t_us - left.0 as f64) / (next.0 as i128 - left.0 as i128) as f64;
        Vector3::from(left.1) * (1.0 - fraction) + Vector3::from(next.1) * fraction
    }

    pub fn measured_on(&self, quats_checksum: u64, context_checksum: u64) -> bool {
        !self.coeffs.is_empty() && self.quats_checksum == quats_checksum && self.context_checksum == context_checksum
    }

    pub fn hash_into(&self, hasher: &mut impl Hasher) {
        hasher.write_u8(self.enabled as u8);
        hasher.write_u64(self.cutoff_hz.to_bits());
        hasher.write_u64(self.quats_checksum);
        hasher.write_u64(self.context_checksum);
        hasher.write_u64(self.start_us.to_bits());
        hasher.write_u64(self.spacing_us.to_bits());
        hasher.write_usize(self.coeffs.len());
        for c in &self.coeffs {
            for v in c { hasher.write_u32(v.to_bits()); }
        }
    }

    pub fn checksum(&self) -> u64 {
        if !self.is_active() { return 0; }
        let mut hasher = DefaultHasher::new();
        self.hash_into(&mut hasher);
        hasher.finish().max(1)
    }

    /// Installs the given table directly; rebuilding still uses only the stored spline and body rotations.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_samples(samples: Vec<(i64, [f64; 3])>) -> Self {
        let mut hasher = DefaultHasher::new();
        hasher.write_usize(samples.len());
        for (time, values) in &samples {
            hasher.write_i64(*time);
            for value in values { hasher.write_u64(value.to_bits()); }
        }
        Self { enabled: true, applies: true, context_checksum: hasher.finish(), table: Arc::new(samples), ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gyro_source::Quat64;
    use nalgebra::Vector3;

    fn rotations(axis: usize, hz: f64) -> TimeQuat {
        (0..=20_000).map(|i| {
            let mut v = Vector3::zeros();
            v[axis] = 0.5_f64.to_radians() * (std::f64::consts::TAU * hz * i as f64 / 1000.0).sin();
            (i * 1000, Quat64::from_scaled_axis(v))
        }).collect()
    }

    fn middle_times() -> Vec<f64> { (5000..15_000).map(|i| i as f64 * 1000.0).collect() }

    fn reconstruction(degrees: f64) -> OpticalStabReconstruction {
        OpticalStabReconstruction {
            enabled: true, applies: true, start_us: 0.0, spacing_us: 1000.0,
            coeffs: vec![[degrees.to_radians() as f32, 0.0, degrees.to_radians() as f32]; 12],
            cutoff_hz: 0.5, ..Default::default()
        }
    }

    #[test]
    fn prior_cancels_fast_rotation_and_keeps_slow() {
        let amplitude = 0.5_f64.to_radians();
        let times = middle_times();
        let fast = prior(&rotations(1, 4.0), 0.5, &times).iter().map(|v| v.x.abs()).fold(0.0, f64::max);
        let slow = prior(&rotations(1, 0.1), 0.5, &times).iter().map(|v| v.x.abs()).fold(0.0, f64::max);
        assert!(fast >= 0.9 * amplitude, "fast={fast} amplitude={amplitude}");
        assert!(slow <= 0.1 * amplitude, "slow={slow} amplitude={amplitude}");
    }

    #[test]
    fn prior_follows_the_axes() {
        for (axis, output) in [(0, 1), (1, 0), (2, 2)] {
            let values = prior(&rotations(axis, 4.0), 0.5, &middle_times());
            let peak = values.iter().map(|v| v[output].abs()).fold(0.0, f64::max);
            assert!(peak >= 0.9 * 0.5_f64.to_radians());
            for other in (0..3).filter(|i| *i != output) {
                assert!(values.iter().all(|v| v[other].abs() <= 0.01 * peak));
            }
        }
    }

    #[test]
    fn round_trip_keeps_what_is_stored() {
        let mut value = reconstruction(1.0);
        value.start_us = 123.0;
        value.spacing_us = 2800.0;
        value.quats_checksum = 42;
        value.context_checksum = 7;
        value.frames = 90;
        value.measured_frames = 88;
        value.max_deg = 1.2;
        value.coeffs[4] = [0.0123, -0.0052, 0.0081];
        let encoded = crate::util::compress_to_base91_cbor(&value).unwrap();
        let mut decoded: OpticalStabReconstruction = crate::util::decompress_from_base91_cbor(&encoded).unwrap();
        assert_eq!((decoded.enabled, decoded.start_us, decoded.spacing_us, decoded.cutoff_hz, decoded.quats_checksum,
                    decoded.context_checksum, decoded.frames, decoded.measured_frames, decoded.max_deg),
                   (value.enabled, value.start_us, value.spacing_us, value.cutoff_hz, value.quats_checksum,
                    value.context_checksum, value.frames, value.measured_frames, value.max_deg));
        assert_eq!(decoded.coeffs.len(), value.coeffs.len());
        let step = value.coeffs.iter().flatten().fold(0.0_f32, |m, v| m.max(v.abs())) / i16::MAX as f32;
        for (actual, expected) in decoded.coeffs.iter().flatten().zip(value.coeffs.iter().flatten()) {
            assert!((actual - expected).abs() <= step);
        }
        assert!(!decoded.applies);
        assert_eq!(decoded.at(10_000.0), Vector3::zeros());
        decoded.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert!(decoded.at(10_000.0).norm() > 0.0);
    }

    #[test]
    fn checksum_is_zero_unless_active_and_follows_the_content() {
        let mut value = reconstruction(1.0);
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        let checksum = value.checksum();
        assert_ne!(checksum, 0);
        value.cutoff_hz = 0.3;
        assert_ne!(value.checksum(), checksum);
        value.cutoff_hz = 0.5;
        value.coeffs[4][1] = 0.3;
        assert_ne!(value.checksum(), checksum);
        for (enabled, applies) in [(false, true), (true, false)] {
            value.enabled = enabled; value.applies = applies;
            assert_eq!(value.checksum(), 0);
        }
        assert_eq!(OpticalStabReconstruction { enabled: true, applies: true, ..Default::default() }.checksum(), 0);
        let a = OpticalStabReconstruction::from_samples(vec![(1000, [0.01, 0.0, 0.0]), (2000, [0.02, 0.0, 0.0])]);
        let b = OpticalStabReconstruction::from_samples(vec![(1000, [0.01, 0.0, 0.0]), (2000, [0.03, 0.0, 0.0])]);
        assert_ne!(a.checksum(), b.checksum());
        assert_eq!(a.at(1500.0), Vector3::new(0.015, 0.0, 0.0));
    }

    #[test]
    fn limit_is_soft_and_bounded() {
        let mut large = reconstruction(10.0);
        large.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        let mut small = reconstruction(1.0);
        small.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        for time in 1000..=10_000 {
            let s = large.at(time as f64);
            assert!(s.xy().norm() <= 3.0_f64.to_radians() + 1e-9);
            assert!(s.z.abs() <= 3.0_f64.to_radians() + 1e-9);
            assert!((small.at(time as f64).x - 1.0_f64.to_radians()).abs() <= 1e-9);
            assert!((small.at(time as f64).z - 1.0_f64.to_radians()).abs() <= 1e-9);
        }
        assert!(large.max_deg <= 3.0 + 1e-9 && large.max_deg > small.max_deg);
    }

    #[test]
    fn nan_never_reaches_the_table() {
        let mut value = reconstruction(1.0);
        value.coeffs[4] = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        for time in 0..=11_000 {
            assert!(value.at(time as f64).iter().all(|v| v.is_finite()));
        }
        assert!(value.max_deg.is_finite());
    }

    #[test]
    fn outside_the_range_is_zero() {
        let mut value = reconstruction(1.0);
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert_eq!(value.at(999.0), Vector3::zeros());
        assert!(value.at(1000.0).norm() > 0.0);
        assert!(value.at(10_000.0).norm() > 0.0);
        assert_eq!(value.at(10_001.0), Vector3::zeros());
        value.enabled = false;
        value.applies = false;
        assert!(value.at(5000.0).norm() > 0.0);
    }

    #[test]
    fn rebuild_combines_spline_and_prior_and_shares_the_cache() {
        let quats = rotations(1, 4.0);
        let times: Vec<_> = (0..=20_000).map(|i| i as f64 * 1000.0).collect();
        let expected_prior = prior(&quats, 0.5, &times);
        let mut value = OpticalStabReconstruction {
            enabled: true, applies: true, start_us: -1000.0, spacing_us: 1000.0,
            coeffs: vec![[0.005, -0.003, 0.002]; 20_003], cutoff_hz: 0.5,
            quats_checksum: 7, context_checksum: 9, ..Default::default()
        };
        value.rebuild(&quats, &StabReconConfig::DEFAULT);
        assert!(value.measured_on(7, 9));
        assert!(!value.measured_on(8, 9) && !value.measured_on(7, 8));
        for i in 5000..15_000 {
            let expected = value.u_at(times[i]) + expected_prior[i];
            assert!((value.at(times[i]) - expected).norm() < 1e-12);
            let expected_half = (value.at(times[i]) + value.at(times[i + 1])) * 0.5;
            assert!((value.at(times[i] + 500.0) - expected_half).norm() < 1e-12);
        }
        let snapshot = value.clone();
        assert!(Arc::ptr_eq(&value.table, &snapshot.table));
        let snapshot_checksum = snapshot.checksum();
        let snapshot_sample = snapshot.at(5_000_000.0);
        value.coeffs[5001][0] += 0.01;
        value.rebuild(&quats, &StabReconConfig::DEFAULT);
        assert!(!Arc::ptr_eq(&value.table, &snapshot.table));
        assert_eq!(snapshot.checksum(), snapshot_checksum);
        assert_eq!(snapshot.at(5_000_000.0), snapshot_sample);
        assert_ne!(value.at(5_000_000.0), snapshot_sample);
    }

    #[test]
    fn prior_is_independent_of_the_query_batch() {
        let quats = rotations(1, 0.7);
        let time = 5_011_000.0;
        let single = prior(&quats, 0.5, &[time])[0];
        for companion in [-2_345_000.0, 0.0, 4_998_203.0, 6_789_123.0, 24_000_000.0] {
            let batch = prior(&quats, 0.5, &[companion, time, f64::NAN]);
            let difference = (batch[1] - single).norm();
            assert!(difference <= 1e-12, "companion={companion} difference={difference}");
            assert_eq!(batch[2], Vector3::zeros());
        }
    }

    #[test]
    fn sparse_prior_cancels_the_prior_in_dense_rebuild() {
        let quats = rotations(1, 0.7);
        let time = 5_011_000.0;
        let sparse_prior = prior(&quats, 0.5, &[time])[0];
        // A zero measured shift gives u = -s0. Constant coefficients reproduce that sparse sample.
        let coefficient = [-sparse_prior.x as f32, -sparse_prior.y as f32, -sparse_prior.z as f32];
        let mut value = OpticalStabReconstruction {
            enabled: true, applies: true, start_us: time - 5000.0, spacing_us: 1000.0,
            coeffs: vec![coefficient; 12], cutoff_hz: 0.5, ..Default::default()
        };
        let rounding_residual = value.u_at(time) + sparse_prior;
        value.rebuild(&quats, &StabReconConfig::DEFAULT);
        let difference = (value.at(time) - rounding_residual).norm();
        assert!(difference <= 1e-12, "dense versus sparse prior difference={difference}");
        assert!(value.at(time).norm() <= rounding_residual.norm() + 1e-12);
    }
    #[test]
    fn cutoff_grid_and_defaults_follow_the_spec() {
        assert_eq!(StabReconConfig::DEFAULT, StabReconConfig { cutoff_hz: None, max_deg: 3.0 });
        assert_eq!(DEFAULT_CUTOFF_HZ, 0.3);
        for (i, cutoff) in CUTOFF_GRID_HZ.iter().enumerate() {
            let expected = 0.05 * 40.0_f64.powf(i as f64 / 15.0);
            assert!((*cutoff - expected).abs() <= expected * 1e-15);
        }
    }
    #[test]
    fn invalid_times_and_spacing_are_safe() {
        let mut point = reconstruction(1.0);
        point.coeffs.truncate(3);
        point.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert!(point.is_active());
        assert!((point.at(1000.0).x - 1.0_f64.to_radians()).abs() <= 1e-9);
        assert_eq!(point.at(999.0), Vector3::zeros());
        assert_eq!(point.at(1001.0), Vector3::zeros());
        for spacing in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut value = reconstruction(1.0);
            value.spacing_us = spacing;
            value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
            assert_eq!(value.at(5000.0), Vector3::zeros());
            assert_eq!(value.u_at(5000.0), Vector3::zeros());
            assert!(!value.is_active());
        }
        let mut value = reconstruction(1.0);
        value.start_us = f64::NAN;
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert!(!value.is_active());
        assert_eq!(value.at(f64::NAN), Vector3::zeros());
        assert_eq!(value.u_at(f64::INFINITY), Vector3::zeros());
        for cutoff in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(prior(&rotations(0, 4.0), cutoff, &[5000.0]), vec![Vector3::zeros()]);
        }
        assert_eq!(prior(&TimeQuat::new(), 0.5, &[f64::NAN, 1000.0]), vec![Vector3::zeros(); 2]);
    }
    fn sampled_sine(time_us:f64)->f64 {
        let sample=time_us/1000.0;let left=sample.floor();let fraction=sample-left;
        let value=|ms:f64|0.005*(std::f64::consts::TAU*3.0*ms/1000.0).sin();
        value(left)*(1.0-fraction)+value(left+1.0)*fraction
    }

    // Independent dense Simpson integration of the piecewise-linear sampled single-axis input.
    fn dense_sine_lowpass(center_us:f64,cutoff:f64)->f64 {
        let sigma=0.1325/cutoff*1e6;let start=center_us-3.0*sigma;let end=center_us+3.0*sigma;
        let panels=(((end-start)/250.0).ceil() as usize).div_ceil(2)*2;
        let mut value=0.0;let mut mass=0.0;
        for i in 0..=panels {
            let t=start+(end-start)*i as f64/panels as f64;
            let quadrature=if i==0 || i==panels {1.0} else if i%2==0 {2.0} else {4.0};
            let weight=quadrature*(-0.5*((t-center_us)/sigma).powi(2)).exp();
            value+=weight*sampled_sine(t);mass+=weight;
        }
        value/mass
    }

    #[test]
    fn prior_preserves_three_hz_without_low_cutoff_aliasing() {
        let quats:TimeQuat=(0..=60000).map(|ms|(ms*1000,Quat64::from_scaled_axis(Vector3::new(0.0,sampled_sine(ms as f64*1000.0),0.0)))).collect();
        let cutoff=0.05;let step=0.1325/cutoff*1e6/8.0;
        let times:Vec<_>=(10000..=40000).map(|ms|ms as f64*1000.0).collect();
        let actual=prior(&quats,cutoff,&times);
        let first=(times[0]/step).floor() as i64;let last=(times[times.len()-1]/step).floor() as i64+1;
        let reference:Vec<_>=(first..=last).map(|i|dense_sine_lowpass(i as f64*step,cutoff)).collect();
        let mut maximum:f64=0.0;let mut square=0.0;
        for (&time,s) in times.iter().zip(&actual) {
            let coordinate=time/step;let left=coordinate.floor() as i64;let fraction=coordinate-left as f64;
            let lp=reference[(left-first) as usize]*(1.0-fraction)+reference[(left-first+1) as usize]*fraction;
            let expected=sampled_sine(time)-lp;
            let error=(s.x-expected).abs();maximum=maximum.max(error);square+=error*error;
        }
        let index=3000;let time=times[index];
        let coordinate=time/step;let left=coordinate.floor() as i64;let fraction=coordinate-left as f64;
        let interpolated=reference[(left-first) as usize]*(1.0-fraction)+reference[(left-first+1) as usize]*fraction;
        let ideal=dense_sine_lowpass(time,cutoff);
        println!("prior alias fixed3Hz amplitude=.005 cutoff=.05 t13_actual={} t13_same_nodes_expected={} max_error_10_40s={maximum} rms_error_10_40s={} output_grid_vs_continuous_t13={}",actual[index].x,sampled_sine(time)-interpolated,(square/times.len() as f64).sqrt(),interpolated-ideal);
        assert!(maximum<=1e-9,"same-node reference error {maximum}");
    }

    fn reference_pose(quats:&TimeQuat,time:f64,center:Quat64)->Vector3<f64> {
        let (&first_t,&first)=quats.first_key_value().unwrap();let (&last_t,&last)=quats.last_key_value().unwrap();
        if time<=first_t as f64 {return (center.inverse()*first).scaled_axis();}
        if time>=last_t as f64 {return (center.inverse()*last).scaled_axis();}
        let (&a,&qa)=quats.range(..=time.floor() as i64).next_back().unwrap();
        let (&b,&mut_qb)=quats.range((std::ops::Bound::Excluded(a),std::ops::Bound::Unbounded)).next().unwrap();
        let mut qb=mut_qb;if qa.coords.dot(&qb.coords)<0.0 {qb=Quat64::new_unchecked(-qb.into_inner());}
        let local=center.inverse()*qa.slerp(&qb,(time-a as f64)/(b-a) as f64);
        let ra=center.inverse()*qa;let rb=center.inverse()*qb;
        if ra.coords[3]==0.0 && rb.coords[3]==0.0 {
            let middle=(center.inverse()*qa.slerp(&qb,0.5)).imag();
            let mut axis=0;for i in 1..3 {if middle[i].abs()>middle[axis].abs() {axis=i;}}
            let sign=if middle[axis]<0.0 {-1.0} else {1.0};
            return local.imag().normalize()*(sign*std::f64::consts::PI);
        }
        local.scaled_axis()
    }

    // Independent dense three-point Gaussian quadrature, retaining source knots and declared cut times.
    fn dense_reference_node(quats:&TimeQuat,cutoff:f64,index:i64,extra_cuts:&[f64])->Quat64 {
        let sigma=0.1325/cutoff*1e6;let center=index as f64*sigma/8.0;
        let reference=GyroSource::clamped_quat_at_gyro_timestamp(quats,center/1000.0);
        let (lo,hi)=(center-3.0*sigma,center+3.0*sigma);
        let mut boundaries=vec![lo,hi];
        boundaries.extend(quats.keys().map(|v|*v as f64).filter(|v|*v>lo && *v<hi));
        boundaries.extend(extra_cuts.iter().copied().filter(|v|*v>lo && *v<hi));
        boundaries.sort_by(f64::total_cmp);boundaries.dedup();
        let mut sum=Vector3::zeros();let mut mass=0.0;
        for span in boundaries.windows(2) {
            let count=((span[1]-span[0])/(sigma/128.0).min(250.0)).ceil().max(1.0) as usize;
            let width=(span[1]-span[0])/count as f64;
            for i in 0..count {
                let mid=span[0]+(i as f64+0.5)*width;
                for (node,weight) in [(-(3.0f64/5.0).sqrt(),5.0/9.0),(0.0,8.0/9.0),((3.0f64/5.0).sqrt(),5.0/9.0)] {
                    let time=mid+node*width/2.0;
                    let w=weight*width/(2.0*sigma)*(-0.5*((time-center)/sigma).powi(2)).exp();
                    sum+=reference_pose(quats,time,reference)*w;mass+=w;
                }
            }
        }
        reference*Quat64::from_scaled_axis(sum/mass)
    }

    #[test]
    fn prior_irregular_noncommuting_input_matches_dense_integral() {
        let mut quats=TimeQuat::new();let mut time=-2_000_000i64;let mut i=0;
        while time<=2_000_000 {
            let t=time as f64/1e6;
            quats.insert(time,Quat64::from_euler_angles(0.07*(3.7*t).sin(),0.05*(2.1*t).cos(),0.03*(5.3*t).sin()));
            time+=[700,1300,3100,5300][i%4];i+=1;
        }
        let mut sampler=PriorSampler::new(&quats,0.5).unwrap();
        for index in [-2,0,3] {
            let actual=sampler.lowpass(index,&||false).unwrap();
            let expected=dense_reference_node(&quats,0.5,index,&[]);
            let error=(actual.inverse()*expected).angle();println!("prior irregular node={index} error_rad={error}");
            assert!(error<=1e-9);
        }
    }

    #[test]
    fn prior_short_input_clamps_both_tails_and_single_pose_is_zero() {
        let quats:TimeQuat=[(0,Vector3::new(0.03,0.0,0.0)),(7000,Vector3::new(0.01,0.02,0.0)),(31000,Vector3::new(0.0,0.03,0.01)),(73000,Vector3::new(-0.01,0.01,0.02))].into_iter().map(|(t,v)|(t,Quat64::from_scaled_axis(v))).collect();
        let mut sampler=PriorSampler::new(&quats,0.5).unwrap();
        for index in [-10,-1,0,2,10] {
            let actual=sampler.lowpass(index,&||false).unwrap();let expected=dense_reference_node(&quats,0.5,index,&[]);
            let error=(actual.inverse()*expected).angle();println!("prior clamp node={index} error_rad={error}");assert!(error<=1e-9);
        }
        let single:TimeQuat=[(1000,Quat64::from_euler_angles(0.7,-0.2,0.4))].into_iter().collect();
        assert_eq!(prior(&single,0.05,&[-1e12,0.0,1e12]),vec![Vector3::zeros();3]);
    }

    #[test]
    fn prior_pi_cuts_inside_segments_and_at_source_nodes_match_dense_reference() {
        for cut in [0.700123,0.7] {
            let angle=|t:f64|std::f64::consts::PI/cut*t+0.2*t*(t-cut);
            let data:Vec<_>=(-2000..=2000).map(|i|(i*1000,angle(i as f64/1000.0))).collect();
            let quats:TimeQuat=data.iter().map(|(t,v)|(*t,Quat64::from_scaled_axis(Vector3::new(0.0,*v,0.0)))).collect();
            let mut cuts=Vec::new();
            for segment in data.windows(2) {for target in [-std::f64::consts::PI,std::f64::consts::PI] {
                if (segment[0].1-target)*(segment[1].1-target)<0.0 {
                    cuts.push(segment[0].0 as f64+(segment[1].0-segment[0].0) as f64*(target-segment[0].1)/(segment[1].1-segment[0].1));
                }
            }}
            let mut sampler=PriorSampler::new(&quats,0.5).unwrap();
            let actual=sampler.lowpass(0,&||false).unwrap();let expected=dense_reference_node(&quats,0.5,0,&cuts);
            let error=(actual.inverse()*expected).angle();println!("prior pi cut={cut} error_rad={error} panels={}",sampler.panels);assert!(error<=1e-9);
        }
    }

    #[test]
    fn prior_whole_cut_axes_have_one_deterministic_original_segment_branch() {
        for changing in [false,true] {
            let half=|fraction:f64| {
                let axis=if changing {Vector3::new(1.0-fraction*0.4,fraction*0.8,0.0).normalize()} else {Vector3::new(-1.0,0.0,0.0)};
                Quat64::new_unchecked(nalgebra::Quaternion::from_parts(0.0,axis))
            };
            let quats:TimeQuat=(-1000..=2000).map(|ms| {
                let q=if ms<=0 || ms>=1000 {Quat64::identity()} else if ms<200 {Quat64::identity().slerp(&half(0.0),ms as f64/200.0)} else if ms<=600 {half((ms-200) as f64/400.0)} else {half(1.0).slerp(&Quat64::identity(),(ms-600) as f64/400.0)};
                (ms*1000,q)
            }).collect();
            let mut sampler=PriorSampler::new(&quats,0.5).unwrap();
            let actual=sampler.lowpass(0,&||false).unwrap();let expected=dense_reference_node(&quats,0.5,0,&[]);
            let error=(actual.inverse()*expected).angle();println!("prior whole cut changing={changing} error_rad={error}");assert!(error<=1e-9);
            let one=prior(&quats,0.5,&[0.0])[0];let batch=prior(&quats,0.5,&[800_000.0,0.0,-700_000.0]);assert!((one-batch[1]).norm()<=1e-12);
            let flipped:TimeQuat=quats.iter().map(|(&t,q)|(t,Quat64::new_unchecked(-q.into_inner()))).collect();
            assert!((one-prior(&flipped,0.5,&[0.0])[0]).norm()<=1e-12);
        }
    }

    #[test]
    fn prior_high_cutoff_subdivides_and_cache_reuse_does_not_refilter() {
        let quats:TimeQuat=(-100..=100).map(|i|(i*500,Quat64::from_euler_angles(0.01*(i as f64*0.07).sin(),0.015*(i as f64*0.09).cos(),0.0))).collect();
        let mut sampler=PriorSampler::new(&quats,50.0).unwrap();
        let actual=sampler.lowpass(0,&||false).unwrap();let expected=dense_reference_node(&quats,50.0,0,&[]);
        assert!((actual.inverse()*expected).angle()<=1e-9);
        assert!(sampler.panels>=62,"500us source segments must be subdivided at fc50");
        let values=sampler.sample(&[0.0,1000.0,2000.0],&||false).unwrap();let count=sampler.contributions;
        assert_eq!(sampler.sample(&[2000.0,0.0,1000.0],&||false).unwrap(),vec![values[2],values[0],values[1]]);
        assert_eq!(sampler.contributions,count);
        let mut table=reconstruction(0.1);table.cutoff_hz=50.0;
        table.rebuild_with_prior(&mut sampler,&StabReconConfig::DEFAULT,&||false).unwrap();let count=sampler.contributions;
        table.rebuild_with_prior(&mut sampler,&StabReconConfig::DEFAULT,&||false).unwrap();assert_eq!(sampler.contributions,count);
    }

    #[test]
    fn prior_cancellation_checks_contributions_and_keeps_the_old_table() {
        let quats=rotations(1,3.0);let mut sampler=PriorSampler::new(&quats,0.05).unwrap();let checks=std::cell::Cell::new(0);
        let result=sampler.sample(&[10e6],&|| {checks.set(checks.get()+1);checks.get()>=3});
        assert_eq!(result.unwrap_err(),PriorError::Cancelled);assert!(sampler.contributions<=1024);
        let mut table=reconstruction(0.1);table.rebuild(&quats,&StabReconConfig::DEFAULT);let old=table.table.clone();
        let mut sampler=PriorSampler::new(&quats,table.cutoff_hz).unwrap();
        assert_eq!(table.rebuild_with_prior(&mut sampler,&StabReconConfig::DEFAULT,&||true).unwrap_err(),PriorError::Cancelled);
        assert!(Arc::ptr_eq(&old,&table.table));
    }

}
