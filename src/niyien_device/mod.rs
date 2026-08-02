// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::{
    io,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering::SeqCst},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
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
pub mod timezone;
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
// Bounds the OTA fast-pump loop rate on backends whose read returns
// immediately with no data instead of blocking on a timeout.
const OTA_PUMP_IDLE_SLEEP: Duration = Duration::from_millis(1);
const A1_DEVICE_PRODUCT_ID: u8 = 0xA1;
// The device firmware may leave its CDC TX dead for the first serial session
// opened after a powered USB re-enumeration; closing and reopening the port
// recovers it. Probe the version periodically and abandon the session after
// HANDSHAKE_MAX_PROBES unanswered probes so the scan loop opens a fresh one.
const HANDSHAKE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const HANDSHAKE_MAX_PROBES: u32 = 3;
// Measured 2026-07-13: a ~160ms close->reopen gap never recovers a wedged
// device (its firmware misses the short DTR-low pulse), while a dwell of
// seconds does. Keep the port closed this long after giving up a handshake.
const HANDSHAKE_REOPEN_COOLDOWN: Duration = Duration::from_secs(5);

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
    last_version_probe: Instant,
    version_probes_sent: u32,
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
            let fast_pump = ota_fast_pump_enabled();
            thread::spawn(move || {
                run_transport_thread(
                    backend,
                    running,
                    transport_rx,
                    event_tx,
                    shared_state,
                    fast_pump,
                )
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
    fast_pump_enabled: bool,
) {
    let config = DeviceConnectionConfig::default();
    let mut scan_tracker = ScanTracker::default();
    let mut backoff = RetryBackoff::new(config.initial_retry_delay, config.max_retry_delay);
    let mut session: Option<DeviceSession<B::Stream>> = None;
    let mut reopen_not_before: Option<Instant> = None;

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
                    let mut handshake_given_up = false;
                    if poll_device_session(active, &event_tx, &shared_state, now)
                        == SessionPoll::Lost
                    {
                        should_disconnect = true;
                    } else if active.version_info.is_none() {
                        if now.saturating_duration_since(active.last_version_probe)
                            >= HANDSHAKE_RETRY_INTERVAL
                        {
                            if active.version_probes_sent >= HANDSHAKE_MAX_PROBES {
                                log::info!(
                                    "NiYien handshake: giving up session on {} after {} probes, will reopen",
                                    active.port_name,
                                    active.version_probes_sent
                                );
                                handshake_given_up = true;
                                should_disconnect = true;
                            } else if let Err(err) =
                                write_packet(&mut active.stream, &commands::ask_version())
                            {
                                log::warn!(
                                    "Failed to resend version probe to {}: {}",
                                    active.port_name,
                                    err
                                );
                                should_disconnect = true;
                            } else {
                                active.version_probes_sent += 1;
                                active.last_version_probe = now;
                                log::info!(
                                    "NiYien handshake: no version reply on {}, resending probe {}/{}",
                                    active.port_name,
                                    active.version_probes_sent,
                                    HANDSHAKE_MAX_PROBES
                                );
                            }
                        }
                    } else if !ota_active(&shared_state)
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
                        if handshake_given_up {
                            // Throttle the reopen cycle so a genuinely dead
                            // device does not cause a tight open/probe loop.
                            backoff.record_failure(now);
                            reopen_not_before = Some(now + HANDSHAKE_REOPEN_COOLDOWN);
                        }
                    } else if fast_pump_enabled
                        && session
                            .as_ref()
                            .is_some_and(|active| active.version_info.is_some())
                        && ota_transfer_pending(&shared_state)
                        && run_ota_pump_loop(
                            &mut backend,
                            &running,
                            &transport_rx,
                            &mut session,
                            &event_tx,
                            &shared_state,
                        ) == OtaPumpExit::Stop
                    {
                        break;
                    }
                    continue;
                }

                if let Some(deadline) = reopen_not_before {
                    if now < deadline {
                        continue;
                    }
                    reopen_not_before = None;
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
        last_version_probe: now,
        version_probes_sent: 1,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionPoll {
    // No bytes arrived this pass; the fast pump uses this to pace itself.
    Idle,
    Activity,
    Lost,
}

fn poll_device_session<P: DeviceTransportStream>(
    session: &mut DeviceSession<P>,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
    now: Instant,
) -> SessionPoll {
    let mut buf = [0u8; 512];
    match session.stream.read(&mut buf) {
        Ok(read) => {
            if read == 0 {
                session.parser.clear_if_timed_out_at(now);
                return SessionPoll::Idle;
            }

            for frame in session.parser.feed_at(&buf[..read], now) {
                if !handle_device_frame(session, frame, event_tx, shared_state, now) {
                    return SessionPoll::Lost;
                }
            }
            SessionPoll::Activity
        }
        Err(err) if is_timeout_error(&err) => {
            session.parser.clear_if_timed_out_at(now);
            SessionPoll::Idle
        }
        Err(err) => {
            if is_detach_error(&err) {
                log::info!("NiYien device detached from {}: {}", session.port_name, err);
            } else {
                log::warn!("Serial read failed on {}: {}", session.port_name, err);
            }
            SessionPoll::Lost
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

    let Some(active) = session.take() else {
        return;
    };

    // Losing the session mid-transfer strands the OTA state machine: no ACK
    // can ever arrive, so fail it now instead of letting the per-state
    // timeout fire seconds later (during which the still-live progress
    // heartbeat would flip the UI back to "updating").
    if ota_transfer_pending(shared_state) {
        clear_ota_state(shared_state);
        let _ = event_tx.send(DeviceEvent::OtaFailed(
            "The device was disconnected during OTA transfer".to_owned(),
        ));
    }

    if active.connected_emitted && !waiting_reconnect {
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

// Errors that usbser-style drivers return once the underlying USB device left
// the bus (unplug or link bounce). These are the normal detach detection path
// on the serial backend, not a protocol failure, so they log as INFO.
fn is_detach_error(err: &io::Error) -> bool {
    #[cfg(windows)]
    {
        // ERROR_BAD_COMMAND (22), ERROR_OPERATION_ABORTED (995),
        // ERROR_DEVICE_NOT_CONNECTED (1167), ERROR_NO_SUCH_DEVICE (433)
        if matches!(err.raw_os_error(), Some(22 | 995 | 1167 | 433)) {
            return true;
        }
    }
    matches!(
        err.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected
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

// Fast-pump eligibility: only the active transfer states benefit from the
// read-driven inner loop. WaitingReconnect must stay on the slow outer loop
// so the port rescan keeps its normal cadence while the device reboots.
fn is_ota_transfer_state(state: OtaState) -> bool {
    matches!(
        state,
        OtaState::Version
            | OtaState::VersionWait
            | OtaState::Begin
            | OtaState::BeginWait
            | OtaState::BinInfo
            | OtaState::BinInfoWait
            | OtaState::Trans
            | OtaState::TransWait
            | OtaState::Verify
            | OtaState::VerifyWait
    )
}

fn ota_transfer_pending(shared_state: &Arc<Mutex<DeviceSharedState>>) -> bool {
    let shared = shared_state.lock();
    shared
        .ota_manager
        .as_ref()
        .is_some_and(|manager| is_ota_transfer_state(manager.state()))
}

fn parse_fast_pump_flag(raw: Option<&str>) -> (bool, &'static str) {
    match raw.map(|value| value.trim().to_ascii_lowercase()) {
        None => (true, "default"),
        Some(value) if matches!(value.as_str(), "0" | "off" | "false" | "no") => (false, "env"),
        Some(value) if matches!(value.as_str(), "1" | "on" | "true" | "yes") => (true, "env"),
        Some(_) => (true, "default_invalid"),
    }
}

fn ota_fast_pump_enabled() -> bool {
    static RESOLVED: OnceLock<bool> = OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_NIYIEN_OTA_FAST_PUMP").ok();
        let (enabled, source) = parse_fast_pump_flag(raw.as_deref());
        log::info!(target: "update", "ota fast pump resolved: enabled={enabled} source={source}");
        enabled
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OtaPumpExit {
    Continue,
    Stop,
}

// Read-driven inner loop for active OTA transfers. The outer loop paces every
// step on SERIAL_LOOP_TICK, which caps the stop-and-wait 128-byte chunks at
// one per tick (~2.5 KB/s). While a transfer is in flight this loop blocks
// directly on the serial read (bounded by the port read timeout) so the next
// chunk goes out as soon as the ACK arrives.
fn run_ota_pump_loop<B: DeviceTransportBackend>(
    backend: &mut B,
    running: &AtomicBool,
    transport_rx: &Receiver<TransportCommand>,
    session: &mut Option<DeviceSession<B::Stream>>,
    event_tx: &Sender<DeviceEvent>,
    shared_state: &Arc<Mutex<DeviceSharedState>>,
) -> OtaPumpExit {
    while running.load(SeqCst) {
        match transport_rx.try_recv() {
            Ok(TransportCommand::Stop) => return OtaPumpExit::Stop,
            Ok(TransportCommand::SyncTime(tz_offset_minutes)) => {
                // Same semantics as the outer loop: a user-triggered time
                // sync interleaves with the OTA byte stream.
                if let Some(active) = session.as_mut() {
                    if let Err(err) = send_current_time(active, tz_offset_minutes) {
                        log::warn!("Failed to send SyncTime to {}: {}", active.port_name, err);
                        disconnect_session(session, event_tx, shared_state);
                        let _ = event_tx.send(DeviceEvent::TimeSyncResult(false));
                        return OtaPumpExit::Continue;
                    }
                } else {
                    let _ = event_tx.send(DeviceEvent::TimeSyncResult(false));
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return OtaPumpExit::Stop,
        }

        while let Some(event) = backend.poll_event() {
            handle_transport_event(event, session, event_tx, shared_state);
        }

        let now = Instant::now();
        if !drive_ota_timeout(session, event_tx, shared_state, now) {
            disconnect_session(session, event_tx, shared_state);
            return OtaPumpExit::Continue;
        }

        if !ota_transfer_pending(shared_state) {
            return OtaPumpExit::Continue;
        }
        let Some(active) = session.as_mut() else {
            return OtaPumpExit::Continue;
        };

        match poll_device_session(active, event_tx, shared_state, now) {
            SessionPoll::Lost => {
                disconnect_session(session, event_tx, shared_state);
                return OtaPumpExit::Continue;
            }
            SessionPoll::Idle => thread::sleep(OTA_PUMP_IDLE_SLEEP),
            SessionPoll::Activity => {}
        }
    }
    OtaPumpExit::Continue
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
            atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
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
        streams: VecDeque<ScriptedStream>,
    }

    impl DeviceTransportBackend for ScriptedBackend {
        type Stream = ScriptedStream;

        fn list_ports(&mut self) -> Result<Vec<DevicePortCandidate>, DeviceTransportError> {
            if self.streams.is_empty() {
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
            self.streams
                .pop_front()
                .ok_or(DeviceTransportError::Unsupported("stream already opened"))
        }
    }

    fn version_frame_with(soft: &str) -> Vec<u8> {
        let mut payload = vec![0xA1];
        payload.extend_from_slice(soft.as_bytes());
        payload.push(0);
        payload.extend_from_slice(b"HW1");
        payload.push(0);
        payload.extend_from_slice(b"SN0000000001");
        protocol::encode(commands::MSG_CMD_VERSION, &payload)
    }

    fn version_frame() -> Vec<u8> {
        version_frame_with("V1.2.3")
    }

    fn time_frame() -> Vec<u8> {
        protocol::encode(commands::MSG_CMD_TIME_GET, &[26, 4, 7, 13, 14, 15])
    }

    #[test]
    fn transport_thread_emits_version_time_and_disconnect_events() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(
                vec![
                    ReadStep::Data(version_frame()),
                    ReadStep::Data(time_frame()),
                    ReadStep::Error(io::ErrorKind::BrokenPipe),
                ],
                Arc::clone(&writes),
            )]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
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
            streams: VecDeque::new(),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
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

    fn expected_version_info() -> VersionInfo {
        VersionInfo {
            product_id: 0xA1,
            soft_version: "V1.2.3".into(),
            hard_version: "HW1".into(),
            serial_number: *b"SN0000000001",
        }
    }

    fn count_version_probes(writes: &parking_lot::Mutex<Vec<Vec<u8>>>) -> usize {
        let probe = commands::ask_version();
        writes.lock().iter().filter(|w| **w == probe).count()
    }

    #[test]
    fn handshake_resends_version_probe_until_reply() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // ~25 read ticks (>= 1.25s) of silence before the version reply, so at
        // least one probe resend must have fired by then.
        let mut reads: Vec<ReadStep> = (0..25)
            .map(|_| ReadStep::Error(io::ErrorKind::TimedOut))
            .collect();
        reads.push(ReadStep::Data(version_frame()));
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(reads, Arc::clone(&writes))]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            DeviceEvent::Connected(expected_version_info())
        );
        let probes = count_version_probes(&writes);
        assert!(
            probes >= 2,
            "expected at least one version probe resend, got {probes}"
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn handshake_gives_up_after_max_probes_and_reopens() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // First session never answers (reads return 0 bytes forever); the
        // session must be abandoned after HANDSHAKE_MAX_PROBES and the scan
        // loop must open the second, healthy stream.
        let mute = ScriptedStream::new(Vec::new(), Arc::clone(&writes));
        let healthy = ScriptedStream::new(
            vec![ReadStep::Data(version_frame())],
            Arc::clone(&writes),
        );
        let backend = ScriptedBackend {
            streams: VecDeque::from([mute, healthy]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        // The abandoned session never reported Connected, so no Disconnected
        // event may precede the second session's Connected.
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(15)).unwrap(),
            DeviceEvent::Connected(expected_version_info())
        );
        let probes = count_version_probes(&writes);
        assert!(
            probes >= HANDSHAKE_MAX_PROBES as usize + 1,
            "expected {} probes from the abandoned session plus one from the new session, got {probes}",
            HANDSHAKE_MAX_PROBES
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn handshake_fast_path_sends_single_version_probe() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(
                vec![ReadStep::Data(version_frame())],
                Arc::clone(&writes),
            )]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = Arc::new(parking_lot::Mutex::new(DeviceSharedState::default()));

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            DeviceEvent::Connected(expected_version_info())
        );
        // Let the post-connect polling cadence run past the retry interval to
        // prove no extra version probe is emitted once connected.
        thread::sleep(Duration::from_millis(1500));

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();

        assert_eq!(count_version_probes(&writes), 1);
        assert_eq!(writes.lock().first(), Some(&commands::ask_version()));
    }

    #[test]
    fn classifies_detach_errors() {
        #[cfg(windows)]
        {
            assert!(is_detach_error(&io::Error::from_raw_os_error(22)));
            assert!(is_detach_error(&io::Error::from_raw_os_error(995)));
            assert!(is_detach_error(&io::Error::from_raw_os_error(1167)));
            assert!(is_detach_error(&io::Error::from_raw_os_error(433)));
        }
        assert!(is_detach_error(&io::Error::from(io::ErrorKind::BrokenPipe)));
        assert!(is_detach_error(&io::Error::from(
            io::ErrorKind::NotConnected
        )));
        assert!(!is_detach_error(&io::Error::from(io::ErrorKind::InvalidData)));
        assert!(!is_detach_error(&io::Error::from(io::ErrorKind::TimedOut)));
        assert!(!is_timeout_error(&io::Error::from(io::ErrorKind::BrokenPipe)));
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
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
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

    fn ota_version_reply() -> Vec<u8> {
        // OTA version replies carry no serial bytes (require_serial=false).
        let mut payload = vec![0xA1];
        payload.extend_from_slice(b"V1.2.3");
        payload.push(0);
        payload.extend_from_slice(b"HW1");
        payload.push(0);
        protocol::encode(commands::MSG_CMD_OTA_VERSION, &payload)
    }

    fn ota_ack_reply(cmd: u8) -> Vec<u8> {
        protocol::encode(cmd, &[0])
    }

    fn ota_trans_ack_reply(index: u32) -> Vec<u8> {
        let mut payload = vec![0u8];
        payload.extend_from_slice(&index.to_le_bytes());
        protocol::encode(commands::MSG_CMD_OTA_TRANS, &payload)
    }

    fn test_firmware(bin_len: usize) -> ota::FirmwarePackage {
        ota::FirmwarePackage {
            company_name: "NiYien".into(),
            product_name: "A1".into(),
            version: "V1.4.0".into(),
            magic_num: 0x1234ABCD,
            crc: 0x89ABCDEF,
            bin_data: (0..bin_len).map(|i| (i % 251) as u8).collect(),
            changelog_en: String::new(),
            changelog_zh: String::new(),
        }
    }

    fn armed_ota_shared_state(bin_len: usize) -> Arc<parking_lot::Mutex<DeviceSharedState>> {
        let firmware = test_firmware(bin_len);
        Arc::new(parking_lot::Mutex::new(DeviceSharedState {
            latest_update: None,
            prepared_firmware: Some(firmware.clone()),
            ota_manager: Some(OtaManager::new(firmware)),
            ota_start_pending: true,
            ota_last_progress_percent: -1,
            ota_last_progress_at: None,
        }))
    }

    fn ota_success_script(chunks: u32) -> Vec<ReadStep> {
        let mut reads = vec![
            ReadStep::Data(version_frame()),
            ReadStep::Data(ota_version_reply()),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_BEGIN)),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_INFO)),
        ];
        for index in 0..chunks {
            reads.push(ReadStep::Data(ota_trans_ack_reply(index)));
        }
        reads.push(ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_VERIFY)));
        // The "rebooted" device reports the freshly flashed version.
        reads.push(ReadStep::Data(version_frame_with("V1.4.0")));
        reads
    }

    fn wait_for_event(
        event_rx: &mpsc::Receiver<DeviceEvent>,
        deadline: Duration,
        mut predicate: impl FnMut(&DeviceEvent) -> bool,
    ) -> Option<DeviceEvent> {
        let start = Instant::now();
        while start.elapsed() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    if predicate(&event) {
                        return Some(event);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    struct CountingBackend {
        inner: ScriptedBackend,
        list_calls: Arc<AtomicUsize>,
    }

    impl DeviceTransportBackend for CountingBackend {
        type Stream = ScriptedStream;

        fn list_ports(&mut self) -> Result<Vec<DevicePortCandidate>, DeviceTransportError> {
            self.list_calls.fetch_add(1, SeqCst);
            self.inner.list_ports()
        }

        fn open(
            &mut self,
            port_name: &str,
            config: &DeviceConnectionConfig,
        ) -> Result<Self::Stream, DeviceTransportError> {
            self.inner.open(port_name, config)
        }
    }

    #[test]
    fn fast_pump_completes_ota_without_tick_pacing() {
        let chunks = 100u32;
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(
                ota_success_script(chunks),
                Arc::clone(&writes),
            )]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = armed_ota_shared_state(chunks as usize * 128);

        let started = Instant::now();
        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        assert!(
            wait_for_event(&event_rx, Duration::from_secs(10), |event| matches!(
                event,
                DeviceEvent::OtaComplete
            ))
            .is_some(),
            "OTA did not complete"
        );
        let elapsed = started.elapsed();
        // Tick-paced transfer would need >= (chunks + protocol steps) * 50ms
        // (~5.6s here); the read-driven pump must finish far below that.
        assert!(
            elapsed < Duration::from_secs(2),
            "OTA took {elapsed:?}, transfer still tick-paced"
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fast_pump_stop_command_exits_promptly() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // ACK only the first chunk, then go silent: the pump idles in
        // TransWait when the stop command arrives.
        let reads = vec![
            ReadStep::Data(version_frame()),
            ReadStep::Data(ota_version_reply()),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_BEGIN)),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_INFO)),
            ReadStep::Data(ota_trans_ack_reply(0)),
        ];
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(reads, Arc::clone(&writes))]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = armed_ota_shared_state(16 * 128);

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        assert!(
            wait_for_event(&event_rx, Duration::from_secs(5), |event| {
                matches!(event, DeviceEvent::OtaProgress(p) if *p > 0.0)
            })
            .is_some(),
            "transfer never reported progress"
        );

        let stop_sent = Instant::now();
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
        assert!(
            stop_sent.elapsed() < Duration::from_secs(2),
            "transport thread took {:?} to honor stop during OTA",
            stop_sent.elapsed()
        );
    }

    #[test]
    fn fast_pump_exits_to_slow_scan_during_waiting_reconnect() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // Single-chunk firmware: verify + reboot right after the first ACK,
        // then the stream dies like a real reboot.
        let reads = vec![
            ReadStep::Data(version_frame()),
            ReadStep::Data(ota_version_reply()),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_BEGIN)),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_INFO)),
            ReadStep::Data(ota_trans_ack_reply(0)),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_VERIFY)),
            ReadStep::Error(io::ErrorKind::BrokenPipe),
        ];
        let list_calls = Arc::new(AtomicUsize::new(0));
        let backend = CountingBackend {
            inner: ScriptedBackend {
                streams: VecDeque::from([ScriptedStream::new(reads, Arc::clone(&writes))]),
            },
            list_calls: Arc::clone(&list_calls),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, _event_rx) = mpsc::channel();
        let shared_state = armed_ota_shared_state(128);

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        // Give the thread time to finish the transfer and sit in
        // WaitingReconnect with the session gone.
        thread::sleep(Duration::from_millis(800));
        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();

        let calls = list_calls.load(SeqCst);
        // Scan cadence is one list_ports per 50ms tick; a hot loop would rack
        // up hundreds of calls within the sleep window.
        assert!(calls <= 30, "list_ports called {calls} times, scan loop running hot");
        assert!(calls >= 2, "scan never resumed after the device rebooted");
    }

    #[test]
    fn fast_pump_detach_mid_transfer_converges_to_failure() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // The device vanishes mid-transfer (detach-class read error while a
        // chunk is awaiting its ACK). The session must drop immediately and
        // the stranded OTA must still fail via the TransWait timeout.
        let reads = vec![
            ReadStep::Data(version_frame()),
            ReadStep::Data(ota_version_reply()),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_BEGIN)),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_INFO)),
            ReadStep::Data(ota_trans_ack_reply(0)),
            ReadStep::Error(io::ErrorKind::BrokenPipe),
        ];
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(reads, Arc::clone(&writes))]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = armed_ota_shared_state(16 * 128);

        let started = Instant::now();
        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        // Fail-fast: the stranded OTA must fail the moment the session drops,
        // not seconds later via the TransWait timeout (which would need >3s).
        let event = wait_for_event(&event_rx, Duration::from_secs(5), |event| {
            matches!(event, DeviceEvent::OtaFailed(_))
        });
        let Some(DeviceEvent::OtaFailed(message)) = event else {
            panic!("stranded OTA never converged to OtaFailed");
        };
        assert!(
            message.contains("disconnected"),
            "unexpected failure message: {message}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "OtaFailed took {:?}, fail-fast on disconnect not working",
            started.elapsed()
        );
        assert!(
            wait_for_event(&event_rx, Duration::from_secs(2), |event| matches!(
                event,
                DeviceEvent::Disconnected
            ))
            .is_some(),
            "mid-transfer detach did not emit Disconnected"
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fast_pump_trans_timeout_still_fails() {
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // Device goes silent after the INFO ack: the first chunk is sent but
        // never ACKed, so the 3s TransWait timeout must fire from inside the
        // pump loop.
        let reads = vec![
            ReadStep::Data(version_frame()),
            ReadStep::Data(ota_version_reply()),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_BEGIN)),
            ReadStep::Data(ota_ack_reply(commands::MSG_CMD_OTA_INFO)),
        ];
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(reads, Arc::clone(&writes))]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = armed_ota_shared_state(16 * 128);

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, true);
            })
        };

        let event = wait_for_event(&event_rx, Duration::from_secs(8), |event| {
            matches!(event, DeviceEvent::OtaFailed(_))
        });
        let Some(DeviceEvent::OtaFailed(message)) = event else {
            panic!("expected OtaFailed after silent TransWait");
        };
        assert!(
            message.contains("timed out"),
            "unexpected failure message: {message}"
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn disabled_fast_pump_still_completes_ota_on_tick_path() {
        let chunks = 2u32;
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let backend = ScriptedBackend {
            streams: VecDeque::from([ScriptedStream::new(
                ota_success_script(chunks),
                Arc::clone(&writes),
            )]),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let shared_state = armed_ota_shared_state(chunks as usize * 128);

        let handle = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                run_transport_thread(backend, running, command_rx, event_tx, shared_state, false);
            })
        };

        assert!(
            wait_for_event(&event_rx, Duration::from_secs(10), |event| matches!(
                event,
                DeviceEvent::OtaComplete
            ))
            .is_some(),
            "tick-driven OTA path did not complete"
        );

        running.store(false, SeqCst);
        command_tx.send(TransportCommand::Stop).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fast_pump_state_gate_covers_transfer_states_only() {
        use OtaState::*;
        for state in [
            Version, VersionWait, Begin, BeginWait, BinInfo, BinInfoWait, Trans, TransWait,
            Verify, VerifyWait,
        ] {
            assert!(is_ota_transfer_state(state), "{state:?} should be pump-eligible");
        }
        for state in [Idle, Reboot, WaitingReconnect, Success, Failed] {
            assert!(!is_ota_transfer_state(state), "{state:?} must stay on the slow loop");
        }
    }

    #[test]
    fn parses_fast_pump_flag() {
        assert_eq!(parse_fast_pump_flag(None), (true, "default"));
        for disable in ["0", "off", "false", "no", " OFF ", "No"] {
            assert_eq!(parse_fast_pump_flag(Some(disable)), (false, "env"), "{disable}");
        }
        for enable in ["1", "on", "true", "YES"] {
            assert_eq!(parse_fast_pump_flag(Some(enable)), (true, "env"), "{enable}");
        }
        assert_eq!(parse_fast_pump_flag(Some("weird")), (true, "default_invalid"));
    }
}
