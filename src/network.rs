// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use std::sync::OnceLock;
use std::time::Duration;

pub fn configure<T>(request: ureq::RequestBuilder<T>) -> ureq::RequestBuilder<T> {
    request
        .config()
        .proxy(None)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
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
/// only used for logging (target `update`).
pub fn call_with_retry<T, F>(label: &str, mut attempt: F) -> Result<T, ureq::Error>
where
    F: FnMut() -> Result<T, ureq::Error>,
{
    let (retries, base) = retry_config();
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
