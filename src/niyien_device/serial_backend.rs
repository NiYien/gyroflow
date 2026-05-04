// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use super::transport::{
    DeviceConnectionConfig as SerialConnectionConfig, DevicePortCandidate as SerialPortCandidate,
    DeviceTransportBackend as SerialBackend, DeviceTransportError as SerialBackendError,
    DeviceTransportStream as SerialStream,
};

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
use serialport::{DataBits, FlowControl, Parity, SerialPortType, StopBits};

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
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(&mut self.0, buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.0)
    }
}

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "niyien-serialport"
))]
impl SerialBackend for RealSerialBackend {
    type Stream = RealSerialPort;

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
    ) -> Result<Self::Stream, SerialBackendError> {
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
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }

    fn write_all(&mut self, _buf: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl SerialBackend for StubSerialBackend {
    type Stream = StubSerialPort;

    fn list_ports(&mut self) -> Result<Vec<SerialPortCandidate>, SerialBackendError> {
        Ok(Vec::new())
    }

    fn open(
        &mut self,
        _port_name: &str,
        _config: &SerialConnectionConfig,
    ) -> Result<Self::Stream, SerialBackendError> {
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
