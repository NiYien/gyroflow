// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>
// Ported from upstream gyroflow 322cb312 + eabdc789

pub mod solver;
pub mod odometry;

const MIN_BAND_POINTS: usize = 25;
