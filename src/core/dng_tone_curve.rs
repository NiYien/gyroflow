// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

// CinemaDNG tone curve.
//
// ffmpeg's tiff/dng decoder follows the DNG spec and applies the file's
// LinearizationTable, so the frames it hands out are scene-linear - it even
// tags the stream `transfer=linear`. That undoes the perceptually uniform log
// encoding the camera already applied. On a BMCC 2.5K frame the entire picture
// collapses into the bottom 4.2% of the linear range, and the 16->8 bit
// conversion that follows leaves ~10 distinct levels with ~90% of the pixels
// sitting at zero. The same camera's ProRes files do not have this problem:
// they store already-encoded log values (`transfer=bt709`), ffmpeg applies no
// inverse transform, and the picture merely looks flat instead of black.
//
// This module rebuilds the inverse mapping from the file's own tags, so the
// preview and the optical-flow sync input see a signal spread across the range
// again (measured on that frame: 10 -> 160+ distinct GRAY8 levels).
//
// Deliberate constraints:
//   * The curve only ever feeds the preview and the sync input. Export is not
//     routed through it, so the mapping is a fixed perceptual one rather than a
//     colorimetrically correct render.
//   * It must stay scene-independent. Per-frame auto exposure would make the
//     preview flicker and would inject brightness jumps into optical flow.
//   * It restores levels, not brightness. An underexposed clip stays dark; that
//     is a property of the footage and must not be compensated here.

use telemetry_parser::tiff_ifd;

// DNG IFD0 tags this module needs.
const TAG_BITS_PER_SAMPLE: u16 = 0x0102;
const TAG_LINEARIZATION_TABLE: u16 = 0xC618;
const TAG_BLACK_LEVEL: u16 = 0xC61A;
const TAG_WHITE_LEVEL: u16 = 0xC61D;

/// The curve is indexed by the full 16-bit range ffmpeg's DNG output uses.
pub const CURVE_LEN: usize = 65536;

/// How much of the file to read while looking for IFD0 and the linearization
/// table. DNG writers place IFD0 metadata ahead of the image data, so a small
/// prefix is enough - but `tiff_ifd::parse_ifd_entries` only reports a tag when
/// its payload is fully inside the slice, so the prefix has to comfortably
/// cover the table itself (4096 SHORT entries = 8 KiB in the BMCC case).
const HEADER_PREFIX_LEN: usize = 1 << 20; // 1 MiB

/// Maps ffmpeg's scene-linear 16-bit DNG samples back onto a perceptually
/// uniform range, so the 16->8 bit conversion downstream keeps its levels.
#[derive(Clone)]
pub struct DngToneCurve {
    lut: Vec<u16>,
    /// The sensor's real bit depth, straight from the file's BitsPerSample.
    /// The UI needs this because the decoder's output format cannot supply it:
    /// ffmpeg widens the samples to a 16-bit CFA container, and MDK has no name
    /// for that layout at all, so the video info panel would otherwise fall back
    /// to reporting "8 bit" for a 12-bit raw source.
    bits_per_sample: u16,
}

impl DngToneCurve {
    /// Builds the curve from a DNG file. Returns `None` for anything this
    /// module must not touch - see `from_dng_bytes`.
    pub fn from_url(url: &str) -> Option<Self> {
        use std::io::Read;

        let mut file = match crate::filesystem::open_file(url, false, false) {
            Ok(f) => f,
            Err(e) => {
                log::debug!("dng_tone_curve: cannot open {url}: {e:?}");
                return None;
            }
        };
        let len = file.size.min(HEADER_PREFIX_LEN);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        if let Err(e) = file.read_exact(&mut buf) {
            log::debug!("dng_tone_curve: cannot read {url}: {e:?}");
            return None;
        }
        Self::from_dng_bytes(&buf)
    }

    /// Builds the curve from a DNG header prefix.
    ///
    /// Returns `None` when the data is not TIFF, or carries no
    /// LinearizationTable. That `None` is the gate the whole feature hangs off:
    /// containers that already store display-domain values (MOV/MP4/BRAW/R3D)
    /// and DNGs whose raw codes are already linear must keep their existing
    /// behaviour, because applying this curve to them would double-encode.
    pub fn from_dng_bytes(data: &[u8]) -> Option<Self> {
        if !tiff_ifd::is_tiff_header(data) {
            return None;
        }
        let is_le = tiff_ifd::detect_byte_order(data)?;
        let ifd0 = tiff_ifd::read_u32(data, 4, is_le)? as usize;

        let mut linearization: Option<Vec<u16>> = None;
        let mut black: Option<f64> = None;
        let mut white: Option<f64> = None;
        let mut bits: Option<f64> = None;

        tiff_ifd::parse_ifd_entries(data, ifd0, is_le, &mut |tag, typ, count, value_data, _| {
            match tag {
                TAG_LINEARIZATION_TABLE => {
                    // The spec types this as SHORT; ignore anything else rather
                    // than guessing at a layout we have never seen.
                    if typ == 3 && count >= 2 {
                        linearization = Some(read_u16_array(value_data, count, is_le));
                    }
                }
                TAG_BLACK_LEVEL => black = read_scalar(typ, value_data, is_le),
                TAG_WHITE_LEVEL => white = read_scalar(typ, value_data, is_le),
                // Count is 1 for a CFA source and 3 for a demosaiced one; every
                // sample shares the depth, so the first entry is enough.
                TAG_BITS_PER_SAMPLE => bits = read_scalar(typ, value_data, is_le),
                _ => {}
            }
        });

        let linearization = linearization?;

        // The table's own extremes are the only sane fallback: the tags are
        // optional, but a table without them still describes a usable range.
        let black = black.unwrap_or(0.0);
        let white = white.unwrap_or_else(|| *linearization.iter().max().unwrap_or(&u16::MAX) as f64);

        let lut = build_curve(&linearization, black, white)?;
        // 0 means "not stated"; callers treat that as unknown rather than
        // inventing a depth.
        let bits_per_sample = bits.filter(|b| *b > 0.0 && *b <= 32.0).unwrap_or(0.0) as u16;
        log::debug!(
            "dng_tone_curve: built from {} entries, black={black}, white={white}, bits={bits_per_sample}",
            linearization.len()
        );
        Some(Self { lut, bits_per_sample })
    }

    /// The source's real bit depth (0 when the file does not state one).
    pub fn bits_per_sample(&self) -> u16 {
        self.bits_per_sample
    }

    /// The full 16-bit lookup table, for handing down to consumers that apply
    /// it themselves (e.g. the preview player across the FFI boundary).
    pub fn lut(&self) -> &[u16] {
        &self.lut
    }

    /// Maps one scene-linear sample.
    #[inline]
    pub fn map(&self, linear: u16) -> u16 {
        self.lut[linear as usize]
    }

    /// Applies the curve to a buffer of native-endian 16-bit samples in place.
    pub fn apply_in_place(&self, samples: &mut [u16]) {
        for s in samples.iter_mut() {
            *s = self.lut[*s as usize];
        }
    }
}

impl std::fmt::Debug for DngToneCurve {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DngToneCurve").field("len", &self.lut.len()).finish()
    }
}

fn read_u16_array(data: &[u8], count: usize, is_le: bool) -> Vec<u16> {
    let n = count.min(data.len() / 2);
    (0..n)
        .map(|i| {
            let b = [data[i * 2], data[i * 2 + 1]];
            if is_le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) }
        })
        .collect()
}

/// Reads the first element of a TIFF value as a number. BlackLevel is allowed
/// to be SHORT, LONG or RATIONAL; WhiteLevel to be SHORT or LONG.
fn read_scalar(typ: u16, data: &[u8], is_le: bool) -> Option<f64> {
    let u16_at = |o: usize| -> Option<u16> { tiff_ifd::read_u16(data, o, is_le) };
    let u32_at = |o: usize| -> Option<u32> { tiff_ifd::read_u32(data, o, is_le) };
    match typ {
        3 => u16_at(0).map(|v| v as f64),
        4 => u32_at(0).map(|v| v as f64),
        5 => {
            let num = u32_at(0)? as f64;
            let den = u32_at(4)? as f64;
            if den == 0.0 { None } else { Some(num / den) }
        }
        _ => None,
    }
}

/// sRGB transfer function. Used as the display encode on top of the log-domain
/// value; picking one fixed OETF here keeps the mapping reproducible.
fn display_encode(x: f64) -> f64 {
    if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// Finds the raw code whose linearized value is `linear`, interpolating between
/// table entries. `lin` is non-decreasing, so the result is monotonic in `linear`.
fn raw_of_linear(lin: &[u16], linear: f64) -> f64 {
    let hi = lin.partition_point(|&v| (v as f64) < linear);
    if hi == 0 {
        0.0
    } else if hi >= lin.len() {
        (lin.len() - 1) as f64
    } else {
        let (a, b) = (lin[hi - 1] as f64, lin[hi] as f64);
        if b > a { (hi - 1) as f64 + (linear - a) / (b - a) } else { (hi - 1) as f64 }
    }
}

/// Inverts the DNG linearization and re-encodes for display.
///
/// ffmpeg produced its sample as `(lin[raw] - black) / (white - black) * 65535`,
/// so walking that backwards recovers `raw` - the camera's own log code - and
/// normalising it against the black/white codes puts the picture back in the
/// domain the camera's non-raw formats already ship in.
fn build_curve(lin: &[u16], black: f64, white: f64) -> Option<Vec<u16>> {
    if lin.len() < 2 || !(white > black) {
        return None;
    }
    // Where black and white sit in the raw code domain. These must come from
    // the same interpolated lookup the loop below uses, otherwise the curve's
    // endpoints drift off 0 and full scale.
    let raw_black = raw_of_linear(lin, black);
    let raw_white = raw_of_linear(lin, white);
    if !(raw_white > raw_black) {
        log::debug!("dng_tone_curve: degenerate raw range {raw_black}..{raw_white}, skipping");
        return None;
    }

    let span = white - black;
    let raw_span = raw_white - raw_black;
    let mut lut = vec![0u16; CURVE_LEN];
    for (i, out) in lut.iter_mut().enumerate() {
        // Back to the linearized value ffmpeg started from, then to the raw code.
        let linear = (i as f64 / (CURVE_LEN - 1) as f64) * span + black;
        let raw = raw_of_linear(lin, linear);
        let t = ((raw - raw_black) / raw_span).clamp(0.0, 1.0);
        *out = (display_encode(t) * u16::MAX as f64).round().clamp(0.0, u16::MAX as f64) as u16;
    }
    Some(lut)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A log-ish table shaped like the BMCC one: slow at the bottom, saturating
    /// well before the last entry.
    fn synth_table(n: usize) -> Vec<u16> {
        (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                let v = 65535.0 * ((t * 6.0).exp() - 1.0) / (6.0f64.exp() - 1.0);
                v.min(65535.0) as u16
            })
            .collect()
    }

    /// Minimal little-endian TIFF carrying LinearizationTable/BlackLevel/WhiteLevel.
    fn synth_dng(table: &[u16], black: u16, white: u16, with_table: bool) -> Vec<u8> {
        let entries: u16 = if with_table { 4 } else { 3 };
        // header(8) + count(2) + entries*12 + next-ifd(4)
        let table_offset = 8 + 2 + entries as usize * 12 + 4;
        let mut d = Vec::new();
        d.extend_from_slice(b"II");
        d.extend_from_slice(&42u16.to_le_bytes());
        d.extend_from_slice(&8u32.to_le_bytes());
        d.extend_from_slice(&entries.to_le_bytes());

        let mut entry = |tag: u16, typ: u16, count: u32, payload: u32, d: &mut Vec<u8>| {
            d.extend_from_slice(&tag.to_le_bytes());
            d.extend_from_slice(&typ.to_le_bytes());
            d.extend_from_slice(&count.to_le_bytes());
            d.extend_from_slice(&payload.to_le_bytes());
        };
        // TIFF wants entries in ascending tag order, and 0x0102 is the lowest here.
        entry(TAG_BITS_PER_SAMPLE, 3, 1, 12, &mut d);
        if with_table {
            entry(TAG_LINEARIZATION_TABLE, 3, table.len() as u32, table_offset as u32, &mut d);
        }
        entry(TAG_BLACK_LEVEL, 3, 1, black as u32, &mut d);
        entry(TAG_WHITE_LEVEL, 3, 1, white as u32, &mut d);
        d.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        assert_eq!(d.len(), table_offset);
        for v in table {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d
    }

    #[test]
    fn curve_is_monotonic_with_correct_endpoints() {
        let table = synth_table(4096);
        let bytes = synth_dng(&table, 256, 60074, true);
        let curve = DngToneCurve::from_dng_bytes(&bytes).expect("curve should build");

        assert_eq!(curve.lut().len(), CURVE_LEN);
        assert_eq!(curve.map(0), 0, "black maps to 0");
        assert_eq!(curve.map(u16::MAX), u16::MAX, "white maps to full scale");
        assert!(
            curve.lut().windows(2).all(|w| w[1] >= w[0]),
            "curve must be monotonic non-decreasing"
        );
    }

    #[test]
    fn curve_lifts_the_bottom_of_the_linear_range() {
        // The whole point: values crushed near zero in the linear domain must
        // come back spread out, otherwise the 8-bit conversion kills them.
        let table = synth_table(4096);
        let bytes = synth_dng(&table, 256, 60074, true);
        let curve = DngToneCurve::from_dng_bytes(&bytes).unwrap();

        let linear_8bit = |v: u16| (v as u32 * 255 / 65535) as u8;
        let curved_8bit = |v: u16| (curve.map(v) as u32 * 255 / 65535) as u8;

        // Sample the bottom 4% of the linear range - where the BMCC frame lives.
        let probes: Vec<u16> = (0..64).map(|i| (i * 65535 / 64 / 25) as u16).collect();
        let linear_levels: std::collections::HashSet<u8> = probes.iter().map(|&v| linear_8bit(v)).collect();
        let curved_levels: std::collections::HashSet<u8> = probes.iter().map(|&v| curved_8bit(v)).collect();
        assert!(
            curved_levels.len() > linear_levels.len() * 4,
            "curve should spread the bottom of the range: {} -> {}",
            linear_levels.len(),
            curved_levels.len()
        );
    }

    #[test]
    fn source_bit_depth_comes_from_the_file_not_the_decoder() {
        // The decoder hands out a 16-bit CFA container for a 12-bit sensor, so
        // the only truthful source for the UI is the file's own BitsPerSample.
        let table = synth_table(4096);
        let curve = DngToneCurve::from_dng_bytes(&synth_dng(&table, 256, 60074, true)).unwrap();
        assert_eq!(curve.bits_per_sample(), 12);
    }

    #[test]
    fn no_linearization_table_yields_no_curve() {
        let table = synth_table(4096);
        let bytes = synth_dng(&table, 256, 60074, false);
        assert!(
            DngToneCurve::from_dng_bytes(&bytes).is_none(),
            "a DNG without a LinearizationTable must not get a curve"
        );
    }

    #[test]
    fn non_tiff_input_yields_no_curve() {
        assert!(DngToneCurve::from_dng_bytes(b"not a tiff at all").is_none());
        assert!(DngToneCurve::from_dng_bytes(&[]).is_none());
    }

    #[test]
    fn degenerate_levels_yield_no_curve() {
        let table = synth_table(4096);
        // white <= black must not produce a curve
        let bytes = synth_dng(&table, 60074, 256, true);
        assert!(DngToneCurve::from_dng_bytes(&bytes).is_none());
    }
}
