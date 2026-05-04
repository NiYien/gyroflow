// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#[cfg(target_os = "android")]
mod android {
    use std::{
        collections::VecDeque,
        io,
        sync::{
            Arc, LazyLock,
            atomic::{AtomicBool, Ordering::SeqCst},
        },
        time::{Duration, Instant},
    };

    use jni::{
        Env,
        objects::{JByteArray, JClass, JObject, JString, JValue},
        sys::{jboolean, jint},
    };
    use parking_lot::Mutex;

    use crate::niyien_device::{
        DeviceConnectionStatus,
        transport::{
            DeviceConnectionConfig, DevicePortCandidate, DeviceTransportBackend,
            DeviceTransportError, DeviceTransportEvent, DeviceTransportStream,
        },
    };

    const ANDROID_PORT_NAME: &str = "android-usb-niyien-a1";
    const JNI_TRUE: jboolean = true;

    static ANDROID_BRIDGE: LazyLock<Arc<AndroidBridgeState>> =
        LazyLock::new(|| Arc::new(AndroidBridgeState::default()));

    #[derive(Default)]
    struct AndroidBridgeState {
        events: Mutex<VecDeque<DeviceTransportEvent>>,
        read_chunks: Mutex<VecDeque<Vec<u8>>>,
        opened_device: Mutex<Option<(u16, u16)>>,
        opened: AtomicBool,
    }

    impl AndroidBridgeState {
        fn push_status(&self, status: DeviceConnectionStatus, message: impl Into<String>) {
            self.events
                .lock()
                .push_back(DeviceTransportEvent::ConnectionStatus(status, message.into()));
        }

        fn push_detached(&self) {
            self.opened.store(false, SeqCst);
            *self.opened_device.lock() = None;
            self.read_chunks.lock().clear();
            self.events.lock().push_back(DeviceTransportEvent::Detached);
        }

        fn push_bytes(&self, bytes: Vec<u8>) {
            self.read_chunks.lock().push_back(bytes);
        }

        fn pop_event(&self) -> Option<DeviceTransportEvent> {
            self.events.lock().pop_front()
        }

        fn pop_bytes(&self) -> Option<Vec<u8>> {
            self.read_chunks.lock().pop_front()
        }

        fn mark_opened(&self, vid: u16, pid: u16) {
            *self.opened_device.lock() = Some((vid, pid));
            self.opened.store(true, SeqCst);
        }

        fn opened_candidate(&self) -> Option<DevicePortCandidate> {
            if !self.opened.load(SeqCst) {
                return None;
            }
            let (vid, pid) = (*self.opened_device.lock())?;
            Some(DevicePortCandidate {
                port_name: ANDROID_PORT_NAME.to_owned(),
                vendor_id: Some(vid),
                product_id: Some(pid),
                serial_number: None,
            })
        }
    }

    pub struct AndroidUsbBackend {
        bridge: Arc<AndroidBridgeState>,
        last_scan_request: Option<Instant>,
    }

    impl Default for AndroidUsbBackend {
        fn default() -> Self {
            Self {
                bridge: Arc::clone(&ANDROID_BRIDGE),
                last_scan_request: None,
            }
        }
    }

    pub struct AndroidUsbStream {
        bridge: Arc<AndroidBridgeState>,
    }

    impl DeviceTransportStream for AndroidUsbStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.bridge.opened.load(SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Android USB device is not open",
                ));
            }

            let Some(chunk) = self.bridge.pop_bytes() else {
                return Ok(0);
            };

            let len = chunk.len().min(buf.len());
            buf[..len].copy_from_slice(&chunk[..len]);
            if len < chunk.len() {
                self.bridge.read_chunks.lock().push_front(chunk[len..].to_vec());
            }
            Ok(len)
        }

        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            android_write_device_bytes(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for AndroidUsbStream {
        fn drop(&mut self) {
            android_close_device();
        }
    }

    impl DeviceTransportBackend for AndroidUsbBackend {
        type Stream = AndroidUsbStream;

        fn list_ports(&mut self) -> Result<Vec<DevicePortCandidate>, DeviceTransportError> {
            self.request_scan_if_due();
            Ok(self
                .bridge
                .opened_candidate()
                .into_iter()
                .collect::<Vec<_>>())
        }

        fn open(
            &mut self,
            port_name: &str,
            _config: &DeviceConnectionConfig,
        ) -> Result<Self::Stream, DeviceTransportError> {
            if port_name != ANDROID_PORT_NAME || !self.bridge.opened.load(SeqCst) {
                return Err(DeviceTransportError::Unsupported(
                    "Android USB device is not open",
                ));
            }
            Ok(AndroidUsbStream {
                bridge: Arc::clone(&self.bridge),
            })
        }

        fn poll_event(&mut self) -> Option<DeviceTransportEvent> {
            self.bridge.pop_event()
        }
    }

    impl AndroidUsbBackend {
        fn request_scan_if_due(&mut self) {
            let now = Instant::now();
            let should_scan = self
                .last_scan_request
                .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(1));
            if should_scan {
                self.last_scan_request = Some(now);
                android_request_usb_scan();
            }
        }
    }

    fn android_java_vm() -> jni::JavaVM {
        unsafe { jni::JavaVM::from_raw(ndk_context::android_context().vm().cast()) }
    }

    fn android_main_activity_class<'local>(
        env: &mut Env<'local>,
    ) -> Result<JClass<'local>, jni::errors::Error> {
        let activity = unsafe {
            JObject::from_raw(env, ndk_context::android_context().context().cast())
        };
        let activity_class = env.get_object_class(&activity)?;
        let class_loader = activity_class.get_class_loader(env)?;
        let class_name = env.new_string("com.niyien.gyroflow.MainActivity")?;
        JClass::for_name_with_loader(env, class_name, true, class_loader)
    }

    fn android_request_usb_scan() {
        let jvm = android_java_vm();
        let result = jvm.attach_current_thread(|env| {
            let class = android_main_activity_class(env)?;
            env.call_static_method(
                class,
                jni::jni_str!("requestUsbDeviceScan"),
                jni::jni_sig!("()V"),
                &[],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(err) = result {
            ANDROID_BRIDGE.push_status(
                DeviceConnectionStatus::Error,
                format!("Failed to request Android USB scan: {err}"),
            );
        }
    }

    fn android_write_device_bytes(buf: &[u8]) -> io::Result<()> {
        let jvm = android_java_vm();
        let result = jvm.attach_current_thread(|env| {
            let class = android_main_activity_class(env)?;
            let bytes = env.byte_array_from_slice(buf)?;
            let ok = env
                .call_static_method(
                    class,
                    jni::jni_str!("writeDeviceBytes"),
                    jni::jni_sig!("([B)Z"),
                    &[JValue::Object(bytes.as_ref())],
                )?
                .z()?;
            Ok::<bool, jni::errors::Error>(ok)
        });

        match result {
            Ok(true) => Ok(()),
            Ok(false) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Android USB write failed",
            )),
            Err(err) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("Failed to call Android USB write: {err}"),
            )),
        }
    }

    fn android_close_device() {
        let jvm = android_java_vm();
        let result = jvm.attach_current_thread(|env| {
            let class = android_main_activity_class(env)?;
            env.call_static_method(
                class,
                jni::jni_str!("closeDeviceFromRust"),
                jni::jni_sig!("()V"),
                &[],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(err) = result {
            log::warn!("Failed to close Android USB device: {err}");
        }
    }

    fn jstring_to_string(env: &Env<'_>, value: &JString<'_>) -> String {
        value.try_to_string(env).unwrap_or_default()
    }

    pub fn startup_connection_event() -> Option<(DeviceConnectionStatus, String)> {
        None
    }

    #[allow(non_snake_case)]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_nativeOnUsbAttached(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
        vid: jint,
        pid: jint,
    ) {
        ANDROID_BRIDGE.push_status(
            DeviceConnectionStatus::RequestingPermission,
            "Requesting USB permission for NiYien A1",
        );
    }

    #[allow(non_snake_case)]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_nativeOnUsbDetached(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
    ) {
        ANDROID_BRIDGE.push_detached();
    }

    #[allow(non_snake_case)]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_nativeOnUsbPermission(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
        granted: jboolean,
    ) {
        let granted = granted == JNI_TRUE;
        if granted {
            ANDROID_BRIDGE.push_status(
                DeviceConnectionStatus::RequestingPermission,
                "USB permission granted; opening NiYien A1",
            );
        } else {
            ANDROID_BRIDGE.opened.store(false, SeqCst);
            ANDROID_BRIDGE.push_status(
                DeviceConnectionStatus::PermissionDenied,
                "USB permission was denied",
            );
        }
    }

    #[allow(non_snake_case)]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_nativeOnUsbOpened(
        _env: *mut std::ffi::c_void,
        _class: *mut std::ffi::c_void,
        vid: jint,
        pid: jint,
    ) {
        let vid = vid as u16;
        let pid = pid as u16;
        ANDROID_BRIDGE.mark_opened(vid, pid);
        ANDROID_BRIDGE.push_status(
            DeviceConnectionStatus::RequestingPermission,
            "USB channel opened; reading NiYien A1 device information",
        );
    }

    #[allow(non_snake_case)]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_nativeOnUsbBytes<'local>(
        mut env: jni::EnvUnowned<'local>,
        _class: JClass<'local>,
        buf: JByteArray<'local>,
        len: jint,
    ) {
        let _ = env
            .with_env(|env| {
                let mut bytes = env.convert_byte_array(&buf)?;
                let len = (len as usize).min(bytes.len());
                bytes.truncate(len);
                ANDROID_BRIDGE.push_bytes(bytes);
                Ok::<(), jni::errors::Error>(())
            })
            .into_outcome();
    }

    #[allow(non_snake_case)]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_nativeOnUsbWriteResult<'local>(
        mut env: jni::EnvUnowned<'local>,
        _class: JClass<'local>,
        ok: jboolean,
        err: JString<'local>,
    ) {
        let _ = env
            .with_env(|env| {
                if ok != JNI_TRUE {
                    let message = jstring_to_string(env, &err);
                    log::warn!("Android USB write failed: {message}");
                    ANDROID_BRIDGE.push_status(
                        DeviceConnectionStatus::Error,
                        if message.is_empty() {
                            "Android USB write failed".to_owned()
                        } else {
                            message
                        },
                    );
                }
                Ok::<(), jni::errors::Error>(())
            })
            .into_outcome();
    }

    #[allow(non_snake_case)]
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_nativeOnUsbError<'local>(
        mut env: jni::EnvUnowned<'local>,
        _class: JClass<'local>,
        err: JString<'local>,
    ) {
        let _ = env
            .with_env(|env| {
                let message = jstring_to_string(env, &err);
                log::warn!("Android USB error: {message}");
                ANDROID_BRIDGE.opened.store(false, SeqCst);
                ANDROID_BRIDGE.push_status(
                    DeviceConnectionStatus::Error,
                    if message.is_empty() {
                        "Android USB bridge error".to_owned()
                    } else {
                        message
                    },
                );
                Ok::<(), jni::errors::Error>(())
            })
            .into_outcome();
    }

    pub type DefaultMobileBackend = AndroidUsbBackend;
}

#[cfg(target_os = "ios")]
mod ios {
    use std::io;

    use crate::niyien_device::{
        DeviceConnectionStatus,
        transport::{
            DeviceConnectionConfig, DevicePortCandidate, DeviceTransportBackend,
            DeviceTransportError, DeviceTransportStream,
        },
    };

    #[derive(Default)]
    pub struct UnsupportedMobileBackend;

    pub struct UnsupportedMobileStream;

    impl DeviceTransportStream for UnsupportedMobileStream {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn write_all(&mut self, _buf: &[u8]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "mobile device bridge is not connected",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl DeviceTransportBackend for UnsupportedMobileBackend {
        type Stream = UnsupportedMobileStream;

        fn list_ports(&mut self) -> Result<Vec<DevicePortCandidate>, DeviceTransportError> {
            Ok(Vec::new())
        }

        fn open(
            &mut self,
            _port_name: &str,
            _config: &DeviceConnectionConfig,
        ) -> Result<Self::Stream, DeviceTransportError> {
            Err(DeviceTransportError::Unsupported(
                "mobile device bridge is not connected",
            ))
        }
    }

    pub fn startup_connection_event() -> Option<(DeviceConnectionStatus, String)> {
        Some((
            DeviceConnectionStatus::Unsupported,
            "iOS cannot access the NiYien A1 real-time device channel on this build".to_owned(),
        ))
    }

    pub type DefaultMobileBackend = UnsupportedMobileBackend;
}

#[cfg(target_os = "android")]
pub use android::{DefaultMobileBackend, startup_connection_event};
#[cfg(target_os = "ios")]
pub use ios::{DefaultMobileBackend, startup_connection_event};
