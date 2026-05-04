// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::{
    collections::{HashMap, HashSet},
    fmt, io,
    time::{Duration, Instant},
};

use super::DeviceConnectionStatus;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConnectionConfig {
    pub vendor_id: u16,
    pub product_id: u16,
    pub baud_rate: u32,
    pub read_timeout: Duration,
    pub scan_interval: Duration,
    pub debounce_interval: Duration,
    pub initial_retry_delay: Duration,
    pub max_retry_delay: Duration,
    pub dtr_on_open: bool,
}

impl Default for DeviceConnectionConfig {
    fn default() -> Self {
        Self {
            vendor_id: 0xFFFF,
            product_id: 0xFFFF,
            baud_rate: 2_000_000,
            read_timeout: Duration::from_millis(50),
            scan_interval: Duration::from_millis(200),
            debounce_interval: Duration::from_millis(300),
            initial_retry_delay: Duration::from_millis(200),
            max_retry_delay: Duration::from_secs(5),
            dtr_on_open: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevicePortCandidate {
    pub port_name: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
}

#[derive(Debug)]
pub enum DeviceTransportError {
    Unsupported(&'static str),
    Io(io::Error),
    Open(String),
}

impl fmt::Display for DeviceTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) => f.write_str(message),
            Self::Io(err) => write!(f, "device transport I/O error: {err}"),
            Self::Open(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DeviceTransportError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceTransportEvent {
    ConnectionStatus(DeviceConnectionStatus, String),
    Detached,
}

pub trait DeviceTransportStream: Send {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

pub trait DeviceTransportBackend: Send + 'static {
    type Stream: DeviceTransportStream;

    fn list_ports(&mut self) -> Result<Vec<DevicePortCandidate>, DeviceTransportError>;
    fn open(
        &mut self,
        port_name: &str,
        config: &DeviceConnectionConfig,
    ) -> Result<Self::Stream, DeviceTransportError>;

    fn poll_event(&mut self) -> Option<DeviceTransportEvent> {
        None
    }
}

#[derive(Default)]
pub struct ScanTracker {
    seen_at: HashMap<String, Instant>,
}

impl ScanTracker {
    pub fn stable_ports(
        &mut self,
        now: Instant,
        ports: &[DevicePortCandidate],
        debounce_interval: Duration,
    ) -> Vec<DevicePortCandidate> {
        let current_names: HashSet<_> = ports.iter().map(|port| port.port_name.clone()).collect();
        self.seen_at.retain(|name, _| current_names.contains(name));

        let mut stable = Vec::new();
        for port in ports {
            let first_seen = self.seen_at.entry(port.port_name.clone()).or_insert(now);
            if now.saturating_duration_since(*first_seen) >= debounce_interval {
                stable.push(port.clone());
            }
        }
        stable
    }
}

#[derive(Clone, Debug)]
pub struct RetryBackoff {
    current_delay: Duration,
    initial_delay: Duration,
    max_delay: Duration,
    next_attempt_at: Option<Instant>,
}

impl RetryBackoff {
    pub fn new(initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            current_delay: initial_delay,
            initial_delay,
            max_delay,
            next_attempt_at: None,
        }
    }

    pub fn can_attempt(&self, now: Instant) -> bool {
        self.next_attempt_at.is_none_or(|deadline| now >= deadline)
    }

    pub fn record_failure(&mut self, now: Instant) {
        self.next_attempt_at = Some(now + self.current_delay);
        let doubled = self.current_delay.saturating_mul(2);
        self.current_delay = doubled.min(self.max_delay);
    }

    pub fn reset(&mut self) {
        self.current_delay = self.initial_delay;
        self.next_attempt_at = None;
    }
}

pub fn filter_matching_ports(
    ports: &[DevicePortCandidate],
    vendor_id: u16,
    product_id: u16,
) -> Vec<DevicePortCandidate> {
    ports
        .iter()
        .filter(|port| port.vendor_id == Some(vendor_id) && port.product_id == Some(product_id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_ports_by_vid_pid() {
        let ports = vec![
            DevicePortCandidate {
                port_name: "COM3".into(),
                vendor_id: Some(0xFFFF),
                product_id: Some(0xFFFF),
                serial_number: None,
            },
            DevicePortCandidate {
                port_name: "COM4".into(),
                vendor_id: Some(0x1234),
                product_id: Some(0xFFFF),
                serial_number: None,
            },
        ];

        assert_eq!(
            filter_matching_ports(&ports, 0xFFFF, 0xFFFF),
            vec![ports[0].clone()]
        );
    }

    #[test]
    fn waits_for_debounce_before_exposing_port() {
        let mut tracker = ScanTracker::default();
        let port = DevicePortCandidate {
            port_name: "COM3".into(),
            vendor_id: Some(0xFFFF),
            product_id: Some(0xFFFF),
            serial_number: None,
        };
        let start = Instant::now();
        let debounce = Duration::from_millis(300);

        assert!(
            tracker
                .stable_ports(start, std::slice::from_ref(&port), debounce)
                .is_empty()
        );
        assert!(
            tracker
                .stable_ports(
                    start + Duration::from_millis(250),
                    std::slice::from_ref(&port),
                    debounce,
                )
                .is_empty()
        );
        assert_eq!(
            tracker.stable_ports(
                start + Duration::from_millis(301),
                std::slice::from_ref(&port),
                debounce,
            ),
            vec![port]
        );
    }

    #[test]
    fn backoff_grows_until_maximum_and_resets() {
        let mut backoff = RetryBackoff::new(Duration::from_millis(200), Duration::from_secs(5));
        let start = Instant::now();

        assert!(backoff.can_attempt(start));
        backoff.record_failure(start);
        assert!(!backoff.can_attempt(start + Duration::from_millis(199)));
        assert!(backoff.can_attempt(start + Duration::from_millis(200)));

        backoff.record_failure(start + Duration::from_millis(200));
        backoff.record_failure(start + Duration::from_millis(600));
        backoff.record_failure(start + Duration::from_millis(1_400));
        backoff.record_failure(start + Duration::from_millis(3_000));
        backoff.record_failure(start + Duration::from_millis(6_200));

        assert!(!backoff.can_attempt(start + Duration::from_millis(11_199)));
        assert!(backoff.can_attempt(start + Duration::from_millis(11_200)));

        backoff.reset();
        assert!(backoff.can_attempt(start + Duration::from_millis(11_200)));
    }
}
