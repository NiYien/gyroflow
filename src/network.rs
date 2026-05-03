// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
