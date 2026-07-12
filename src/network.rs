// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use std::sync::OnceLock;
use std::time::{Duration, Instant};

// `timeout_recv_body` bounds how long receiving the *entire* response body may
// take. It is a total budget, NOT an idle/stall detector: do not size it like a
// per-chunk latency budget. The Windows app-update package zip alone is ~102 MB,
// which needs 100-170s on a ~1 MB/s link, so a "reasonable-looking" 30s would
// make every large download fail on a healthy connection. See
// `body_timeout_config` for how the default is calibrated.
//
// Without this timeout a stalled transfer — peer stops sending bytes but never
// closes the connection (half-open TCP, no RST) — blocks the `reader.read()`
// download loops in `distribution.rs` / `nle_plugins.rs` forever: frozen
// progress bar, no error, no log line, no self-recovery. `call_with_retry`
// cannot save it either, since it only wraps `.call()` (request + response
// headers) and the body phase runs entirely outside that cover.
pub fn configure<T>(request: ureq::RequestBuilder<T>) -> ureq::RequestBuilder<T> {
    request
        .config()
        .proxy(None)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(body_timeout_config())
        .build()
}

pub fn get(uri: impl AsRef<str>) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    configure(ureq::get(uri.as_ref()))
}

pub fn post(uri: impl AsRef<str>) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    configure(ureq::post(uri.as_ref()))
}

pub fn put(uri: impl AsRef<str>) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    configure(ureq::put(uri.as_ref()))
}

const BODY_TIMEOUT_DEFAULT_SECS: u64 = 600;
const BODY_TIMEOUT_MAX_SECS: u64 = 3600;

/// Pure parser for the body-timeout profile (extracted so it can be unit-tested
/// without the `OnceLock`/env-global side effects). Returns the resolved seconds
/// plus the source label used for logging; `0` seconds means "no timeout".
///
/// Default 600s; `0` disables; values above the 3600s cap are clamped; invalid
/// (non-numeric) values fall back to the default.
fn parse_body_timeout(raw: Option<&str>) -> (u64, &'static str) {
    match raw.map(str::trim) {
        None | Some("") => (BODY_TIMEOUT_DEFAULT_SECS, "default"),
        Some(value) => match value.parse::<u64>() {
            Ok(secs) if secs > BODY_TIMEOUT_MAX_SECS => (BODY_TIMEOUT_MAX_SECS, "env_clamped"),
            Ok(secs) => (secs, "env"),
            Err(_) => (BODY_TIMEOUT_DEFAULT_SECS, "default_invalid"),
        },
    }
}

/// Resolve the response-body receive timeout once. Shared by every request built
/// through `configure` (manifest fetch, NLE plugin download, app/lens/sdk update
/// download) — all of them stream their body through the same unguarded read
/// loop, so the bound belongs at this single choke point.
///
/// The default tolerates ~180 KB/s for the largest expected body (~102 MB), i.e.
/// a 3-6x margin over the ~1 MB/s measured in practice.
/// `NIYIEN_UPDATE_BODY_TIMEOUT_S=0` disables the timeout entirely, reproducing
/// the pre-change behavior byte-for-byte (emergency rollback path). Logged once
/// on first use.
fn body_timeout_config() -> Option<Duration> {
    static CONFIG: OnceLock<Option<Duration>> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        let raw = std::env::var("NIYIEN_UPDATE_BODY_TIMEOUT_S").ok();
        let (secs, source) = parse_body_timeout(raw.as_deref());
        match source {
            "env_clamped" => log::warn!(
                target: "update",
                "NIYIEN_UPDATE_BODY_TIMEOUT_S={} exceeds the {BODY_TIMEOUT_MAX_SECS}s cap, clamping",
                raw.as_deref().unwrap_or_default().trim()
            ),
            "default_invalid" => log::warn!(
                target: "update",
                "NIYIEN_UPDATE_BODY_TIMEOUT_S={:?} is not a valid number, falling back to {BODY_TIMEOUT_DEFAULT_SECS}s",
                raw.as_deref().unwrap_or_default().trim()
            ),
            _ => {}
        }
        // secs == 0 means "no body timeout"; it is logged as such so the disabled
        // state is greppable rather than silent.
        log::info!(
            target: "update",
            "body timeout resolved: timeout_s={secs} source={source}{}",
            if secs == 0 { " (disabled)" } else { "" }
        );
        (secs > 0).then(|| Duration::from_secs(secs))
    })
}

// ---------------------------------------------------------------------------
// Bounded retry for idempotent GET downloads (CN update resilience).
//
// CN app-update files stream from 123 cloud's direct-link CDN, which
// sporadically returns a transient 504 (cache-cold origin fetch or direct-link
// throttle). A single GET attempt turns that transient blip into a hard,
// user-visible update failure. Retrying the whole GET a few times with backoff
// recovers most cases, and the first attempt tends to warm the CDN cache so the
// retry hits an edge cache hit. Only safe (transient) failures are retried.
// ---------------------------------------------------------------------------

/// Resolve retry config once: (number of *retries* after the first attempt,
/// base backoff). `NIYIEN_UPDATE_DOWNLOAD_RETRIES=0` => single-shot, identical
/// to the pre-change behavior. Logged once on first use.
fn retry_config() -> (u32, Duration) {
    static CONFIG: OnceLock<(u32, Duration)> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        let retries = std::env::var("NIYIEN_UPDATE_DOWNLOAD_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(2)
            .min(10);
        let base_ms = std::env::var("NIYIEN_UPDATE_DOWNLOAD_RETRY_BASE_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(500);
        log::info!(
            target: "update",
            "download retry resolved: retries={retries} base_delay_ms={base_ms}"
        );
        (retries, Duration::from_millis(base_ms))
    })
}

/// Exponential backoff capped at 5s. With the default 500ms base the schedule
/// is 500ms, 2s, 5s (8s capped), ... per successive retry.
fn backoff_delay(base: Duration, retry_index: u32) -> Duration {
    let factor = 4u64.saturating_pow(retry_index);
    let ms = (base.as_millis() as u64)
        .saturating_mul(factor)
        .min(5_000);
    Duration::from_millis(ms)
}

/// True for transient failures that are safe to retry on an idempotent GET:
/// gateway 5xx (500/502/503/504), timeouts, and connection/DNS/TLS hiccups.
/// Definitive failures (4xx, protocol/config errors) return false so they are
/// surfaced immediately without retrying.
pub fn is_transient_error(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::StatusCode(code) => matches!(*code, 500 | 502 | 503 | 504),
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Tls(_) => true,
        _ => false,
    }
}

/// Run `attempt` (an idempotent GET `.call()`) up to `retries + 1` times,
/// retrying only on `is_transient_error` with exponential backoff. Non-transient
/// errors and the final attempt's error are returned immediately. `label` is
/// only used for logging (target `update`). Uses the shared app/update retry
/// profile (`retry_config`); delegates to `call_with_retry_config`.
pub fn call_with_retry<T, F>(label: &str, attempt: F) -> Result<T, ureq::Error>
where
    F: FnMut() -> Result<T, ureq::Error>,
{
    let (retries, base) = retry_config();
    call_with_retry_config(label, retries, base, attempt)
}

/// Core retry loop parameterized by an explicit `(retries, base)` budget so
/// different download paths can use independent retry profiles (e.g. the more
/// aggressive plugin profile) while sharing the transient-error classification
/// and exponential backoff. Behavior is identical to the previous inline loop
/// in `call_with_retry`.
pub fn call_with_retry_config<T, F>(
    label: &str,
    retries: u32,
    base: Duration,
    mut attempt: F,
) -> Result<T, ureq::Error>
where
    F: FnMut() -> Result<T, ureq::Error>,
{
    let mut tries = 0u32;
    loop {
        match attempt() {
            Ok(value) => {
                if tries > 0 {
                    log::info!(
                        target: "update",
                        "download {label} recovered after {tries} retr{}",
                        if tries == 1 { "y" } else { "ies" }
                    );
                }
                return Ok(value);
            }
            Err(err) => {
                if tries >= retries || !is_transient_error(&err) {
                    return Err(err);
                }
                let delay = backoff_delay(base, tries);
                log::warn!(
                    target: "update",
                    "download {label} transient error (attempt {}/{}): {err}; retrying in {delay:?}",
                    tries + 1,
                    retries + 1
                );
                tries += 1;
                std::thread::sleep(delay);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin-download resilience.
//
// CN NLE plugin zips stream from the same 123 cloud direct-link CDN as app
// updates, whose cold origin fetch sporadically returns a transient 504 (each
// cold attempt blocks ~11s on the CDN gateway timeout before failing). Plugin
// install is a user-initiated, low-frequency action, so it can afford a more
// aggressive retry budget than the background app-update path, plus a cold-edge
// prewarm to nudge the CDN before the full download. Both are env-tunable; set
// retries=0 + prewarm off to fall back to the pre-change single-shot behavior.
// ---------------------------------------------------------------------------

/// Pure parser for the plugin retry profile (extracted so it can be unit-tested
/// without the `OnceLock`/env-global side effects). retries default 3, clamped
/// to 10; base default 1000ms, with 0/invalid falling back to the default.
fn parse_plugin_retry_config(retries: Option<&str>, base_ms: Option<&str>) -> (u32, Duration) {
    let retries = retries
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(3)
        .min(10);
    let base_ms = base_ms
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(1000);
    (retries, Duration::from_millis(base_ms))
}

/// Resolve the plugin-download retry profile once: `(retries, base)`. Kept
/// independent from `retry_config` (the app/update profile) so plugin installs
/// can use a wider window. `NIYIEN_PLUGIN_DOWNLOAD_RETRIES=0` => single-shot,
/// identical to the pre-change behavior. Logged once on first use.
fn plugin_retry_config() -> (u32, Duration) {
    static CONFIG: OnceLock<(u32, Duration)> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        let retries_env = std::env::var("NIYIEN_PLUGIN_DOWNLOAD_RETRIES").ok();
        let base_env = std::env::var("NIYIEN_PLUGIN_DOWNLOAD_RETRY_BASE_MS").ok();
        let (retries, base) =
            parse_plugin_retry_config(retries_env.as_deref(), base_env.as_deref());
        log::info!(
            target: "update",
            "plugin download retry resolved: retries={retries} base_delay_ms={}",
            base.as_millis()
        );
        (retries, base)
    })
}

/// Resolve whether cold-edge prewarm is enabled once. Shared by all 123-CDN
/// download paths (plugin / app update / lens·sdk).
/// `NIYIEN_DOWNLOAD_PREWARM=0|off|false|no` disables it; default on.
fn download_prewarm_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let enabled = match std::env::var("NIYIEN_DOWNLOAD_PREWARM") {
            Ok(v) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            ),
            Err(_) => true,
        };
        log::info!(
            target: "update",
            "download prewarm resolved: enabled={enabled}"
        );
        enabled
    })
}

/// Plugin-download variant of `call_with_retry` using the dedicated, more
/// aggressive plugin retry profile (`plugin_retry_config`).
pub fn call_with_plugin_retry<T, F>(label: &str, attempt: F) -> Result<T, ureq::Error>
where
    F: FnMut() -> Result<T, ureq::Error>,
{
    let (retries, base) = plugin_retry_config();
    call_with_retry_config(label, retries, base, attempt)
}

/// Best-effort cold-edge prewarm for a download URL. Sends a tiny ranged
/// `bytes=0-0` GET to trigger the 123 direct-link CDN's cold origin fetch so a
/// subsequent full download is more likely to hit a warm edge. Never affects
/// the caller: all failures are swallowed (logged at debug/warn). A short
/// receive timeout keeps it quick, and only a few bytes are read even if the
/// server ignores the Range header and streams the whole object. No-op when
/// disabled via `NIYIEN_DOWNLOAD_PREWARM`. Shared by all 123-CDN download paths.
pub fn prewarm_url(url: &str) {
    if !download_prewarm_enabled() {
        return;
    }
    let started = Instant::now();
    // `get` already applies proxy(None) + connect timeout; re-config only the
    // receive timeout to keep the prewarm short.
    let request = get(url)
        .header("Range", "bytes=0-0")
        .config()
        .timeout_recv_response(Some(Duration::from_secs(10)))
        .build();
    match request.call() {
        Ok(resp) => {
            let status = resp.status();
            let cache = resp
                .headers()
                .get("x-mf-cdn-cache-status")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_owned())
                .unwrap_or_default();
            // Read only a few bytes then drop, in case the server ignored Range
            // and started streaming the whole file.
            use std::io::Read;
            let mut reader = resp.into_body().into_reader();
            let mut buf = [0u8; 64];
            let _ = reader.read(&mut buf);
            log::debug!(
                target: "update",
                "plugin prewarm url={url} http={status} cache={} elapsed_ms={}",
                if cache.is_empty() { "n/a" } else { cache.as_str() },
                started.elapsed().as_millis()
            );
        }
        Err(err) => {
            log::warn!(
                target: "update",
                "plugin prewarm failed (ignored) url={url}: {err} elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn classifies_transient_errors() {
        use super::is_transient_error;
        // Gateway 5xx are transient and worth retrying.
        assert!(is_transient_error(&ureq::Error::StatusCode(500)));
        assert!(is_transient_error(&ureq::Error::StatusCode(502)));
        assert!(is_transient_error(&ureq::Error::StatusCode(503)));
        assert!(is_transient_error(&ureq::Error::StatusCode(504)));
        // Connection / DNS / TLS hiccups are transient.
        assert!(is_transient_error(&ureq::Error::HostNotFound));
        assert!(is_transient_error(&ureq::Error::ConnectionFailed));
        assert!(is_transient_error(&ureq::Error::Tls("handshake")));
        // 4xx and non-gateway statuses are definitive: not retried.
        assert!(!is_transient_error(&ureq::Error::StatusCode(400)));
        assert!(!is_transient_error(&ureq::Error::StatusCode(403)));
        assert!(!is_transient_error(&ureq::Error::StatusCode(404)));
        assert!(!is_transient_error(&ureq::Error::StatusCode(501)));
    }

    #[test]
    fn retry_config_recovers_after_transient() {
        use std::time::Duration;
        // 504 (transient) on the first two attempts, success on the third.
        let mut attempts = 0u32;
        let result = super::call_with_retry_config::<u32, _>(
            "test",
            3,
            Duration::from_millis(1),
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(ureq::Error::StatusCode(504))
                } else {
                    Ok(attempts)
                }
            },
        );
        assert_eq!(result.unwrap(), 3);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn retry_config_does_not_retry_definitive() {
        use std::time::Duration;
        // 404 is definitive: must fail on the first attempt, no retries.
        let mut attempts = 0u32;
        let result = super::call_with_retry_config::<(), _>(
            "test",
            5,
            Duration::from_millis(1),
            || {
                attempts += 1;
                Err(ureq::Error::StatusCode(404))
            },
        );
        assert!(matches!(result, Err(ureq::Error::StatusCode(404))));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn retry_config_zero_is_single_shot() {
        use std::time::Duration;
        // retries=0 => single attempt even for a transient 504 (pre-change parity).
        let mut attempts = 0u32;
        let result = super::call_with_retry_config::<(), _>(
            "test",
            0,
            Duration::from_millis(1),
            || {
                attempts += 1;
                Err(ureq::Error::StatusCode(504))
            },
        );
        assert!(matches!(result, Err(ureq::Error::StatusCode(504))));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn plugin_retry_config_defaults_and_overrides() {
        use super::parse_plugin_retry_config;
        use std::time::Duration;
        // Defaults: retries=3, base=1000ms.
        assert_eq!(
            parse_plugin_retry_config(None, None),
            (3, Duration::from_millis(1000))
        );
        // Explicit overrides.
        assert_eq!(
            parse_plugin_retry_config(Some("5"), Some("250")),
            (5, Duration::from_millis(250))
        );
        // retries clamped to 10.
        assert_eq!(parse_plugin_retry_config(Some("99"), None).0, 10);
        // retries=0 is allowed (single-shot).
        assert_eq!(parse_plugin_retry_config(Some("0"), None).0, 0);
        // base 0 / invalid falls back to default 1000ms.
        assert_eq!(
            parse_plugin_retry_config(None, Some("0")).1,
            Duration::from_millis(1000)
        );
        assert_eq!(
            parse_plugin_retry_config(Some("x"), Some("y")),
            (3, Duration::from_millis(1000))
        );
    }

    #[test]
    fn body_timeout_defaults_disables_and_clamps() {
        use super::{parse_body_timeout, BODY_TIMEOUT_DEFAULT_SECS, BODY_TIMEOUT_MAX_SECS};
        // Unset / empty => default.
        assert_eq!(parse_body_timeout(None), (BODY_TIMEOUT_DEFAULT_SECS, "default"));
        assert_eq!(
            parse_body_timeout(Some("   ")),
            (BODY_TIMEOUT_DEFAULT_SECS, "default")
        );
        // 0 => disabled (the emergency rollback path); it is NOT an instant timeout.
        assert_eq!(parse_body_timeout(Some("0")), (0, "env"));
        // Explicit in-range override, whitespace tolerated.
        assert_eq!(parse_body_timeout(Some("120")), (120, "env"));
        assert_eq!(parse_body_timeout(Some(" 120 ")), (120, "env"));
        // Boundary: exactly the cap is accepted as-is, not clamped.
        assert_eq!(
            parse_body_timeout(Some("3600")),
            (BODY_TIMEOUT_MAX_SECS, "env")
        );
        // Above the cap => clamped.
        assert_eq!(
            parse_body_timeout(Some("99999")),
            (BODY_TIMEOUT_MAX_SECS, "env_clamped")
        );
        // Invalid => default (never disabled, never zero).
        assert_eq!(
            parse_body_timeout(Some("abc")),
            (BODY_TIMEOUT_DEFAULT_SECS, "default_invalid")
        );
        assert_eq!(
            parse_body_timeout(Some("-5")),
            (BODY_TIMEOUT_DEFAULT_SECS, "default_invalid")
        );
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        use super::backoff_delay;
        use std::time::Duration;
        let base = Duration::from_millis(500);
        assert_eq!(backoff_delay(base, 0), Duration::from_millis(500));
        assert_eq!(backoff_delay(base, 1), Duration::from_millis(2000));
        // 500 * 4^2 = 8000 -> capped at 5000.
        assert_eq!(backoff_delay(base, 2), Duration::from_millis(5000));
        assert_eq!(backoff_delay(base, 8), Duration::from_millis(5000));
    }

    #[test]
    fn get_ignores_proxy_environment_variables() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _proxy_env = ProxyEnvGuard::set("http://127.0.0.1:9");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 512];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
        });

        let body = super::get(&url)
            .call()
            .unwrap()
            .into_body()
            .read_to_string()
            .unwrap();
        server.join().unwrap();

        assert_eq!(body, "ok");
    }

    struct ProxyEnvGuard(Vec<(&'static str, Option<String>)>);

    impl ProxyEnvGuard {
        fn set(value: &str) -> Self {
            let saved = proxy_var_names()
                .into_iter()
                .map(|name| (name, std::env::var(name).ok()))
                .collect();
            set_proxy_vars(value);
            Self(saved)
        }
    }

    impl Drop for ProxyEnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn set_proxy_vars(value: &str) {
        for name in proxy_var_names() {
            unsafe {
                std::env::set_var(name, value);
            }
        }
    }

    fn proxy_var_names() -> [&'static str; 6] {
        [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ]
    }
}
