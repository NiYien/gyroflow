// SPDX-License-Identifier: GPL-3.0-or-later

//! Optical motion sync method (offset_method = 3): KLT feature tracks plus a
//! rotation-residual cost, independent of the optical-flow based methods.

pub mod config;
pub mod tracks;
