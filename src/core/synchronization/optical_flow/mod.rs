// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

use super::OpticalFlowPair;
use std::sync::Arc;

mod akaze;
pub use self::akaze::*;
mod opencv_dis;
pub use opencv_dis::*;
mod opencv_pyrlk;
pub use opencv_pyrlk::*;
#[cfg(any(feature = "neuflow-ort", feature = "neuflow-burn"))]
mod neuflow;
#[cfg(any(feature = "neuflow-ort", feature = "neuflow-burn"))]
pub use self::neuflow::*;
#[cfg(feature = "neuflow-ort")]
mod neuflow_ort;
#[cfg(feature = "neuflow-burn")]
mod neuflow_burn;

#[enum_delegate::register]
pub trait OpticalFlowTrait {
    fn size(&self) -> (u32, u32);
    fn features(&self) -> &Vec<(f32, f32)>;
    fn optical_flow_to(&self, to: &OpticalFlowMethod) -> OpticalFlowPair;
    fn cleanup(&mut self);
    fn can_cleanup(&self) -> bool;
    fn has_data(&self) -> bool { true }
}

#[cfg(any(feature = "neuflow-ort", feature = "neuflow-burn"))]
#[enum_delegate::implement(OpticalFlowTrait)]
#[derive(Clone)]
pub enum OpticalFlowMethod {
    OFAkaze(OFAkaze),
    OFOpenCVPyrLK(OFOpenCVPyrLK),
    OFOpenCVDis(OFOpenCVDis),
    OFNeuFlowV2(OFNeuFlowV2),
}

#[cfg(not(any(feature = "neuflow-ort", feature = "neuflow-burn")))]
#[enum_delegate::implement(OpticalFlowTrait)]
#[derive(Clone)]
pub enum OpticalFlowMethod {
    OFAkaze(OFAkaze),
    OFOpenCVPyrLK(OFOpenCVPyrLK),
    OFOpenCVDis(OFOpenCVDis),
}

impl OpticalFlowMethod {
    pub fn detect_features(
        method: u32,
        timestamp_us: i64,
        img: Arc<image::GrayImage>,
        frame_data: Option<Arc<Vec<u8>>>,
        width: u32,
        height: u32,
        stride: usize,
    ) -> Self {
        match method {
            0 => Self::OFAkaze(OFAkaze::detect_features(timestamp_us, img, width, height)),
            1 => Self::OFOpenCVPyrLK(OFOpenCVPyrLK::detect_features(
                timestamp_us,
                img,
                width,
                height,
            )),
            2 => Self::OFOpenCVDis(OFOpenCVDis::detect_features(
                timestamp_us,
                img,
                width,
                height,
            )),
            #[cfg(feature = "neuflow-ort")]
            3 => Self::OFNeuFlowV2(OFNeuFlowV2::new(
                timestamp_us,
                frame_data.clone().unwrap_or_else(|| Arc::new(Vec::new())),
                width,
                height,
                stride,
                3,
            )),
            #[cfg(feature = "neuflow-burn")]
            4 => Self::OFNeuFlowV2(OFNeuFlowV2::new(
                timestamp_us,
                frame_data.unwrap_or_else(|| Arc::new(Vec::new())),
                width,
                height,
                stride,
                4,
            )),
            _ => {
                log::error!("Unknown OF method {method}, falling back to OpenCV DIS");
                Self::OFOpenCVDis(OFOpenCVDis::detect_features(
                    timestamp_us,
                    img,
                    width,
                    height,
                ))
            }
        }
    }
}
