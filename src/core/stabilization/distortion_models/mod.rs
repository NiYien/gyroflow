// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

mod insta360;
mod opencv_fisheye;
mod opencv_standard;
mod poly3;
mod poly5;
mod ptlens;
mod sony;

mod digital_stretch;
mod gopro6_superview;
mod gopro_hyperview;
mod gopro_superview;

use super::KernelParams;

macro_rules! impl_models {
    ($($name:ident => $class:ty,)*) => {
        #[derive(Clone)]
        pub enum DistortionModels {
            $($name($class),)*
        }
        impl Default for DistortionModels {
            fn default() -> Self { Self::OpenCVFisheye(Default::default()) }
        }
        #[derive(Default, Clone)]
        pub struct DistortionModel {
            pub inner: DistortionModels
        }
        impl DistortionModel {
            pub fn undistort_point(&self, point: (f32, f32), params: &KernelParams) -> Option<(f32, f32)> {
                match &self.inner {
                    $(DistortionModels::$name(m) => m.undistort_point(point, params),)*
                }
            }
            pub fn distort_point(&self, x: f32, y: f32, z: f32, params: &KernelParams) -> (f32, f32) {
                match &self.inner {
                    $(DistortionModels::$name(m) => m.distort_point(x, y, z, params),)*
                }
            }
            pub fn adjust_lens_profile(&self, profile: &mut crate::LensProfile) {
                match &self.inner {
                    $(DistortionModels::$name(m) => m.adjust_lens_profile(profile),)*
                }
            }
            pub fn radial_distortion_limit(&self, k: &[f64]) -> Option<f64> {
                let max_theta = std::f64::consts::FRAC_PI_2; // PI/2
                let mut low = 0.0;
                let mut high = max_theta;
                let tolerance = 1e-4;

                while high - low > tolerance {
                    let mid = (low + high) / 2.0;
                    let deriv = match &self.inner {
                        $(DistortionModels::$name(x) => { x.distortion_derivative(mid, k)? })*
                    };
                    if deriv > 0.0 {
                        low = mid;
                    } else {
                        high = mid;
                    }
                }

                let theta_max = (low + high) / 2.0;
                if (theta_max - max_theta).abs() > 0.001 {
                    Some(theta_max.tan())
                } else {
                    None
                }
            }

            pub fn id(&self)               -> &'static str { match &self.inner { $(DistortionModels::$name(_) => <$class>::id(),)* } }
            pub fn name(&self)             -> &'static str { match &self.inner { $(DistortionModels::$name(_) => <$class>::name(),)* } }
            pub fn opencl_functions(&self) -> &'static str { match &self.inner { $(DistortionModels::$name(x) => x.opencl_functions(),)* } }
            pub fn wgsl_functions(&self)   -> &'static str { match &self.inner { $(DistortionModels::$name(x) => x.wgsl_functions(),)* } }

            pub fn from_name(id: &str) -> Self {
                $(
                    if <$class>::id() == id { return Self { inner: DistortionModels::$name(Default::default()) }; }
                )*
                DistortionModel::default()
            }
        }
    };
}

impl_models! {
    // Physical lenses
    OpenCVFisheye  => opencv_fisheye::OpenCVFisheye,
    OpenCVStandard => opencv_standard::OpenCVStandard,
    Poly3          => poly3::Poly3,
    Poly5          => poly5::Poly5,
    PtLens         => ptlens::PtLens,
    Insta360       => insta360::Insta360,
    Sony           => sony::Sony,

    // Digital lenses (ie. post-processing)
    GoProSuperview => gopro_superview::GoProSuperview,
    GoPro6Superview => gopro6_superview::GoPro6Superview,
    GoProHyperview => gopro_hyperview::GoProHyperview,
    DigitalStretch => digital_stretch::DigitalStretch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distortion_model_from_name_empty_falls_through_to_default() {
        // Freezes the contract: an unrecognized name (here `""`, written verbatim by
        // niyien-lens-data anamorphic presets) falls through to `DistortionModel::default()`.
        // If anyone later adds a model whose `id()` is `""` it would shadow this fall-through
        // and silently change behavior; the assert below would then fail.
        assert_eq!(
            DistortionModel::from_name("").id(),
            DistortionModel::default().id()
        );
    }
}
