// SPDX-License-Identifier: GPL-3.0-or-later

//! Optical motion sync method (offset_method = 3): KLT feature tracks plus a
//! rotation-residual cost, independent of the optical-flow based methods.

pub mod bearings;
pub mod config;
pub mod cost;
pub mod quat_table;
pub mod search;
#[cfg(feature = "use-opencv")] pub mod tracker;
pub mod tracks;
#[cfg(test)] pub mod testutil;

use crate::synchronization::GrayImage;
use self::config::OpticalConfig;
use self::tracks::Observation;

/// Tracks kept alive per frame (upstream value)
pub const MAX_POINTS: usize = 1500;

/// Feature tracker fed the frames of one window in order; runs on a tracking thread
pub trait FrameTracker: Send {
    fn reset(&mut self);
    /// Observations between the previous frame and this one, with the size they are expressed in
    fn track(&mut self, img: &GrayImage) -> Result<(Vec<Observation>, (u32, u32)), String>;
}

/// None when built without `use-opencv`
pub fn default_tracker(cfg: &OpticalConfig) -> Option<Box<dyn FrameTracker>> {
    #[cfg(feature = "use-opencv")]
    { Some(Box::new(tracker::KltTracker::new(MAX_POINTS, cfg.replenish, cfg.track_width))) }
    #[cfg(not(feature = "use-opencv"))]
    { let _ = cfg; None }
}
