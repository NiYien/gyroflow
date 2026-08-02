// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

//! DST-aware timezone offset computation for the device clock.
//!
//! The timezone picker table (`src/ui/menu/device_timezones.js`) carries an
//! IANA tz ID per city; the static `offsetMinutes` in that table is only a
//! fallback for IDs this build cannot resolve. Offsets are computed at call
//! time from the bundled IANA database, so DST transitions (including the
//! 30/45-minute zones) are reflected without hand-maintained rules.

use std::sync::OnceLock;

use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;

/// Sentinel handed to QML when a tz ID cannot be resolved. QML falls back to
/// the static `offsetMinutes` from the picker table when it sees this value.
pub const OFFSET_SENTINEL: i32 = i32::MIN;

/// UTC offset in minutes for `tz_id` at instant `at`, or `None` when the ID
/// is not a valid IANA timezone name.
pub fn offset_minutes_for_tz_at(tz_id: &str, at: DateTime<Utc>) -> Option<i32> {
    let tz: Tz = tz_id.trim().parse().ok()?;
    Some(at.with_timezone(&tz).offset().fix().local_minus_utc() / 60)
}

/// UTC offset in minutes for `tz_id` right now. Unresolvable IDs return
/// `OFFSET_SENTINEL` with a warn log — never a silently wrong offset.
pub fn offset_minutes_for_tz_now(tz_id: &str) -> i32 {
    match offset_minutes_for_tz_at(tz_id, Utc::now()) {
        Some(minutes) => minutes,
        None => {
            log::warn!(
                "Unknown IANA timezone id {tz_id:?}; caller falls back to the static table offset"
            );
            OFFSET_SENTINEL
        }
    }
}

/// `GYROFLOW_NIYIEN_AUTO_SYNC_TIME` kill-switch parsing. Default is ON; the
/// historical opt-in spelling `=1` stays ON so users who already set it keep
/// the behaviour they asked for.
pub fn parse_auto_sync_flag(raw: Option<&str>) -> (bool, &'static str) {
    match raw.map(|value| value.trim().to_ascii_lowercase()) {
        None => (true, "default"),
        Some(value) if matches!(value.as_str(), "0" | "off" | "false" | "no") => (false, "env"),
        Some(value) if matches!(value.as_str(), "1" | "on" | "true" | "yes") => (true, "env"),
        Some(_) => (true, "default_invalid"),
    }
}

/// Whether the connect-time automatic device time sync is enabled.
pub fn auto_sync_time_enabled() -> bool {
    static RESOLVED: OnceLock<bool> = OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_NIYIEN_AUTO_SYNC_TIME").ok();
        let (enabled, source) = parse_auto_sync_flag(raw.as_deref());
        log::info!(target: "app", "NiYien: auto time sync resolved: enabled={enabled} source={source}");
        enabled
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    #[test]
    fn dst_city_flips_between_winter_and_summer() {
        assert_eq!(offset_minutes_for_tz_at("America/Los_Angeles", at(2026, 1, 15)), Some(-480));
        assert_eq!(offset_minutes_for_tz_at("America/Los_Angeles", at(2026, 7, 15)), Some(-420));
        // Southern hemisphere: DST is active in January.
        assert_eq!(offset_minutes_for_tz_at("Australia/Sydney", at(2026, 1, 15)), Some(660));
        assert_eq!(offset_minutes_for_tz_at("Australia/Sydney", at(2026, 7, 15)), Some(600));
    }

    #[test]
    fn non_whole_hour_zones_resolve() {
        // +8:45 year-round, no DST.
        assert_eq!(offset_minutes_for_tz_at("Australia/Eucla", at(2026, 1, 15)), Some(525));
        assert_eq!(offset_minutes_for_tz_at("Australia/Eucla", at(2026, 7, 15)), Some(525));
        // +12:45 standard, +13:45 during southern-summer DST.
        assert_eq!(offset_minutes_for_tz_at("Pacific/Chatham", at(2026, 7, 15)), Some(765));
        assert_eq!(offset_minutes_for_tz_at("Pacific/Chatham", at(2026, 1, 15)), Some(825));
    }

    #[test]
    fn dst_free_zone_is_stable() {
        assert_eq!(offset_minutes_for_tz_at("Asia/Shanghai", at(2026, 1, 15)), Some(480));
        assert_eq!(offset_minutes_for_tz_at("Asia/Shanghai", at(2026, 7, 15)), Some(480));
    }

    #[test]
    fn invalid_ids_yield_sentinel() {
        assert_eq!(offset_minutes_for_tz_at("Not/AZone", at(2026, 1, 15)), None);
        assert_eq!(offset_minutes_for_tz_at("", at(2026, 1, 15)), None);
        assert_eq!(offset_minutes_for_tz_now("Not/AZone"), OFFSET_SENTINEL);
    }

    #[test]
    fn auto_sync_flag_defaults_on_and_honours_kill_switch() {
        assert_eq!(parse_auto_sync_flag(None), (true, "default"));
        assert_eq!(parse_auto_sync_flag(Some("0")), (false, "env"));
        assert_eq!(parse_auto_sync_flag(Some("off")), (false, "env"));
        assert_eq!(parse_auto_sync_flag(Some("False")), (false, "env"));
        assert_eq!(parse_auto_sync_flag(Some(" no ")), (false, "env"));
        // The historical opt-in spelling stays enabled.
        assert_eq!(parse_auto_sync_flag(Some("1")), (true, "env"));
        assert_eq!(parse_auto_sync_flag(Some("on")), (true, "env"));
        assert_eq!(parse_auto_sync_flag(Some("gibberish")), (true, "default_invalid"));
    }

    // --- Guard over the QML-side picker table ---

    fn timezone_table_source() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("ui")
            .join("menu")
            .join("device_timezones.js");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"))
    }

    /// Literal `field: "value"` occurrences, the only shape the table uses.
    fn extract_string_fields(source: &str, field: &str) -> Vec<String> {
        let needle = format!("{field}: \"");
        let mut out = Vec::new();
        let mut rest = source;
        while let Some(pos) = rest.find(&needle) {
            rest = &rest[pos + needle.len()..];
            if let Some(end) = rest.find('"') {
                out.push(rest[..end].to_string());
            }
        }
        out
    }

    #[test]
    fn every_city_in_the_picker_table_has_a_resolvable_tz_id() {
        let source = timezone_table_source();
        let keys = extract_string_fields(&source, "key");
        let tz_ids = extract_string_fields(&source, "tzId");
        // Without this floor the test passes vacuously when the extractor
        // silently stops matching the table's syntax.
        assert!(
            keys.len() >= 60,
            "extractor found only {} city keys; it is probably broken",
            keys.len()
        );
        assert_eq!(
            keys.len(),
            tz_ids.len(),
            "every city choice in device_timezones.js must carry a tzId field"
        );
        for tz_id in &tz_ids {
            assert!(
                tz_id.parse::<Tz>().is_ok(),
                "device_timezones.js carries unresolvable tzId {tz_id:?}"
            );
        }
    }
}
