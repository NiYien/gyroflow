// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

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

use chrono::{Datelike, Timelike, Utc};
use parking_lot::Mutex;

pub mod commands;
#[cfg(any(target_os = "android", target_os = "ios"))]
pub mod mobile_backend;
pub mod ota;
pub mod protocol;
pub mod serial_backend;
pub mod transport;
pub mod update_checker;

use crate::niyien_device::protocol::FrameParser;
use commands::{DeviceTime, VersionInfo};
use ota::{FirmwarePackage, OtaAction, OtaManager, OtaState};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use serial_backend::DefaultSerialBackend;
use transport::{
    DeviceConnectionConfig, DevicePortCandidate, DeviceTransportBackend, DeviceTransportError,
    DeviceTransportEvent, DeviceTransportStream, RetryBackoff, ScanTracker, filter_matching_ports,
};
use update_checker::FirmwareUpdateInfo;

const SERIAL_LOOP_TICK: Duration = Duration::from_millis(50);
const NETWORK_LOOP_TICK: Duration = Duration::from_millis(100);
const A1_DEVICE_PRODUCT_ID: u8 = 0xA1;

#[derive(Debug)]
pub enum DeviceCommand {
    SyncTime(i16),
    CheckUpdate(String),
    StartOta,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceConnectionStatus {
    Idle,
    RequestingPermission,
    Connected,
    PermissionDenied,
    Unsupported,
    Error,
}

impl DeviceConnectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::RequestingPermission => "requesting_permission",
            Self::Connected => "connected",
            Self::PermissionDenied => "permission_denied",
            Self::Unsupported => "unsupported",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeviceEvent {
    ConnectionStatus(DeviceConnectionStatus, String),
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

enum TransportCommand {
    SyncTime(i16),
    Stop,
}

enum NetworkCommand {
    CheckUpdate(String),
    StartOta,
    Stop,
}

#[derive(Default)]
struct DeviceSharedState {
    latest_update: Option<FirmwareUpdateInfo>,
    prepared_firmware: Option<FirmwarePackage>,
    ota_manager: Option<OtaManager>,
    ota_start_pending: bool,
    ota_last_progress_percent: i32,
    ota_last_progress_at: Option<Instant>,
}

struct DeviceSession<P: DeviceTransportStream> {
    port_name: String,
    stream: P,
    parser: FrameParser,
    version_info: Option<VersionInfo>,
    connected_emitted: bool,
    last_time_poll: Instant,
}

pub struct DeviceManager {
    command_tx: Sender<DeviceCommand>,
    event_rx: Arc<Mutex<Receiver<DeviceEvent>>>,
    running: Arc<AtomicBool>,
    dispatcher_thread: Option<JoinHandle<()>>,
    transport_thread: Option<JoinHandle<()>>,
    network_thread: Option<JoinHandle<()>>,
}

impl DeviceManager {
    pub fn new() -> Self {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            Self::with_backend(DefaultSerialBackend::default(), None)
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            Self::with_backend(
                mobile_backend::DefaultMobileBackend::default(),
                mobile_backend::startup_connection_event(),
            )
        }
    }

    fn with_backend<B: DeviceTransportBackend>(
        backend: B,
        startup_connection_event: Option<(DeviceConnectionStatus, String)>,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (transport_tx, transport_rx) = mpsc::channel();
        let (network_tx, network_rx) = mpsc::channel();

        let running = Arc::new(AtomicBool::new(true));
        let event_rx = Arc::new(Mutex::new(event_rx));
        let shared_state = Arc::new(Mutex::new(DeviceSharedState::default()));

        if let Some((status, message)) = startup_connection_event {
            let _ = event_tx.send(DeviceEvent::ConnectionStatus(status, message));
        }

        let transport_thread = {
            let running = Arc::clone(&running);
            let event_tx = event_tx.clone();
            let shared_state = Arc::clone(&shared_state);
            thread::spawn(move || {
                run_transport_thread(backend, running, transport_rx, event_tx, shared_state)
            })
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
                dispatcher_loop(command_rx, transport_tx, network_tx, running);
            })
        };

        Self {
            command_tx,
            event_rx,
            running,
            dispatcher_thread: Some(dispatcher_thread),
            transport_thread: Some(transport_thread),
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
        if let Some(thread) = self.transport_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.network_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DeviceManager {
    fn drop(&mut self) {
        self.stop();
    }
}

fn dispatcher_loop(
    command_rx: Receiver<DeviceCommand>,
    transport_tx: Sender<TransportCommand>,
    network_tx: Sender<NetworkCommand>,
    running: Arc<AtomicBool>,
) {
    while running.load(SeqCst) {
        match command_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(DeviceCommand::SyncTime(tz_offset_minutes)) => {
                let _ = transport_tx.send(TransportCommand::SyncTime(tz_offset_minutes));
            }
            Ok(DeviceCommand::CheckUpdate(current_version)) => {
                let _ = network_tx.send(NetworkCommand::CheckUpdate(current_version));
            }
            Ok(DeviceCommand::StartOta) => {
                let _ = network_tx.send(NetworkCommand::StartOta);
            }
            Ok(DeviceCommand::Stop) => {
                running.store(false, SeqCst);
                let _ = transport_tx.send(TransportCommand::Stop);
                let _ = network_tx.send(NetworkCommand::Stop);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                running.store(false, SeqCst);
                let _ = transport_tx.send(TransportCommand::Stop);
                let _ = network_tx.send(NetworkCommand::Stop);
                break;
            }
        }
    }
}

fn run_transport_thread<B: DeviceTransportBackend>(
    mut backend: B,
    running: Arc<AtomicBool>,
    transport_rx: Receiver<TransportCommand>,
    event_tx: Sender<DeviceEvent>,
    shared_state: Arc<Mutex<DeviceSharedState>>,
) {
    let config = DeviceConnectionConfig::default();
    let mut scan_tracker = ScanTracker::default();
    let mut backoff = RetryBackoff::new(config.initial_retry_delay, config.max_retry_delay);
    let mut session: Option<DeviceSession<B::Stream>> = None;

    while running.load(SeqCst) {
        while let Some(event) = backend.poll_event() {
            handle_transport_event(event, &mut session, &event_tx, &shared_state);
        }

        match transport_rx.recv_timeout(SERIAL_LOOP_TICK) {
            Ok(TransportCommand::SyncTime(tz_offset_minutes)) => {
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
            Ok(TransportCommand::Stop) => break,
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();

                if !drive_ota_timeout(&mut session, &event_tx, &shared_state, now) {
                    disconnect_session(&mut session, &event_tx, &shared_state);
                    continue;
                }

                if let Some(active) = session.as_mut() {
                    let mut should_disconnect = false;
                    if !poll_device_session(active, &event_tx, &shared_state, now) {
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

fn handle_transport_event<P: DeviceTransportStream>(
    event: DeviceTransportEvent,
    session: &mut Option<DeviceSession<P>>,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
) {
    match event {
        DeviceTransportEvent::ConnectionStatus(status, message) => {
            if matches!(
                status,
                DeviceConnectionStatus::PermissionDenied
                    | DeviceConnectionStatus::Unsupported
                    | DeviceConnectionStatus::Error
            ) {
                disconnect_session(session, event_tx, shared_state);
            }
            let _ = event_tx.send(DeviceEvent::ConnectionStatus(status, message));
        }
        DeviceTransportEvent::Detached => {
            disconnect_session(session, event_tx, shared_state);
            let _ = event_tx.send(DeviceEvent::ConnectionStatus(
                DeviceConnectionStatus::Idle,
                String::new(),
            ));
        }
    }
}

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

fn try_open_candidate<B: DeviceTransportBackend>(
    backend: &mut B,
    candidate: &DevicePortCandidate,
    config: &DeviceConnectionConfig,
    now: Instant,
    _event_tx: &Sender<DeviceEvent>,
) -> Result<DeviceSession<B::Stream>, DeviceTransportError> {
    let mut stream = backend.open(&candidate.port_name, config)?;
    write_packet(&mut stream, &commands::ask_version()).map_err(DeviceTransportError::Io)?;

    Ok(DeviceSession {
        port_name: candidate.port_name.clone(),
        stream,
        parser: FrameParser::new(),
        version_info: None,
        connected_emitted: false,
        last_time_poll: now,
    })
}

fn poll_device_session<P: DeviceTransportStream>(
    session: &mut DeviceSession<P>,
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
                if !handle_device_frame(session, frame, event_tx, shared_state, now) {
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

fn handle_device_frame<P: DeviceTransportStream>(
    session: &mut DeviceSession<P>,
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

fn disconnect_session<P: DeviceTransportStream>(
    session: &mut Option<DeviceSession<P>>,
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

fn write_packet<P: DeviceTransportStream>(stream: &mut P, packet: &[u8]) -> io::Result<()> {
    stream.write_all(packet)?;
    stream.flush()
}

fn send_current_time<P: DeviceTransportStream>(
    session: &mut DeviceSession<P>,
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

fn is_timeout_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn start_pending_ota<P: DeviceTransportStream>(
    session: &mut DeviceSession<P>,
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

fn drive_ota_timeout<P: DeviceTransportStream>(
    session: &mut Option<DeviceSession<P>>,
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

fn handle_ota_frame_action<P: DeviceTransportStream>(
    session: &mut DeviceSession<P>,
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

struct SomeSession<'a, P: DeviceTransportStream>(&'a mut DeviceSession<P>);

fn handle_ota_action<P: DeviceTransportStream>(
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

trait OtaSessionAccess<P: DeviceTransportStream> {
    fn session_mut(&mut self) -> Option<&mut DeviceSession<P>>;
}

impl<P: DeviceTransportStream> OtaSessionAccess<P> for Option<DeviceSession<P>> {
    fn session_mut(&mut self) -> Option<&mut DeviceSession<P>> {
        self.as_mut()
    }
}

impl<'a, P: DeviceTransportStream> OtaSessionAccess<P> for SomeSession<'a, P> {
    fn session_mut(&mut self) -> Option<&mut DeviceSession<P>> {
        Some(self.0)
    }
}

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

fn clear_ota_state(shared_state: &Arc<Mutex<DeviceSharedState>>) {
    let mut shared = shared_state.lock();
    shared.prepared_firmware = None;
    shared.ota_manager = None;
    shared.ota_start_pending = false;
    shared.ota_last_progress_percent = -1;
    shared.ota_last_progress_at = None;
}

fn ota_active(shared_state: &Arc<Mutex<DeviceSharedState>>) -> bool {
    let shared = shared_state.lock();
    shared.ota_manager.is_some()
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use std::{
        collections::VecDeque,
        io,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering::SeqCst},
            mpsc,
        },
        thread,
        time::Duration,
    };

    use chrono::{Datelike, TimeZone, Timelike, Utc};

    use super::{
        commands::{self, DeviceTime, VersionInfo},
        protocol,
        transport::{
            DeviceConnectionConfig, DevicePortCandidate, DeviceTransportBackend,
            DeviceTransportError, DeviceTransportStream,
        },
        *,
    };

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

    enum ReadStep {
        Data(Vec<u8>),
        Error(io::ErrorKind),
    }

    struct ScriptedStream {
        reads: VecDeque<ReadStep>,
        writes: Arc<parking_lot::Mutex<Vec<Vec<u8>>>>,
    }

    impl ScriptedStream {
        fn new(reads: Vec<ReadStep>, writes: Arc<parking_lot::Mutex<Vec<Vec<u8>>>>) -> Self {
            Self {
                reads: reads.into(),
                writes,
            }
        }
    }

    impl DeviceTransportStream for ScriptedStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.reads.pop_front() {
                Some(ReadStep::Data(data)) => {
                    let len = data.len().min(buf.len());
                    buf[..len].copy_from_slice(&data[..len]);
                    Ok(len)
                }
                Some(ReadStep::Error(kind)) => Err(io::Error::from(kind)),
                None => Ok(0),
            }
        }

        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            self.writes.lock().push(buf.to_vec());
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ScriptedBackend {
        stream: Option<ScriptedStream>,
    }

    impl DeviceTransportBackend for ScriptedBackend {
        type Stream = ScriptedStream;

        fn list_ports(&mut self) -> Result<Vec<DevicePortCandidate>, DeviceTransportError> {
            if self.stream.is_none() {
                return Ok(Vec::new());
            }
            Ok(vec![DevicePortCandidate {
                port_name: "scripted".into(),
                vendor_id: Some(0xFFFF),
                product_id: Some(0xFFFF),
                serial_number: None,
            }])
        }

        fn open(
            &mut self,
            _port_name: &str,
            _config: &DeviceConnectionConfig,
        ) -> Result<Self::Stream, DeviceTransportError> {
            self.stream
                .take()
                .ok_or(DeviceTransportError::Unsupported("stream already opened"))
        }
    }

    fn version_frame() -> Vec<u8> {
        let mut payload = vec![0xA1];
        payload.extend_from_slice(b"V1.2.3");
        payload.push(0);
        payload.extend_from_slice(b"HW1");
        payload.push(0);
        payload.extend_from_slice(b"SN0000000001");
        protocol::encode(commands::MSG_CMD_VERSION, &payload)
    }

    fn time_frame() -> Vec<u8> {
        protocol::encode(commands::MSG_CMD_TIME_GET, &[26, 4, 7, 13, 14, 15])
    }

    #[test]
    fn transport_thread_emits_version_time_and_disconnect_events() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let backend = ScriptedBackend {
            stream: Some(ScriptedStream::new(
                vec![
                    ReadStep::Data(version_frame()),
                    ReadStep::Data(time_frame()),
                    ReadStep::Error(io::ErrorKind::BrokenPipe),
                ],
                Arc::clone(&writes),
            )),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state);
            })
        };

        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::Connected(VersionInfo {
                product_id: 0xA1,
                soft_version: "V1.2.3".into(),
                hard_version: "HW1".into(),
                serial_number: *b"SN0000000001",
            })
        );
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::TimeReceived(DeviceTime {
                year: 2026,
                month: 4,
                day: 7,
                hour: 13,
                minute: 14,
                second: 15,
            })
        );
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::Disconnected
        );
        assert_eq!(writes.lock().first(), Some(&commands::ask_version()));

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn transport_thread_reports_sync_failure_without_active_session() {
        let backend = ScriptedBackend {
            stream: None,
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state);
            })
        };

        command_tx.send(TransportCommand::SyncTime(480)).unwrap();
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::TimeSyncResult(false)
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    struct EventBackend {
        events: VecDeque<transport::DeviceTransportEvent>,
    }

    impl DeviceTransportBackend for EventBackend {
        type Stream = ScriptedStream;

        fn list_ports(&mut self) -> Result<Vec<DevicePortCandidate>, DeviceTransportError> {
            Ok(Vec::new())
        }

        fn open(
            &mut self,
            _port_name: &str,
            _config: &DeviceConnectionConfig,
        ) -> Result<Self::Stream, DeviceTransportError> {
            Err(DeviceTransportError::Unsupported("no stream"))
        }

        fn poll_event(&mut self) -> Option<transport::DeviceTransportEvent> {
            self.events.pop_front()
        }
    }

    #[test]
    fn transport_thread_forwards_platform_connection_status_events() {
        let backend = EventBackend {
            events: VecDeque::from([transport::DeviceTransportEvent::ConnectionStatus(
                DeviceConnectionStatus::RequestingPermission,
                "Requesting USB permission".to_owned(),
            )]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state);
            })
        };

        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::ConnectionStatus(
                DeviceConnectionStatus::RequestingPermission,
                "Requesting USB permission".to_owned()
            )
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }
}
