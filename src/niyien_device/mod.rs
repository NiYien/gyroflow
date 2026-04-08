// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::SeqCst},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use chrono::{Datelike, Timelike, Utc};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use parking_lot::Mutex;

pub mod commands;
pub mod ota;
pub mod protocol;
pub mod serial_backend;
pub mod update_checker;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use crate::niyien_device::protocol::FrameParser;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use commands::{DeviceTime, VersionInfo};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use ota::{FirmwarePackage, OtaAction, OtaManager, OtaState};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use serial_backend::{
    DefaultSerialBackend, RetryBackoff, ScanTracker, SerialBackend, SerialConnectionConfig,
    SerialPortCandidate, SerialStream, filter_matching_ports,
};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use update_checker::FirmwareUpdateInfo;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
const SERIAL_LOOP_TICK: Duration = Duration::from_millis(50);
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const NETWORK_LOOP_TICK: Duration = Duration::from_millis(100);
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const A1_DEVICE_PRODUCT_ID: u8 = 0xA1;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug)]
pub enum DeviceCommand {
    SyncTime(i16),
    CheckUpdate(String),
    StartOta,
    Stop,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Clone, Debug)]
pub enum DeviceEvent {
    Connected(VersionInfo),
    Disconnected,
    TimeReceived(DeviceTime),
    TimeSyncResult(bool),
    UpdateAvailable(Option<FirmwareUpdateInfo>),
    UpdateCheckFailed(String),
    OtaProgress(f64),
    OtaComplete,
    OtaFailed(String),
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
enum SerialCommand {
    SyncTime(i16),
    Stop,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
enum NetworkCommand {
    CheckUpdate(String),
    StartOta,
    Stop,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
struct DeviceSharedState {
    latest_update: Option<FirmwareUpdateInfo>,
    prepared_firmware: Option<FirmwarePackage>,
    ota_manager: Option<OtaManager>,
    ota_start_pending: bool,
    ota_last_progress_percent: i32,
    ota_last_progress_at: Option<Instant>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct SerialSession<P: SerialStream> {
    port_name: String,
    stream: P,
    parser: FrameParser,
    version_info: Option<VersionInfo>,
    connected_emitted: bool,
    last_time_poll: Instant,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub struct DeviceManager {
    command_tx: Sender<DeviceCommand>,
    event_rx: Arc<Mutex<Receiver<DeviceEvent>>>,
    running: Arc<AtomicBool>,
    dispatcher_thread: Option<JoinHandle<()>>,
    serial_thread: Option<JoinHandle<()>>,
    network_thread: Option<JoinHandle<()>>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl DeviceManager {
    pub fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (serial_tx, serial_rx) = mpsc::channel();
        let (network_tx, network_rx) = mpsc::channel();

        let running = Arc::new(AtomicBool::new(true));
        let event_rx = Arc::new(Mutex::new(event_rx));
        let shared_state = Arc::new(Mutex::new(DeviceSharedState::default()));

        let serial_thread = {
            let running = Arc::clone(&running);
            let event_tx = event_tx.clone();
            let shared_state = Arc::clone(&shared_state);
            thread::spawn(move || serial_thread_loop(running, serial_rx, event_tx, shared_state))
        };

        let network_thread = {
            let running = Arc::clone(&running);
            let event_tx = event_tx.clone();
            let shared_state = Arc::clone(&shared_state);
            thread::spawn(move || network_thread_loop(running, network_rx, event_tx, shared_state))
        };

        let dispatcher_thread = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                dispatcher_loop(command_rx, serial_tx, network_tx, running);
            })
        };

        Self {
            command_tx,
            event_rx,
            running,
            dispatcher_thread: Some(dispatcher_thread),
            serial_thread: Some(serial_thread),
            network_thread: Some(network_thread),
        }
    }

    pub fn command_sender(&self) -> Sender<DeviceCommand> {
        self.command_tx.clone()
    }

    pub fn event_receiver(&self) -> Arc<Mutex<Receiver<DeviceEvent>>> {
        Arc::clone(&self.event_rx)
    }

    pub fn stop(&mut self) {
        if !self.running.swap(false, SeqCst) {
            return;
        }

        let _ = self.command_tx.send(DeviceCommand::Stop);

        if let Some(thread) = self.dispatcher_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.serial_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.network_thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Drop for DeviceManager {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn dispatcher_loop(
    command_rx: Receiver<DeviceCommand>,
    serial_tx: Sender<SerialCommand>,
    network_tx: Sender<NetworkCommand>,
    running: Arc<AtomicBool>,
) {
    while running.load(SeqCst) {
        match command_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(DeviceCommand::SyncTime(tz_offset_minutes)) => {
                let _ = serial_tx.send(SerialCommand::SyncTime(tz_offset_minutes));
            }
            Ok(DeviceCommand::CheckUpdate(current_version)) => {
                let _ = network_tx.send(NetworkCommand::CheckUpdate(current_version));
            }
            Ok(DeviceCommand::StartOta) => {
                let _ = network_tx.send(NetworkCommand::StartOta);
            }
            Ok(DeviceCommand::Stop) => {
                running.store(false, SeqCst);
                let _ = serial_tx.send(SerialCommand::Stop);
                let _ = network_tx.send(NetworkCommand::Stop);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                running.store(false, SeqCst);
                let _ = serial_tx.send(SerialCommand::Stop);
                let _ = network_tx.send(NetworkCommand::Stop);
                break;
            }
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn serial_thread_loop(
    running: Arc<AtomicBool>,
    serial_rx: Receiver<SerialCommand>,
    event_tx: Sender<DeviceEvent>,
    shared_state: Arc<Mutex<DeviceSharedState>>,
) {
    run_serial_thread(
        DefaultSerialBackend::default(),
        running,
        serial_rx,
        event_tx,
        shared_state,
    );
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn run_serial_thread<B: SerialBackend>(
    mut backend: B,
    running: Arc<AtomicBool>,
    serial_rx: Receiver<SerialCommand>,
    event_tx: Sender<DeviceEvent>,
    shared_state: Arc<Mutex<DeviceSharedState>>,
) {
    let config = SerialConnectionConfig::default();
    let mut scan_tracker = ScanTracker::default();
    let mut backoff = RetryBackoff::new(config.initial_retry_delay, config.max_retry_delay);
    let mut session: Option<SerialSession<B::Port>> = None;

    while running.load(SeqCst) {
        match serial_rx.recv_timeout(SERIAL_LOOP_TICK) {
            Ok(SerialCommand::SyncTime(tz_offset_minutes)) => {
                if let Some(active) = session.as_mut() {
                    if let Err(err) = send_current_time(active, tz_offset_minutes) {
                        log::warn!("Failed to send SyncTime to {}: {}", active.port_name, err);
                        disconnect_session(&mut session, &event_tx, &shared_state);
                        let _ = event_tx.send(DeviceEvent::TimeSyncResult(false));
                    }
                } else {
                    let _ = event_tx.send(DeviceEvent::TimeSyncResult(false));
                }
            }
            Ok(SerialCommand::Stop) => break,
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();

                if !drive_ota_timeout(&mut session, &event_tx, &shared_state, now) {
                    disconnect_session(&mut session, &event_tx, &shared_state);
                    continue;
                }

                if let Some(active) = session.as_mut() {
                    let mut should_disconnect = false;
                    if !poll_serial_session(active, &event_tx, &shared_state, now) {
                        should_disconnect = true;
                    } else if active.version_info.is_some()
                        && !ota_active(&shared_state)
                        && now.saturating_duration_since(active.last_time_poll)
                            >= Duration::from_secs(1)
                    {
                        if let Err(err) = write_packet(&mut active.stream, &commands::ask_time()) {
                            log::warn!("Failed to send ask_time to {}: {}", active.port_name, err);
                            should_disconnect = true;
                        } else {
                            active.last_time_poll = now;
                        }
                    }
                    if !should_disconnect
                        && !start_pending_ota(active, &event_tx, &shared_state, now)
                    {
                        should_disconnect = true;
                    }
                    if should_disconnect {
                        disconnect_session(&mut session, &event_tx, &shared_state);
                    }
                    continue;
                }

                if !backoff.can_attempt(now) {
                    continue;
                }

                let ports = match backend.list_ports() {
                    Ok(ports) => ports,
                    Err(err) => {
                        log::warn!("Serial port scan failed: {}", err);
                        backoff.record_failure(now);
                        continue;
                    }
                };
                let ports = filter_matching_ports(&ports, config.vendor_id, config.product_id);
                let stable_ports = scan_tracker.stable_ports(now, &ports, config.debounce_interval);
                if let Some(candidate) = stable_ports.first() {
                    log::info!(
                        "NiYien candidate serial port detected: {}",
                        candidate.port_name
                    );
                    match try_open_candidate(&mut backend, candidate, &config, now, &event_tx) {
                        Ok(opened) => {
                            session = Some(opened);
                            backoff.reset();
                        }
                        Err(err) => {
                            log::warn!(
                                "Failed to open serial port {}: {}",
                                candidate.port_name,
                                err
                            );
                            backoff.record_failure(now);
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    disconnect_session(&mut session, &event_tx, &shared_state);
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn network_thread_loop(
    running: Arc<AtomicBool>,
    network_rx: Receiver<NetworkCommand>,
    event_tx: Sender<DeviceEvent>,
    shared_state: Arc<Mutex<DeviceSharedState>>,
) {
    while running.load(SeqCst) {
        match network_rx.recv_timeout(NETWORK_LOOP_TICK) {
            Ok(NetworkCommand::CheckUpdate(current_version)) => {
                match update_checker::check_update(&current_version) {
                    Ok(info) => {
                        let mut shared = shared_state.lock();
                        shared.prepared_firmware = None;
                        shared.ota_manager = None;
                        shared.latest_update = info.clone();
                        drop(shared);
                        let _ = event_tx.send(DeviceEvent::UpdateAvailable(info));
                    }
                    Err(err) => {
                        let mut shared = shared_state.lock();
                        shared.latest_update = None;
                        shared.prepared_firmware = None;
                        shared.ota_manager = None;
                        drop(shared);
                        let _ = event_tx.send(DeviceEvent::UpdateCheckFailed(err.to_string()));
                    }
                }
            }
            Ok(NetworkCommand::StartOta) => {
                prepare_ota(&event_tx, &shared_state);
            }
            Ok(NetworkCommand::Stop) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn prepare_ota(event_tx: &Sender<DeviceEvent>, shared_state: &Arc<Mutex<DeviceSharedState>>) {
    let update_info = {
        let shared = shared_state.lock();
        shared.latest_update.clone()
    };

    let Some(update_info) = update_info else {
        let _ = event_tx.send(DeviceEvent::OtaFailed(
            "Check for firmware updates first".to_owned(),
        ));
        return;
    };

    let _ = event_tx.send(DeviceEvent::OtaProgress(0.05));

    let bytes = match update_checker::download_firmware(&update_info) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = event_tx.send(DeviceEvent::OtaFailed(err.to_string()));
            return;
        }
    };

    let _ = event_tx.send(DeviceEvent::OtaProgress(0.35));

    let firmware = match ota::load_firmware(&bytes) {
        Ok(firmware) => firmware,
        Err(err) => {
            let _ = event_tx.send(DeviceEvent::OtaFailed(err.to_string()));
            return;
        }
    };

    let _ = event_tx.send(DeviceEvent::OtaProgress(0.55));

    let ota_manager = OtaManager::new(firmware.clone());
    if let Err(err) = ota_manager.validate_firmware(A1_DEVICE_PRODUCT_ID) {
        let _ = event_tx.send(DeviceEvent::OtaFailed(err.to_string()));
        return;
    }

    {
        let mut shared = shared_state.lock();
        shared.prepared_firmware = Some(firmware);
        shared.ota_manager = Some(ota_manager);
        shared.ota_start_pending = true;
        shared.ota_last_progress_percent = -1;
        shared.ota_last_progress_at = None;
    }

    let _ = event_tx.send(DeviceEvent::OtaProgress(0.7));
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn try_open_candidate<B: SerialBackend>(
    backend: &mut B,
    candidate: &SerialPortCandidate,
    config: &SerialConnectionConfig,
    now: Instant,
    _event_tx: &Sender<DeviceEvent>,
) -> Result<SerialSession<B::Port>, serial_backend::SerialBackendError> {
    let mut stream = backend.open(&candidate.port_name, config)?;
    write_packet(&mut stream, &commands::ask_version())
        .map_err(serial_backend::SerialBackendError::Io)?;

    Ok(SerialSession {
        port_name: candidate.port_name.clone(),
        stream,
        parser: FrameParser::new(),
        version_info: None,
        connected_emitted: false,
        last_time_poll: now,
    })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn poll_serial_session<P: SerialStream>(
    session: &mut SerialSession<P>,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) -> bool {
    let mut buf = [0u8; 512];
    match session.stream.read(&mut buf) {
        Ok(read) => {
            if read == 0 {
                session.parser.clear_if_timed_out_at(now);
                return true;
            }

            for frame in session.parser.feed_at(&buf[..read], now) {
                if !handle_serial_frame(session, frame, event_tx, shared_state, now) {
                    return false;
                }
            }
            true
        }
        Err(err) if is_timeout_error(&err) => {
            session.parser.clear_if_timed_out_at(now);
            true
        }
        Err(err) => {
            log::warn!("Serial read failed on {}: {}", session.port_name, err);
            false
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn handle_serial_frame<P: SerialStream>(
    session: &mut SerialSession<P>,
    frame: protocol::Frame,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) -> bool {
    match commands::parse_response(&frame) {
        Some(commands::Response::Version(info)) => {
            session.version_info = Some(info.clone());
            if !session.connected_emitted {
                session.connected_emitted = true;
                log::info!(
                    "NiYien connected on {}: soft={}, hard={}",
                    session.port_name,
                    info.soft_version,
                    info.hard_version
                );
                let _ = event_tx.send(DeviceEvent::Connected(info));
            }
            handle_ota_frame_action(session, &frame, event_tx, shared_state, now)
        }
        Some(commands::Response::TimeGet(time)) => {
            let _ = event_tx.send(DeviceEvent::TimeReceived(time));
            handle_ota_frame_action(session, &frame, event_tx, shared_state, now)
        }
        Some(commands::Response::TimeSetResult(result)) => {
            let _ = event_tx.send(DeviceEvent::TimeSyncResult(result.success));
            handle_ota_frame_action(session, &frame, event_tx, shared_state, now)
        }
        Some(commands::Response::OtaAck(_)) => {
            handle_ota_frame_action(session, &frame, event_tx, shared_state, now)
        }
        None => true,
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn disconnect_session<P: SerialStream>(
    session: &mut Option<SerialSession<P>>,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
) {
    let waiting_reconnect = {
        let shared = shared_state.lock();
        shared
            .ota_manager
            .as_ref()
            .is_some_and(|manager| manager.state() == OtaState::WaitingReconnect)
    };

    if session
        .take()
        .is_some_and(|active| active.connected_emitted)
        && !waiting_reconnect
    {
        let _ = event_tx.send(DeviceEvent::Disconnected);
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn write_packet<P: SerialStream>(stream: &mut P, packet: &[u8]) -> io::Result<()> {
    stream.write_all(packet)?;
    stream.flush()
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn send_current_time<P: SerialStream>(
    session: &mut SerialSession<P>,
    tz_offset_minutes: i16,
) -> io::Result<()> {
    let now = Utc::now() + chrono::Duration::minutes(tz_offset_minutes as i64);
    let packet = commands::set_time(
        now.year() as u16,
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
        tz_offset_minutes,
    );
    write_packet(&mut session.stream, &packet)
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn is_timeout_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn start_pending_ota<P: SerialStream>(
    session: &mut SerialSession<P>,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) -> bool {
    if session.version_info.is_none() {
        return true;
    }

    let packet = {
        let mut shared = shared_state.lock();
        if !shared.ota_start_pending {
            return true;
        }
        shared.ota_start_pending = false;
        let Some(manager) = shared.ota_manager.as_mut() else {
            return true;
        };
        if manager.state() != OtaState::Idle {
            return true;
        }
        Some(manager.start_at(now))
    };

    if let Some(packet) = packet {
        if let Err(err) = write_packet(&mut session.stream, &packet) {
            let _ = event_tx.send(DeviceEvent::OtaFailed(format!(
                "Failed to start OTA: {err}"
            )));
            clear_ota_state(shared_state);
            return false;
        }
        maybe_emit_ota_progress(event_tx, shared_state, now);
    }
    true
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn drive_ota_timeout<P: SerialStream>(
    session: &mut Option<SerialSession<P>>,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) -> bool {
    let action = {
        let mut shared = shared_state.lock();
        let Some(manager) = shared.ota_manager.as_mut() else {
            return true;
        };
        manager.on_timeout_at(now)
    };
    handle_ota_action(session, action, event_tx, shared_state, now)
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn handle_ota_frame_action<P: SerialStream>(
    session: &mut SerialSession<P>,
    frame: &protocol::Frame,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) -> bool {
    let action = {
        let mut shared = shared_state.lock();
        let Some(manager) = shared.ota_manager.as_mut() else {
            return true;
        };

        if manager.state() == OtaState::WaitingReconnect {
            if let Some(version) = session.version_info.as_ref() {
                manager.on_device_reconnected_at(version, now)
            } else {
                OtaAction::Noop
            }
        } else {
            manager.on_frame_at(frame, now)
        }
    };

    handle_ota_action(
        &mut SomeSession(session),
        action,
        event_tx,
        shared_state,
        now,
    )
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct SomeSession<'a, P: SerialStream>(&'a mut SerialSession<P>);

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn handle_ota_action<P: SerialStream>(
    session: &mut impl OtaSessionAccess<P>,
    action: OtaAction,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) -> bool {
    match action {
        OtaAction::Send(packet) => {
            let Some(active) = session.session_mut() else {
                let _ = event_tx.send(DeviceEvent::OtaFailed(
                    "The device was disconnected during OTA transfer".to_owned(),
                ));
                clear_ota_state(shared_state);
                return false;
            };
            if let Err(err) = write_packet(&mut active.stream, &packet) {
                let _ = event_tx.send(DeviceEvent::OtaFailed(format!(
                    "Failed to send OTA packet: {err}"
                )));
                clear_ota_state(shared_state);
                return false;
            }
            maybe_emit_ota_progress(event_tx, shared_state, now);
            true
        }
        OtaAction::WaitingReconnect => {
            maybe_emit_ota_progress(event_tx, shared_state, now);
            true
        }
        OtaAction::Complete(version) => {
            if let Some(active) = session.session_mut() {
                active.version_info = Some(version);
            }
            maybe_emit_ota_progress_force(1.0, event_tx, shared_state, now);
            clear_ota_state(shared_state);
            let _ = event_tx.send(DeviceEvent::OtaComplete);
            true
        }
        OtaAction::Failed(message) => {
            clear_ota_state(shared_state);
            let _ = event_tx.send(DeviceEvent::OtaFailed(message));
            true
        }
        OtaAction::Noop => {
            maybe_emit_ota_progress(event_tx, shared_state, now);
            true
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
trait OtaSessionAccess<P: SerialStream> {
    fn session_mut(&mut self) -> Option<&mut SerialSession<P>>;
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl<P: SerialStream> OtaSessionAccess<P> for Option<SerialSession<P>> {
    fn session_mut(&mut self) -> Option<&mut SerialSession<P>> {
        self.as_mut()
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl<'a, P: SerialStream> OtaSessionAccess<P> for SomeSession<'a, P> {
    fn session_mut(&mut self) -> Option<&mut SerialSession<P>> {
        Some(self.0)
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn maybe_emit_ota_progress(
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) {
    let progress = {
        let shared = shared_state.lock();
        shared.ota_manager.as_ref().map(OtaManager::progress)
    };
    if let Some(progress) = progress {
        maybe_emit_ota_progress_force(progress, event_tx, shared_state, now);
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn maybe_emit_ota_progress_force(
    progress: f64,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) {
    let percent = (progress * 100.0).round() as i32;
    let should_emit = {
        let mut shared = shared_state.lock();
        let last_percent = shared.ota_last_progress_percent;
        let last_at = shared.ota_last_progress_at;
        let changed_enough = last_percent < 0 || (percent - last_percent).abs() >= 1;
        let elapsed_enough = last_at
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(100));
        if changed_enough || elapsed_enough || percent == 100 {
            shared.ota_last_progress_percent = percent;
            shared.ota_last_progress_at = Some(now);
            true
        } else {
            false
        }
    };

    if should_emit {
        let _ = event_tx.send(DeviceEvent::OtaProgress(progress));
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn clear_ota_state(shared_state: &Arc<Mutex<DeviceSharedState>>) {
    let mut shared = shared_state.lock();
    shared.prepared_firmware = None;
    shared.ota_manager = None;
    shared.ota_start_pending = false;
    shared.ota_last_progress_percent = -1;
    shared.ota_last_progress_at = None;
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn ota_active(shared_state: &Arc<Mutex<DeviceSharedState>>) -> bool {
    let shared = shared_state.lock();
    shared.ota_manager.is_some()
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use chrono::{Datelike, TimeZone, Timelike, Utc};

    fn utc_components_with_offset(
        now_utc: chrono::DateTime<Utc>,
        tz_offset_minutes: i16,
    ) -> (i32, u32, u32, u32, u32, u32) {
        let shifted = now_utc + chrono::Duration::minutes(tz_offset_minutes as i64);
        (
            shifted.year(),
            shifted.month(),
            shifted.day(),
            shifted.hour(),
            shifted.minute(),
            shifted.second(),
        )
    }

    #[test]
    fn applies_positive_timezone_offset_across_day_boundary() {
        let now_utc = Utc.with_ymd_and_hms(2026, 4, 8, 20, 30, 15).unwrap();
        assert_eq!(
            utc_components_with_offset(now_utc, 480),
            (2026, 4, 9, 4, 30, 15)
        );
    }

    #[test]
    fn applies_negative_timezone_offset_across_day_boundary() {
        let now_utc = Utc.with_ymd_and_hms(2026, 4, 8, 3, 5, 9).unwrap();
        assert_eq!(
            utc_components_with_offset(now_utc, -420),
            (2026, 4, 7, 20, 5, 9)
        );
    }
}
