// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION_CODE_EPOCH: u64 = 1_577_836_800; // 2020-01-01 UTC
const CI_VERSION_CODE_BASE: u64 = 1_000_000_000;
const MAX_VERSION_CODE: u64 = 2_100_000_000;

#[derive(Debug, PartialEq, Eq)]
pub struct AndroidVersion {
    pub name: String,
    pub code: u64,
}

pub fn resolve(
    base: &str,
    minimum_code: &str,
    run_number: Option<&str>,
    name_override: Option<&str>,
    code_override: Option<&str>,
    unix_seconds: u64,
) -> Result<AndroidVersion, String> {
    let parts = base.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err("Mobile release version must contain three integers".into());
    }
    let minimum = parse_code(minimum_code)?;
    let sequence = run_number
        .map(|value| {
            value
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or("GitHub run number must be a positive integer")
        })
        .transpose()?;
    let code = match code_override {
        Some(value) => parse_code(value)?,
        None if sequence.is_some() => CI_VERSION_CODE_BASE
            .checked_add(sequence.unwrap())
            .filter(|code| *code <= MAX_VERSION_CODE)
            .ok_or("GitHub run number exceeds the Android version code limit")?,
        None => unix_seconds
            .checked_sub(VERSION_CODE_EPOCH)
            .filter(|code| *code > minimum && *code < CI_VERSION_CODE_BASE)
            .ok_or("Cannot generate Android version code from the current time")?,
    };
    if code <= minimum {
        return Err("Android version code must be above the legacy minimum".into());
    }
    if name_override.is_some_and(|name| name != base) {
        return Err("Android version name must match the shared mobile release version".into());
    }
    let name = base.to_owned();
    Ok(AndroidVersion { name, code })
}

fn parse_code(value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        return Err("Android version code must contain only digits".into());
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|code| (1..=MAX_VERSION_CODE).contains(code))
        .ok_or_else(|| "Android version code must be a positive Play-compatible integer".into())
}

pub fn from_env(base: &str, minimum_code: &str) -> Result<AndroidVersion, String> {
    let run = env::var("GITHUB_RUN_NUMBER").ok();
    let name = env::var("NIYIEN_ANDROID_VERSION_NAME").ok();
    let code = env::var("NIYIEN_ANDROID_VERSION_CODE").ok();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "System time is before the Unix epoch")?
        .as_secs();
    resolve(
        base,
        minimum_code,
        run.as_deref(),
        name.as_deref(),
        code.as_deref(),
        now,
    )
}

// The packaging recipe runs this same resolver before compiling the app.
#[allow(dead_code)]
fn main() {
    let args = env::args().collect::<Vec<_>>();
    let result = if args.len() == 3 {
        from_env(&args[1], &args[2])
    } else {
        Err("Usage: android-version <base-version> <legacy-minimum-code>".into())
    };
    match result {
        Ok(version) => println!(
            "{{\"version\":\"{}\",\"version_code\":{}}}",
            version.name, version.code
        ),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: u64 = VERSION_CODE_EPOCH + 200_000_000;

    #[test]
    fn ci_builds_keep_the_shared_release_version() {
        let first = resolve("1.0.5", "100", Some("151"), None, None, NOW).unwrap();
        let next = resolve("1.0.5", "100", Some("152"), None, None, NOW - 60).unwrap();
        assert_eq!(first.name, "1.0.5");
        assert_eq!(next.name, "1.0.5");
        assert!(next.code > first.code);
        assert!(first.code > 100);
        assert_eq!(first.code, 1_000_000_151);
    }

    #[test]
    fn ci_updates_outrank_local_test_builds() {
        let local = resolve("1.0.0", "100", None, None, None, NOW).unwrap();
        let ci = resolve("1.0.0", "100", Some("153"), None, None, NOW + 1).unwrap();
        assert_eq!(local.name, "1.0.0");
        assert!(ci.code > local.code);
    }

    #[test]
    fn pinned_packaging_values_survive_the_compilation_delay() {
        let packaged = resolve("1.0.0", "100", Some("151"), None, None, NOW).unwrap();
        let code = packaged.code.to_string();
        assert_eq!(
            resolve(
                "1.0.0",
                "100",
                Some("151"),
                Some(&packaged.name),
                Some(&code),
                NOW + 600
            )
            .unwrap(),
            packaged
        );
    }

    #[test]
    fn release_versions_can_skip_numbers_but_cannot_diverge() {
        assert_eq!(
            resolve("1.1.2", "100", Some("3"), None, Some("101"), NOW)
                .unwrap()
                .name,
            "1.1.2"
        );
        assert_eq!(
            resolve("1.2.3", "100", None, Some("1.2.3"), Some("102"), NOW)
                .unwrap()
                .name,
            "1.2.3"
        );
        assert!(resolve("1.0.5", "100", None, Some("1.0.6"), Some("102"), NOW).is_err());
    }

    #[test]
    fn invalid_or_legacy_codes_are_rejected() {
        for code in ["", "0", "100", "-1", "2100000001", "1.0"] {
            assert!(
                resolve("1.0.0", "100", None, None, Some(code), NOW).is_err(),
                "{code}"
            );
        }
        assert!(resolve("1.0.0", "100", Some("invalid"), None, None, NOW).is_err());
        assert!(resolve("1.0", "100", None, None, None, NOW).is_err());
        assert!(resolve("1.0.0", "100", None, Some("bad\"version"), None, NOW).is_err());
    }
}
