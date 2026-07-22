// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

use super::OpticalFlowPair;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

pub mod flow_gate;

/// Unknown optical-flow method ids already reported by [`OpticalFlowMethod::detect_features`].
///
/// `detect_features` runs once per sync frame, so an id that leaks into the
/// fallback arm would otherwise write one error line per frame — batch sync
/// multiplies that by clips and sync points, and `gyroflow-incidents.log` is
/// append-only until a successful upload, so the noise buries every real warning
/// in it. Deduplicating keeps the diagnostic without the flood.
///
/// Dedupe is per id, not global: a second leak carrying a different id still
/// leaves a record instead of being masked by whichever one happened to arrive
/// first.
static LOGGED_UNKNOWN_OF_METHODS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

/// Marks `method` as reported, returning whether this is the first time it was seen.
///
/// Only reached from the fallback arm, so the lock is never contended on a healthy
/// run and costs nothing measurable against the optical flow of the same frame.
fn claim_unknown_of_method_log(method: u32) -> bool {
    let logged = LOGGED_UNKNOWN_OF_METHODS.get_or_init(|| Mutex::new(HashSet::new()));
    // A poisoned lock means some other thread panicked mid-insert; recovering and
    // logging again beats going silent about an already-broken state.
    let mut logged = logged.lock().unwrap_or_else(|p| p.into_inner());
    logged.insert(method)
}

mod akaze;
pub use self::akaze::*;
mod opencv_dis;
pub use opencv_dis::*;
mod opencv_pyrlk;
pub use opencv_pyrlk::*;
#[cfg(any(feature = "neuflow-ort", neuflow_burn_enabled))]
mod neuflow;
#[cfg(any(feature = "neuflow-ort", neuflow_burn_enabled))]
pub use self::neuflow::*;
#[cfg(neuflow_burn_enabled)]
mod neuflow_burn;
#[cfg(feature = "neuflow-ort")]
mod neuflow_ort;

#[enum_delegate::register]
pub trait OpticalFlowTrait {
    fn size(&self) -> (u32, u32);
    fn features(&self) -> &Vec<(f32, f32)>;
    fn optical_flow_to(&self, to: &OpticalFlowMethod) -> OpticalFlowPair;
    fn cleanup(&mut self);
    fn can_cleanup(&self) -> bool;
    fn has_data(&self) -> bool {
        true
    }
}

#[cfg(any(feature = "neuflow-ort", neuflow_burn_enabled))]
#[enum_delegate::implement(OpticalFlowTrait)]
#[derive(Clone)]
pub enum OpticalFlowMethod {
    OFAkaze(OFAkaze),
    OFOpenCVPyrLK(OFOpenCVPyrLK),
    OFOpenCVDis(OFOpenCVDis),
    OFNeuFlowV2(OFNeuFlowV2),
}

#[cfg(not(any(feature = "neuflow-ort", neuflow_burn_enabled)))]
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
        #[cfg_attr(not(any(feature = "neuflow-ort", neuflow_burn_enabled)), allow(unused_variables))]
        frame_data: Option<Arc<Vec<u8>>>,
        width: u32,
        height: u32,
        #[cfg_attr(not(any(feature = "neuflow-ort", neuflow_burn_enabled)), allow(unused_variables))]
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
            #[cfg(neuflow_burn_enabled)]
            4 => Self::OFNeuFlowV2(OFNeuFlowV2::new(
                timestamp_us,
                frame_data.unwrap_or_else(|| Arc::new(Vec::new())),
                width,
                height,
                stride,
                4,
            )),
            _ => {
                // Logged once per id — see LOGGED_UNKNOWN_OF_METHODS for why.
                if claim_unknown_of_method_log(method) {
                    log::error!("Unknown OF method {method}, falling back to OpenCV DIS");
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(width: u32, height: u32) -> Arc<image::GrayImage> {
        Arc::new(image::GrayImage::new(width, height))
    }

    // LOGGED_UNKNOWN_OF_METHODS is process-wide and tests share it, so each test
    // below claims its own ids and never asserts on the set as a whole.

    #[test]
    fn unknown_of_method_falls_back_to_dis_on_every_call() {
        const UNKNOWN_METHOD: u32 = 4242;

        // Deduplicating the log must not deduplicate the fallback: every frame
        // still needs a usable optical flow, not just the first one.
        for frame in 0..64i64 {
            let result = OpticalFlowMethod::detect_features(
                UNKNOWN_METHOD,
                frame * 1000,
                gray(16, 16),
                None,
                16,
                16,
                16,
            );
            assert!(
                matches!(result, OpticalFlowMethod::OFOpenCVDis(_)),
                "frame {frame} with an unknown method id did not fall back to DIS"
            );
        }
    }

    #[test]
    fn unknown_of_method_is_logged_only_on_first_sighting() {
        assert!(
            claim_unknown_of_method_log(4243),
            "first sighting of an id must be logged"
        );

        for _ in 0..10_000 {
            assert!(
                !claim_unknown_of_method_log(4243),
                "an already-reported id must never be logged again"
            );
        }
    }

    #[test]
    fn distinct_unknown_of_methods_are_logged_independently() {
        assert!(claim_unknown_of_method_log(4244));
        assert!(!claim_unknown_of_method_log(4244));

        // A different id must still get through — dedupe is per id, so a second
        // leak is not masked by whichever one arrived first.
        assert!(claim_unknown_of_method_log(4245));
        assert!(!claim_unknown_of_method_log(4245));
    }
}
