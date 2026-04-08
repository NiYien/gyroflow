// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::{
    collections::{HashMap, HashSet},
    fmt, io,
    time::{Duration, Instant},
};

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
use serialport::{DataBits, FlowControl, Parity, SerialPortType, StopBits};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialConnectionConfig {
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Default for SerialConnectionConfig {
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialPortCandidate {
    pub port_name: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug)]
pub enum SerialBackendError {
    Unsupported(&'static str),
    Io(io::Error),
    Open(String),
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl fmt::Display for SerialBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) => f.write_str(message),
            Self::Io(err) => write!(f, "serial I/O error: {err}"),
            Self::Open(message) => f.write_str(message),
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl std::error::Error for SerialBackendError {}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub trait SerialStream: Send {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub trait SerialBackend: Send + 'static {
    type Port: SerialStream;

    fn list_ports(&mut self) -> Result<Vec<SerialPortCandidate>, SerialBackendError>;
    fn open(
        &mut self,
        port_name: &str,
        config: &SerialConnectionConfig,
    ) -> Result<Self::Port, SerialBackendError>;
}

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
#[derive(Default)]
pub struct RealSerialBackend;

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
pub struct RealSerialPort(Box<dyn serialport::SerialPort>);

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
impl SerialStream for RealSerialPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        io::Write::write_all(&mut self.0, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut self.0)
    }
}

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
impl SerialBackend for RealSerialBackend {
    type Port = RealSerialPort;

    fn list_ports(&mut self) -> Result<Vec<SerialPortCandidate>, SerialBackendError> {
        serialport::available_ports()
            .map_err(|err| SerialBackendError::Open(err.to_string()))?
            .into_iter()
            .map(|port| {
                let (vendor_id, product_id, serial_number) = match port.port_type {
                    SerialPortType::UsbPort(info) => (
                        Some(info.vid),
                        Some(info.pid),
                        info.serial_number.map(|value| value.to_string()),
                    ),
                    _ => (None, None, None),
                };
                Ok(SerialPortCandidate {
                    port_name: port.port_name,
                    vendor_id,
                    product_id,
                    serial_number,
                })
            })
            .collect()
    }

    fn open(
        &mut self,
        port_name: &str,
        config: &SerialConnectionConfig,
    ) -> Result<Self::Port, SerialBackendError> {
        let mut builder = serialport::new(port_name, config.baud_rate)
            .timeout(config.read_timeout)
            .data_bits(DataBits::Eight)
            .flow_control(FlowControl::None)
            .parity(Parity::None)
            .stop_bits(StopBits::One);

        builder = builder.dtr_on_open(config.dtr_on_open);
        let port = builder
            .open()
            .map_err(|err| SerialBackendError::Open(err.to_string()))?;
        Ok(RealSerialPort(port))
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
pub struct StubSerialBackend;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub struct StubSerialPort;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl SerialStream for StubSerialPort {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn write_all(&mut self, _buf: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl SerialBackend for StubSerialBackend {
    type Port = StubSerialPort;

    fn list_ports(&mut self) -> Result<Vec<SerialPortCandidate>, SerialBackendError> {
        Ok(Vec::new())
    }

    fn open(
        &mut self,
        _port_name: &str,
        _config: &SerialConnectionConfig,
    ) -> Result<Self::Port, SerialBackendError> {
        Err(SerialBackendError::Unsupported(
            "serialport backend is not linked yet",
        ))
    }
}

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
pub type DefaultSerialBackend = RealSerialBackend;

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    not(feature = "niyien-serialport")
))]
pub type DefaultSerialBackend = StubSerialBackend;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
pub struct ScanTracker {
    seen_at: HashMap<String, Instant>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl ScanTracker {
    pub fn stable_ports(
        &mut self,
        now: Instant,
        ports: &[SerialPortCandidate],
        debounce_interval: Duration,
    ) -> Vec<SerialPortCandidate> {
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Clone, Debug)]
pub struct RetryBackoff {
    current_delay: Duration,
    initial_delay: Duration,
    max_delay: Duration,
    next_attempt_at: Option<Instant>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn filter_matching_ports(
    ports: &[SerialPortCandidate],
    vendor_id: u16,
    product_id: u16,
) -> Vec<SerialPortCandidate> {
    ports
        .iter()
        .filter(|port| port.vendor_id == Some(vendor_id) && port.product_id == Some(product_id))
        .cloned()
        .collect()
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use super::*;

    #[test]
    fn filters_ports_by_vid_pid() {
        let ports = vec![
            SerialPortCandidate {
                port_name: "COM3".into(),
                vendor_id: Some(0xFFFF),
                product_id: Some(0xFFFF),
                serial_number: None,
            },
            SerialPortCandidate {
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
        let port = SerialPortCandidate {
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
