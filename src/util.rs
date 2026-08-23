// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use cpp::*;
use qmetaobject::*;
use std::ffi::c_void;

pub fn serde_json_to_qt_array(v: &serde_json::Value) -> QJsonArray {
    let mut ret = QJsonArray::default();
    if let Some(arr) = v.as_array() {
        for param in arr {
            match param {
                serde_json::Value::Number(v) => {
                    ret.push(QJsonValue::from(v.as_f64().unwrap()));
                }
                serde_json::Value::Bool(v) => {
                    ret.push(QJsonValue::from(*v));
                }
                serde_json::Value::String(v) => {
                    ret.push(QJsonValue::from(QString::from(v.clone())));
                }
                serde_json::Value::Array(v) => {
                    ret.push(QJsonValue::from(serde_json_to_qt_array(
                        &serde_json::Value::Array(v.to_vec()),
                    )));
                }
                serde_json::Value::Object(_) => {
                    ret.push(QJsonValue::from(serde_json_to_qt_object(param)));
                }
                serde_json::Value::Null => { /* ::log::warn!("null unimplemented");*/ }
            };
        }
    }
    ret
}
pub fn serde_json_to_qt_object(v: &serde_json::Value) -> QJsonObject {
    let mut map = QJsonObject::default();
    if let Some(obj) = v.as_object() {
        for (k, v) in obj {
            match v {
                serde_json::Value::Number(v) => {
                    map.insert(k, QJsonValue::from(v.as_f64().unwrap()));
                }
                serde_json::Value::Bool(v) => {
                    map.insert(k, QJsonValue::from(*v));
                }
                serde_json::Value::String(v) => {
                    map.insert(k, QJsonValue::from(QString::from(v.clone())));
                }
                serde_json::Value::Array(v) => {
                    map.insert(
                        k,
                        QJsonValue::from(serde_json_to_qt_array(&serde_json::Value::Array(
                            v.to_vec(),
                        ))),
                    );
                }
                serde_json::Value::Object(_) => {
                    map.insert(k, QJsonValue::from(serde_json_to_qt_object(v)));
                }
                serde_json::Value::Null => { /* ::log::warn!("null unimplemented");*/ }
            };
        }
    }
    map
}

pub fn is_opengl() -> bool {
    cpp!(unsafe [] -> bool as "bool" {
        return QQuickWindow::graphicsApi() == QSGRendererInterface::OpenGLRhi;
    })
}

pub fn qt_queued_callback<T: QObject + 'static, T2: Send + 'static, F: FnMut(&T, T2) + 'static>(
    qptr: QPointer<T>,
    mut cb: F,
) -> impl Fn(T2) + Send + Sync + Clone + 'static {
    qmetaobject::queued_callback(move |arg| {
        if let Some(this) = qptr.as_pinned() {
            let this = this.borrow();
            cb(this, arg);
        }
    })
}
pub fn qt_queued_callback_mut<
    T: QObject + 'static,
    T2: Send + 'static,
    F: FnMut(&mut T, T2) + 'static,
>(
    qptr: QPointer<T>,
    mut cb: F,
) -> impl Fn(T2) + Send + Sync + Clone + 'static {
    qmetaobject::queued_callback(move |arg| {
        if let Some(this) = qptr.as_pinned() {
            let mut this = this.borrow_mut();
            cb(&mut this, arg);
        }
    })
}

#[macro_export]
macro_rules! wrap_simple_method {
    ($name:ident, $($param:ident:$type:ty),*) => {
        fn $name(&self, $($param:$type,)*) {
            self.stabilizer.$name($($param,)*);
        }
    };
    ($name:ident, $($param:ident:$type:ty),*; recompute) => {
        fn $name(&self, $($param:$type,)*) {
            self.stabilizer.$name($($param,)*);
            self.request_recompute();
        }
    };
    ($name:ident, $($param:ident:$type:ty),*; recompute$(; $extra_call:ident)*) => {
        fn $name(&mut self, $($param:$type,)*) {
            self.stabilizer.$name($($param,)*);
            self.request_recompute();
            $( self.$extra_call(); )*
        }
    };
}

cpp! {{
    #ifdef Q_OS_ANDROID
    #   include <QJniObject>
    #endif
    #include <QDesktopServices>
    #include <QLocale>
    #include <QStandardPaths>
    #include <QBuffer>
    #include <QImage>
    #include <QGuiApplication>
    #include <QObject>
    #include <QClipboard>
    #include <QEvent>
    #if (__APPLE__ + 0) || (__linux__ + 0)
    #   include <sys/resource.h>
    #endif

    static QObject *globalUrlCatcherPtr = nullptr;
    static QString pendingUrl;
    static QStringList pendingUrls;

    class QtEventFilter : public QObject {
    public:
        QtEventFilter(std::function<void(QUrl)> cb) : m_cb(cb) { }
        bool eventFilter(QObject *obj, QEvent *event) override {
            if (event->type() == QEvent::FileOpen) {
                m_cb(static_cast<QFileOpenEvent *>(event)->url());
            }
            return QObject::eventFilter(obj, event);
        }
        std::function<void(QUrl)> m_cb;
    };
}}
#[cfg(target_os = "android")]
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_urlReceived(
    _vm: *mut c_void,
    _: *mut c_void,
    jstr: *mut c_void,
) {
    cpp!(unsafe [jstr as "void*"] {
        #ifdef Q_OS_ANDROID
            QString str = QJniObject((jstring)jstr).toString();
            if (globalUrlCatcherPtr) {
                QMetaObject::invokeMethod(globalUrlCatcherPtr, "catch_url_open", Qt::QueuedConnection, Q_ARG(QUrl, QUrl(str)));
            } else {
                pendingUrl = str;
            }
        #else
            (void)jstr;
        #endif
    });
}
#[cfg(target_os = "android")]
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_urlsReceived(
    _vm: *mut c_void,
    _: *mut c_void,
    jstr: *mut c_void,
) {
    // Multi-URI dispatch from MainActivity.onActivityResult. The Java side joins
    // all picker URIs with '\n'; here we split and forward as a QStringList so QML
    // can route the whole batch (e.g. into the render queue) instead of dropping
    // 2..N like the legacy single-URL path did.
    cpp!(unsafe [jstr as "void*"] {
        #ifdef Q_OS_ANDROID
            QString joined = QJniObject((jstring)jstr).toString();
            QStringList urls = joined.split('\n', Qt::SkipEmptyParts);
            if (urls.isEmpty()) return;
            if (globalUrlCatcherPtr) {
                QMetaObject::invokeMethod(globalUrlCatcherPtr, "catch_urls_open", Qt::QueuedConnection, Q_ARG(QStringList, urls));
            } else {
                pendingUrls = urls;
            }
        #else
            (void)jstr;
        #endif
    });
}
#[cfg(target_os = "android")]
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_niyien_gyroflow_MainActivity_pickerCancelled(
    _vm: *mut c_void,
    _: *mut c_void,
) {
    // A picker we launched ourselves closed without a selection. Only Qt-owned
    // dialogs emit onRejected, so QML needs this to drop its pending callback.
    cpp!(unsafe [] {
        #ifdef Q_OS_ANDROID
            if (globalUrlCatcherPtr) {
                QMetaObject::invokeMethod(globalUrlCatcherPtr, "catch_picker_cancelled", Qt::QueuedConnection);
            }
        #endif
    });
}
pub fn set_url_catcher(ctlptr: *mut c_void) {
    cpp!(unsafe [ctlptr as "QObject *"] {
        globalUrlCatcherPtr = ctlptr;
        if (!pendingUrl.isEmpty()) {
            QMetaObject::invokeMethod(globalUrlCatcherPtr, "catch_url_open", Qt::QueuedConnection, Q_ARG(QUrl, QUrl(pendingUrl)));
            pendingUrl.clear();
        }
        if (!pendingUrls.isEmpty()) {
            QMetaObject::invokeMethod(globalUrlCatcherPtr, "catch_urls_open", Qt::QueuedConnection, Q_ARG(QStringList, pendingUrls));
            pendingUrls.clear();
        }
    });
}
pub fn register_url_handlers() {
    cpp!(unsafe [] {
        #if defined(Q_OS_ANDROID) || defined(Q_OS_IOS)
            QDesktopServices::setUrlHandler("content", globalUrlCatcherPtr, "catch_url_open");
            QDesktopServices::setUrlHandler("file",    globalUrlCatcherPtr, "catch_url_open");
        #endif
    });
}
pub fn unregister_url_handlers() {
    cpp!(unsafe [] {
        #if defined(Q_OS_ANDROID) || defined(Q_OS_IOS)
            QDesktopServices::unsetUrlHandler("content");
            QDesktopServices::unsetUrlHandler("file");
        #endif
    });
}
pub fn dispatch_url_event(url: QUrl) {
    cpp!(unsafe [url as "QUrl"] {
        QFileOpenEvent evt(url);
        qGuiApp->sendEvent(qGuiApp, &evt);
    });
}
pub fn qurl_to_encoded(url: QUrl) -> String {
    cpp!(unsafe [url as "QUrl"] -> QString as "QString" {
        return QString(url.toEncoded());
    })
    .to_string()
}

/// Normalize a path string for logging: strip `file://` scheme, percent-decode,
/// and on Windows convert forward slashes to backslashes for visual parity with
/// the OS. Used in two places: (a) at controller entry points where QML pushes
/// `QUrl` strings, (b) inside `LogContext::enter` when storing `video_path`.
pub fn normalize_path_for_log(s: &str) -> String {
    let trimmed = s.strip_prefix("file:///")
        .or_else(|| s.strip_prefix("file://"))
        .unwrap_or(s);
    // percent-decode; lossy fallback on malformed sequences
    let decoded = percent_encoding::percent_decode_str(trimmed)
        .decode_utf8_lossy()
        .into_owned();
    if cfg!(target_os = "windows") {
        decoded.replace('/', "\\")
    } else {
        decoded
    }
}

#[cfg(test)]
mod normalize_path_tests {
    use super::normalize_path_for_log;

    #[test]
    fn windows_qurl_with_chinese() {
        let input = "file:///D:/%E4%B8%8B%E8%BD%BD/clip.mp4";
        let out = normalize_path_for_log(input);
        if cfg!(target_os = "windows") {
            assert_eq!(out, "D:\\下载\\clip.mp4");
        } else {
            assert_eq!(out, "D:/下载/clip.mp4");
        }
    }

    #[test]
    fn linux_passthrough() {
        let input = "/home/user/video clip.mp4";
        assert_eq!(normalize_path_for_log(input), if cfg!(target_os = "windows") {
            "\\home\\user\\video clip.mp4"
        } else {
            "/home/user/video clip.mp4"
        });
    }

    #[test]
    fn empty_string() {
        assert_eq!(normalize_path_for_log(""), "");
    }

    #[test]
    fn malformed_percent_sequence() {
        // %ZZ is not valid hex; percent_decode_str preserves the original bytes,
        // and the result remains UTF-8 valid.
        let out = normalize_path_for_log("/tmp/foo%ZZ.mp4");
        if cfg!(target_os = "windows") {
            assert_eq!(out, "\\tmp\\foo%ZZ.mp4");
        } else {
            assert_eq!(out, "/tmp/foo%ZZ.mp4");
        }
    }

    #[test]
    fn double_slash_only_strips_file_prefix() {
        let out = normalize_path_for_log("file://server/share/file.mp4");
        if cfg!(target_os = "windows") {
            assert_eq!(out, "server\\share\\file.mp4");
        } else {
            assert_eq!(out, "server/share/file.mp4");
        }
    }
}
pub fn catch_qt_file_open<F: FnMut(QUrl)>(cb: F) {
    let func: Box<dyn FnMut(QUrl)> = Box::new(cb);
    let cb_ptr = Box::into_raw(func);
    cpp!(unsafe [cb_ptr as "TraitObject2"] {
        qGuiApp->installEventFilter(new QtEventFilter([cb_ptr](QUrl url) {
            rust!(Rust_catch_qt_file_open [cb_ptr: *mut dyn FnMut(QUrl) as "TraitObject2", url: QUrl as "QUrl"] {
                let mut cb = unsafe { Box::from_raw(cb_ptr) };
                cb(url.clone());
                let _ = Box::into_raw(cb); // leak again so it doesn't get deleted here
            });
        }));
    });
}

pub fn open_file_externally(url: QUrl) {
    unregister_url_handlers();
    cpp!(unsafe [url as "QUrl"] { QDesktopServices::openUrl(url); });
    register_url_handlers();
}

pub fn get_data_location() -> String {
    cpp!(unsafe [] -> QString as "QString" {
        return QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    })
    .into()
}

/// System locale name in Qt's form (`zh_CN`, `en_US`, `ru_RU`, ...).
///
/// This is deliberately NOT the selected UI translation (`settings["lang"]`),
/// which holds a translation *file name* constrained to the shipped `.qm` set.
/// NiYien Tool reports `QLocale::system().name()`, so matching it keeps the two
/// products' language breakdowns comparable — mixing the two would produce
/// values that look alike while describing a different population.
pub fn system_locale_name() -> String {
    cpp!(unsafe [] -> QString as "QString" {
        return QLocale::system().name();
    })
    .into()
}

pub fn update_rlimit() {
    cpp!(unsafe [] {
        #if (__APPLE__ + 0) || (__linux__ + 0)
            // Increase open file limit, because it gets hit pretty quickly with R3D or BRAW in render queue
            struct rlimit limit;
            if (::getrlimit(RLIMIT_NOFILE, &limit) == 0) {
                if (limit.rlim_cur < 4096) {
                    limit.rlim_cur = 4096;
                    if (limit.rlim_max < 4096)
                        limit.rlim_max = 4096;
                    if (::setrlimit(RLIMIT_NOFILE, &limit) != 0) {
                        qDebug() << "Failed to set RLIMIT_NOFILE to 4096!";
                    }
                }
            }
        #endif
    });
}

pub fn set_android_context() {
    #[cfg(target_os = "android")]
    {
        let jvm = cpp!(unsafe [] -> *mut c_void as "void *" {
            #ifdef Q_OS_ANDROID
                return QJniEnvironment::javaVM();
            #else
                return nullptr;
            #endif
        });
        let activity = cpp!(unsafe [] -> *mut c_void as "void *" {
            #ifdef Q_OS_ANDROID
                auto ctx = QNativeInterface::QAndroidApplication::context();
                return QJniEnvironment::getJniEnv()->NewGlobalRef(ctx.object());
            #else
                return nullptr;
            #endif
        });
        unsafe {
            ndk_context::initialize_android_context(jvm, activity);
        }
    }
}

pub fn init_logging() {
    // Phase 1: delegated to crate::logger which writes to data_dir/logs/.
    // Android still goes through simplelog::WriteLogger to AndroidLog (no
    // file system / stderr equivalent), keeping legacy behavior on mobile.
    #[cfg(target_os = "android")]
    {
        use simplelog::*;
        let log_config = ["mp4parse", "wgpu", "naga", "akaze", "ureq", "rustls", "mdk"]
            .into_iter()
            .fold(ConfigBuilder::new(), |mut cfg, x| {
                cfg.add_filter_ignore_str(x);
                cfg
            })
            .build();
        WriteLogger::init(
            LevelFilter::Debug,
            log_config,
            crate::util::AndroidLog::default(),
        )
        .unwrap();
        // Even on Android we want a session id in LogContext for any future
        // panic / feedback wiring. Generated inline since the file-based
        // logger module is not used here.
        let sid = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        crate::log_context::LogContext::set_session_id(sid);
    }

    #[cfg(not(target_os = "android"))]
    {
        crate::logger::init();
    }

    qmetaobject::log::init_qt_to_rust();

    // Suppress MDK SDK internal logs. App-level media errors are logged at
    // the call sites that handle them.
    qml_video_rs::video_item::MDKVideoItem::setLogHandler(|_: i32, _: &str| {});
}

/// Invalidate Qt RHI pipeline cache and QML bytecode cache when the host application's
/// version (Cargo.toml `version`) changes. Qt's auto-managed ABI tag pins to CPU /
/// endianness / data-model only — it does not bind to gyroflow's build, shader source,
/// or Controller QMetaObject layout. A cache from a previous build can corrupt heap
/// during deserialization and crash V4 in unrelated places (e.g. AV in
/// QV4::warnAboutCoercionToVoid). Wipe the caches whenever the build changes (binary mtime).
///
/// Must run before Qt's first RHI-aware QQuickWindow is shown (which is when the
/// cache files are read). Currently invoked right after init_logging — comfortably
/// ahead of QmlEngine::new() and main_window.qml load.
pub fn invalidate_qt_cache_if_version_changed() {
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    // Build identity = version + this binary's mtime. Every fresh build — incl.
    // dev rebuilds where VERSION stays the same (e.g. 1.6.3 across many `just
    // run`s) — relinks the exe and bumps its mtime, yielding a new stamp that
    // wipes the stale Qt RHI PSO / QML-bytecode blobs which otherwise crash the
    // renderer on the next video load. A shipped binary the user never
    // recompiles keeps a stable mtime, so its caches survive as before. Falls
    // back to version-only when the exe mtime can't be read.
    let build_id = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| format!("{VERSION}-{}", d.as_secs()))
        .unwrap_or_else(|| VERSION.to_string());
    let cache_dir = gyroflow_core::settings::data_dir().join("cache");
    let stamp_file = cache_dir.join(".gyroflow-qt-cache-stamp");
    let prev = std::fs::read_to_string(&stamp_file)
        .ok()
        .unwrap_or_default();
    if prev.trim() == build_id {
        return;
    }
    let mut wiped: Vec<String> = Vec::new();
    let mut had_failures = false;
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Match all Qt RHI pipeline cache variants (Windows -llp64, *nix -lp64,
            // arm64 / future Qt versions ...), the older qtshadercache name, plus
            // the QML bytecode cache. All can hold meta-id offsets / PSO blobs that
            // go stale across builds.
            let is_target = name_str.starts_with("qtpipelinecache-")
                || name_str.starts_with("qtshadercache")
                || name_str == "qmlcache";
            if !is_target {
                continue;
            }
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => wiped.push(name_str.into_owned()),
                Err(e) => {
                    had_failures = true;
                    ::log::warn!(
                        "Failed to wipe stale Qt cache entry {}: {e}",
                        entry.path().display()
                    );
                }
            }
        }
    }
    let _ = std::fs::create_dir_all(&cache_dir);
    // Update the stamp even if some removals failed: blocking the stamp write would
    // leave us retrying every launch with no progress. The warn above gives the user
    // a signal that something needs manual cleanup (e.g. another Gyroflow instance
    // holding the files open).
    let _ = std::fs::write(&stamp_file, &build_id);
    if had_failures {
        ::log::warn!(
            "Qt cache partially invalidated (stamp {prev:?} -> {build_id:?}); wiped {wiped:?} under {} — some entries could not be removed, see warnings above",
            cache_dir.display()
        );
    } else {
        ::log::info!(
            "Qt cache invalidated (stamp {prev:?} -> {build_id:?}); wiped {wiped:?} under {}",
            cache_dir.display()
        );
    }
}

pub fn install_crash_handler() -> std::io::Result<()> {
    // Breakpad and `crate::crash` are desktop-only (`pub mod crash` is cfg-gated
    // out on android/ios in gyroflow.rs), so this handler is a no-op on mobile.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let cur_dir = std::env::current_dir()?;

        let os_str = cur_dir.as_os_str();
        let path: Vec<breakpad_sys::PathChar> = {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                os_str.encode_wide().collect()
            }
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                Vec::from(os_str.as_bytes())
            }
        };

        unsafe {
            extern "C" fn callback(
                path: *const breakpad_sys::PathChar,
                path_len: usize,
                _ctx: *mut c_void,
            ) {
                let path_slice = unsafe { std::slice::from_raw_parts(path, path_len) };

                let path = {
                    #[cfg(windows)]
                    {
                        use std::os::windows::ffi::OsStringExt;
                        std::path::PathBuf::from(std::ffi::OsString::from_wide(path_slice))
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::ffi::OsStrExt;
                        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(path_slice).to_owned())
                    }
                };

                println!("Crashdump written to {}", path.display());
            }

            breakpad_sys::attach_exception_handler(
                path.as_ptr(),
                path.len(),
                callback,
                std::ptr::null_mut(),
                breakpad_sys::INSTALL_BOTH_HANDLERS,
            );
        }

        // Crash dump handling (niyien fork). Upstream POSTed every local *.dmp to
        // api.gyroflow.xyz and deleted it on success — leaking crash data upstream
        // and wiping the dumps before they could be inspected. Instead, package
        // each breakpad OS dump into a crash zip under logs/crashes/ so the fork's
        // OWN feedback pickup uploads it to our 123/R2 (next to the Rust-panic
        // crash zips, same FeedbackDialog flow). Never touches api.gyroflow.xyz.
        // The original .dmp is removed only once packaging succeeds; on failure it
        // is left in place so nothing is lost.
        crate::core::run_threaded(move || {
            if let Ok(files) = std::fs::read_dir(cur_dir) {
                for path in files.flatten() {
                    let path = path.path();
                    if path.to_string_lossy().ends_with(".dmp") {
                        match crate::crash::package_os_dump(&path) {
                            Ok(zip) => {
                                ::log::info!(
                                    target: "lifecycle",
                                    "OS crash dump packaged for local feedback upload: {} -> {}",
                                    path.display(),
                                    zip.display()
                                );
                                let _ = std::fs::remove_file(&path);
                            }
                            Err(e) => {
                                ::log::warn!(
                                    target: "lifecycle",
                                    "Failed to package OS crash dump {} ({e}); left in place",
                                    path.display()
                                );
                            }
                        }
                    }
                }
            }
        });
    }
    Ok(())
}

// Launches the SAF picker through MainActivity.openPicker instead of Qt's
// FileDialog, so the intent can be aimed at DocumentsUI explicitly (see the Java
// side for why). Results keep arriving through the existing urlsReceived bridge,
// so nothing downstream changes. mode: 0 = files, 1 = folder tree.
#[cfg(target_os = "android")]
pub fn android_open_picker(mode: i32, allow_multiple: bool, initial_uri: &str) -> Result<(), String> {
    use jni::objects::{JClass, JObject, JString, JValue};
    let jvm = unsafe { jni::JavaVM::from_raw(ndk_context::android_context().vm().cast()) };
    let status = jvm
        .attach_current_thread(|env| {
            let activity = unsafe {
                JObject::from_raw(env, ndk_context::android_context().context().cast())
            };
            let activity_class = env.get_object_class(&activity)?;
            let class_loader = activity_class.get_class_loader(env)?;
            let class_name = env.new_string("com.niyien.gyroflow.MainActivity")?;
            let class = JClass::for_name_with_loader(env, class_name, true, class_loader)?;
            let jinitial = env.new_string(initial_uri)?;
            let result = env
                .call_static_method(
                    class,
                    jni::jni_str!("openPicker"),
                    jni::jni_sig!("(IZLjava/lang/String;)Ljava/lang/String;"),
                    &[
                        JValue::Int(mode),
                        JValue::Bool(allow_multiple),
                        JValue::Object(jinitial.as_ref()),
                    ],
                )?
                .l()?;
            Ok::<String, jni::errors::Error>(env.as_cast::<JString>(&result)?.to_string())
        })
        .map_err(|err| format!("openPicker JNI call failed: {err}"))?;
    if status == "ok" {
        Ok(())
    } else {
        Err(match status.strip_prefix("error:") {
            Some(detail) => detail.to_owned(),
            None => format!("unexpected openPicker status: {status}"),
        })
    }
}

// In-app update handoff: forward the downloaded APK to MainActivity.installApk,
// which dispatches the system package installer (or the unknown-sources grant
// page). Uses the activity's classloader to resolve the app class — a plain
// FindClass on a Rust worker thread only sees system classes.
#[cfg(target_os = "android")]
pub fn android_install_apk(path: &str) -> Result<(), String> {
    use jni::objects::{JClass, JObject, JString, JValue};
    let jvm = unsafe { jni::JavaVM::from_raw(ndk_context::android_context().vm().cast()) };
    let status = jvm
        .attach_current_thread(|env| {
            let activity = unsafe {
                JObject::from_raw(env, ndk_context::android_context().context().cast())
            };
            let activity_class = env.get_object_class(&activity)?;
            let class_loader = activity_class.get_class_loader(env)?;
            let class_name = env.new_string("com.niyien.gyroflow.MainActivity")?;
            let class = JClass::for_name_with_loader(env, class_name, true, class_loader)?;
            let jpath = env.new_string(path)?;
            let result = env
                .call_static_method(
                    class,
                    jni::jni_str!("installApk"),
                    jni::jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                    &[JValue::Object(jpath.as_ref())],
                )?
                .l()?;
            Ok::<String, jni::errors::Error>(env.as_cast::<JString>(&result)?.to_string())
        })
        .map_err(|err| format!("installApk JNI call failed: {err}"))?;
    match status.as_str() {
        "ok" => {
            ::log::info!(target: "update", "android install handoff dispatched");
            Ok(())
        }
        "needs-permission" => Err(crate::distribution::INSTALL_PERMISSION_REQUIRED_ERROR.to_owned()),
        other => Err(match other.strip_prefix("error:") {
            Some(detail) => detail.to_owned(),
            None => format!("unexpected installApk status: {other}"),
        }),
    }
}

#[cfg(target_os = "android")]
pub fn android_log(v: String) {
    use std::ffi::{CStr, CString};
    let tag = CStr::from_bytes_with_nul(b"Gyroflow\0").unwrap();
    if let Ok(msg) = CString::new(v) {
        unsafe {
            ndk_sys::__android_log_write(
                ndk_sys::android_LogPriority::ANDROID_LOG_DEBUG.0 as std::os::raw::c_int,
                tag.as_ptr(),
                msg.as_ptr(),
            );
        }
    }
}

#[cfg(target_os = "android")]
#[derive(Default)]
pub struct AndroidLog {
    buf: String,
}
#[cfg(target_os = "android")]
impl std::io::Write for AndroidLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(s) = String::from_utf8(buf.to_vec()) {
            self.buf.push_str(&s);
        };
        if self.buf.contains('\n') {
            self.flush()?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        android_log(self.buf.clone());
        self.buf.clear();
        Ok(())
    }
}

pub fn tr(context: &str, text: &str) -> String {
    let context = QString::from(context);
    let text = QString::from(text);
    cpp!(unsafe [context as "QString", text as "QString"] -> QString as "QString" {
        return QCoreApplication::translate(qUtf8Printable(context), qUtf8Printable(text));
    })
    .to_string()
}

pub fn qt_graphics_api() -> QString {
    cpp!(unsafe [] -> QString as "QString" {
        switch (QQuickWindow::graphicsApi()) {
            case QSGRendererInterface::OpenGL:     return "opengl";
            case QSGRendererInterface::Direct3D11: return "directx";
            case QSGRendererInterface::Vulkan:     return "vulkan";
            case QSGRendererInterface::Metal:      return "metal";
            default: return "unknown";
        }
    })
}

pub fn get_version() -> String {
    env!("NIYIEN_VERSION_DISPLAY").to_string()
}

pub fn get_canonical_version() -> &'static str {
    env!("NIYIEN_VERSION_CANONICAL")
}
pub fn copy_to_clipboard(text: QString) {
    cpp!(unsafe [text as "QString"] { QGuiApplication::clipboard()->setText(text); })
}

pub fn save_exe_location() {
    if let Ok(exe_path) = std::env::current_exe() {
        if cfg!(target_os = "macos") {
            if let Some(parent) = exe_path.parent() {
                // MacOS
                if let Some(parent) = parent.parent() {
                    // Contents
                    if let Some(parent) = parent.parent() {
                        // App bundle
                        gyroflow_core::settings::set(
                            "exeLocation",
                            parent.to_string_lossy().into(),
                        );
                    }
                }
            }
        } else {
            #[allow(unused_mut)]
            let mut exe_str = exe_path.to_string_lossy().to_string();

            #[cfg(target_os = "windows")]
            if exe_str.contains("29160AdrianRoss.Gyroflow") {
                let parts = exe_str.split("\\").collect::<Vec<_>>();
                let parts = parts
                    .into_iter()
                    .rev()
                    .skip(1)
                    .next()
                    .unwrap_or("")
                    .split("_")
                    .collect::<Vec<_>>();
                if let Some(publisher) = parts.first() {
                    if let Some(app_id) = parts.last() {
                        if !publisher.is_empty() && !app_id.is_empty() {
                            exe_str = format!("shell:AppsFolder\\{publisher}_{app_id}!Gyroflow");
                        }
                    }
                }
            }
            #[cfg(target_os = "linux")]
            if exe_str.contains("/tmp/.mount") {
                if let Ok(appimg) = std::env::var("APPIMAGE") {
                    if !appimg.is_empty() {
                        exe_str = appimg;
                    }
                }
            }

            gyroflow_core::settings::set("exeLocation", exe_str.into());
        }
    }
}

pub fn image_data_to_base64(w: u32, h: u32, s: u32, data: &[u8]) -> QString {
    let ptr = data.as_ptr();
    cpp!(unsafe [w as "uint32_t", h as "uint32_t", s as "uint32_t", ptr as "const uint8_t *"] -> QString as "QString" {
        QImage img(ptr, w, h, s, QImage::Format_RGBA8888_Premultiplied);
        QByteArray byteArray;
        QBuffer buffer(&byteArray);
        buffer.open(QIODevice::WriteOnly);
        img.save(&buffer, "JPEG", 50);
        QString b64("data:image/jpg;base64,");
        b64.append(QString::fromLatin1(byteArray.toBase64().data()));
        return b64;
    })
}

pub fn image_to_b64(img: QImage) -> QString {
    cpp!(unsafe [img as "QImage"] -> QString as "QString" {
        QByteArray byteArray;
        QBuffer buffer(&byteArray);
        buffer.open(QIODevice::WriteOnly);
        img.save(&buffer, "JPEG", 50);
        QString b64("data:image/jpg;base64,");
        b64.append(QString::fromLatin1(byteArray.toBase64().data()));
        return b64;
    })
}

pub fn update_file_times(output_url: &str, input_url: &str, additional_ms: Option<f64>) {
    if let Err(e) = || -> std::io::Result<()> {
        let input_path = gyroflow_core::filesystem::url_to_path(input_url);
        let output_path = gyroflow_core::filesystem::url_to_path(output_url);
        if input_path.is_empty() || output_path.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "Can't get path from url! Input: {input_url} / {input_path}, output: {output_url} / {output_path}"
                ),
            ));
        }
        let mut org_time_c =
            filetime_creation::FileTime::from_creation_time(&std::fs::metadata(&input_path)?);
        let mut org_time_m = filetime_creation::FileTime::from_last_modification_time(
            &std::fs::metadata(&input_path)?,
        );
        if let Some(additional_ms) = additional_ms {
            if additional_ms > 0.0 {
                if let Some(ctime) = org_time_c {
                    org_time_c = Some(filetime_creation::FileTime::from_unix_time(
                        ctime.unix_seconds() + (additional_ms / 1000.0).round() as i64,
                        ctime.nanoseconds(),
                    ));
                }
                org_time_m = filetime_creation::FileTime::from_unix_time(
                    org_time_m.unix_seconds() + (additional_ms / 1000.0).round() as i64,
                    org_time_m.nanoseconds(),
                );
            }
        }
        if cfg!(target_os = "windows") {
            if let Some(org_time_c) = org_time_c {
                ::log::debug!(
                    "Updating creation time of {} to {}",
                    output_path,
                    org_time_c.to_string()
                );
                filetime_creation::set_file_ctime(output_path.clone(), org_time_c)?;
            }
        }
        ::log::debug!(
            "Updating modification time of {} to {}",
            output_path,
            org_time_m.to_string()
        );
        filetime_creation::set_file_mtime(output_path, org_time_m)?;

        Ok(())
    }() {
        ::log::warn!("Failed to update file times: {e:?}");
    }
}

pub fn is_store_package() -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        let mut len = 0;
        let _ = windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName(&mut len, None);
        if len > 0 {
            return true;
        }
    }

    // Only the app from App Store is sandboxed on MacOS
    if cfg!(target_os = "macos") && gyroflow_core::filesystem::is_sandboxed() {
        return true;
    }

    false
}

pub fn is_insta360(input_url: &str) -> bool {
    use std::io::*;
    let mut buf = vec![0u8; 32];
    if let Ok(mut input) = gyroflow_core::filesystem::open_file(input_url, false, false) {
        let _ = input.seek(SeekFrom::End(-32));
        let _ = input.read_exact(&mut buf);
    }
    &buf == b"8db42d694ccc418790edff439fe026bf"
}
pub fn copy_insta360_metadata(
    output_url: &str,
    input_url: &str,
) -> Result<(), gyroflow_core::filesystem::FilesystemError> {
    use std::io::*;
    pub const HEADER_SIZE: usize = 32 + 4 + 4 + 32; // padding(32), size(4), version(4), magic(32)
    pub const MAGIC: &[u8] = b"8db42d694ccc418790edff439fe026bf";

    let mut input = gyroflow_core::filesystem::open_file(input_url, false, false)?;

    let mut buf = vec![0u8; HEADER_SIZE];
    input.seek(SeekFrom::End(-(HEADER_SIZE as i64)))?;
    input.read_exact(&mut buf)?;
    if &buf[HEADER_SIZE - 32..] == MAGIC {
        let extra_size = u32::from_le_bytes(buf[32..36].try_into().unwrap()) as i64;
        input.seek(SeekFrom::End(-extra_size))?;

        let mut output = gyroflow_core::filesystem::open_file(output_url, true, false)?;
        output.seek(SeekFrom::End(0))?;
        std::io::copy(&mut input, &mut output.get_file())?;
    }

    Ok(())
}

// Probe the container rotation of an `.r3d` file via telemetry-parser's mp4parse
// path. Returns the detected rotation (0/90/180/270) or 0 on any failure.
//
// Nikon ZR ships `.r3d` files as MP4 containers with NR3D codec — the MDK R3D
// plugin does not parse `tkhd.matrix`, so we bypass via mp4parse to read the
// real rotation. Real RED2-container R3D files fail `parse_mp4` and we return 0,
// matching prior behavior.
// Locate the `%0Nd` image2-demuxer token in a decoded path. Returns
// `(start, end, width)`, where `start` indexes the `%` and `end` is one past
// the trailing `d`.
fn find_image_sequence_token(path: &str) -> Option<(usize, usize, usize)> {
    let bytes = path.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        // Match `%0<width>d` (image2 demuxer specifier).
        if bytes[i] == b'%' && bytes[i + 1] == b'0' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 && j < bytes.len() && bytes[j] == b'd' {
                if let Ok(width) = path[i + 2..j].parse::<usize>() {
                    if width > 0 {
                        return Some((i, j + 1, width));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn is_path_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

// Reconstruct the first-frame url of a `%0Nd` image sequence from its pattern
// url and start index. Telemetry (camera model, lens params, frame readout
// time) must be parsed from a real frame, not the `%0Nd` pattern that
// `open_file` cannot resolve. Returns None when the url carries no `%0Nd` token.
//
// Formatting only - it does not check that the file exists. Callers that need a
// url naming a file that is really there use
// `resolve_image_sequence_first_frame` instead.
pub fn image_sequence_first_frame_url(pattern_url: &str, start: i32) -> Option<String> {
    // Decode to an OS path first so the sequence token is the literal `%0Nd`
    // (url form may percent-encode the `%` as `%25`).
    let path = gyroflow_core::filesystem::url_to_path(pattern_url);
    let (i, j, width) = find_image_sequence_token(&path)?;
    let replaced = format!(
        "{}{:0width$}{}",
        &path[..i],
        start.max(0),
        &path[j..],
        width = width
    );
    Some(gyroflow_core::filesystem::path_to_url(&replaced))
}

// Start indices probed by `resolve_image_sequence_first_frame`, in order and
// deduplicated: the caller's hint first, then the two numbering conventions
// every writer in the wild uses. A negative hint is clamped, so it collapses
// onto the `0` candidate instead of formatting a `-1` frame that cannot exist.
fn first_frame_candidates(start_hint: i32) -> Vec<i32> {
    let mut candidates: Vec<i32> = Vec::with_capacity(3);
    for n in [start_hint.max(0), 0, 1] {
        if !candidates.contains(&n) {
            candidates.push(n);
        }
    }
    candidates
}

// Directory backstop for `resolve_image_sequence_first_frame`: list the folder
// holding the pattern and return the lowest-numbered file matching
// `<prefix><digits><suffix>`. The digit run is deliberately not required to be
// `width` wide, so a writer that overflowed its own padding still resolves.
//
// Local paths only: an Android SAF url decodes to a bare filename, which has no
// folder to scan, and a token sitting in a folder component is not a sequence
// this can enumerate. Both return None and leave the caller on the miss path.
fn scan_folder_for_first_frame(path: &str, token_start: usize, token_end: usize) -> Option<String> {
    let sep = path[..token_start].rfind(is_path_separator)?;
    let prefix = &path[sep + 1..token_start];
    let suffix = &path[token_end..];
    if suffix.contains(is_path_separator) {
        return None;
    }
    let folder = &path[..sep];

    let mut best: Option<(u64, String)> = None;
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.len() <= prefix.len() + suffix.len()
            || !name.starts_with(prefix)
            || !name.ends_with(suffix)
        {
            continue;
        }
        // `starts_with` / `ends_with` matched, so both offsets are char
        // boundaries even for non-ASCII filenames.
        let digits = &name[prefix.len()..name.len() - suffix.len()];
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(n) = digits.parse::<u64>()
            && best.as_ref().map(|(b, _)| n < *b).unwrap_or(true)
        {
            best = Some((n, name));
        }
    }

    let (_, name) = best?;
    Some(gyroflow_core::filesystem::path_to_url(&format!(
        "{}{}",
        &path[..=sep],
        name
    )))
}

// Resolve a `%0Nd` image sequence pattern url to a url naming a file that is
// really on disk. Shared by every consumer that has to read the sequence's
// first frame - the preview tone curve, the sync tone curve and
// `load_telemetry` - because none of them can open the pattern itself.
//
// `start_hint` is only a hint: on the queue-Play / .gyroflow-project paths the
// controller still holds the previous clip's start index, so `0` and `1` are
// probed as well, and a directory scan backstops sequences numbered from
// anything else.
//
// Returns None both when the url carries no `%0Nd` token - the non-sequence
// identity path, a control-flow short circuit rather than a value coincidence -
// and when nothing could be resolved. Callers use the url unchanged in either
// case, so a miss reproduces exactly the pre-resolve failure path.
pub fn resolve_image_sequence_first_frame(pattern_url: &str, start_hint: i32) -> Option<String> {
    let path = gyroflow_core::filesystem::url_to_path(pattern_url);
    let (i, j, width) = find_image_sequence_token(&path)?;

    let candidates = first_frame_candidates(start_hint);

    for n in &candidates {
        let replaced = format!("{}{:0width$}{}", &path[..i], n, &path[j..], width = width);
        let url = gyroflow_core::filesystem::path_to_url(&replaced);
        if gyroflow_core::filesystem::exists(&url) {
            ::log::debug!(
                target: "video.load",
                "image sequence first frame resolved via candidate:{} -> {}",
                n,
                gyroflow_core::filesystem::get_filename(&url),
            );
            return Some(url);
        }
    }

    if let Some(url) = scan_folder_for_first_frame(&path, i, j) {
        ::log::debug!(
            target: "video.load",
            "image sequence first frame resolved via dir_scan -> {}",
            gyroflow_core::filesystem::get_filename(&url),
        );
        return Some(url);
    }

    // The direct precursor of "the tone curve should have activated and did
    // not" / "telemetry was parsed from the pattern" - keep it visible.
    ::log::warn!(
        target: "video.load",
        "image sequence first frame unresolved for {}: candidates {:?} missing, dir_scan found no match",
        gyroflow_core::filesystem::get_filename(pattern_url),
        candidates,
    );
    None
}

// Derive a creation timestamp for a clip whose container carries none, from the
// date in its filename plus its SMPTE timecode.
//
// ⚠ THIS IS A DELIBERATE GUESS, kept because the alternative (no timestamp at
// all) leaves the video-information panel blank. Two ways it can be wrong, both
// accepted by the user on 2026-08-23:
//   - The date comes from a NAMING CONVENTION, so renaming a clip changes it.
//   - The timecode is the camera's LOCAL time of day and CinemaDNG records no
//     timezone, while every consumer of `video_created_at` works in UTC. On the
//     reference material the camera read 19:03:44 local while the paired gyro
//     logger reported +03:00, i.e. the derived value is three hours ahead of the
//     gyro's UTC. Wall-clock matching and batch-clock learning consume this, so
//     a wrong value is confidently wrong.
// Every derivation is logged at info so a suspicious offset can be traced back
// here in one grep.
//
// `timecode` is REQUIRED, not optional, and that is the whole scoping mechanism:
// only a container that states a timecode and no date - CinemaDNG today - can
// reach this. Making it a parameter rather than a check at each call site means
// a new caller cannot widen the guess to ordinary videos by forgetting a
// condition (an MOV named `A001_002_20240615.MOV` must keep having no creation
// date rather than acquiring a fabricated one).
//
// Returns "YYYY:MM:DD HH:MM:SS" (the format `parse_creation_date_to_millis`
// expects), or None when the name holds no date or the timecode does not parse.
pub fn derive_creation_date_from_filename(filename: &str, timecode: &str) -> Option<String> {
    let (y, mo, d, _) = find_date_in_filename(filename)?;
    let (h, mi, s) = parse_timecode_hms(timecode)?;
    let out = format!("{y:04}:{mo:02}:{d:02} {h:02}:{mi:02}:{s:02}");
    ::log::info!(
        target: "video.load",
        "derived creation date {out} for {filename} (date from filename, time from timecode {timecode}) - GUESSED, no timezone in the container",
    );
    Some(out)
}

fn parse_timecode_hms(tc: &str) -> Option<(u32, u32, u32)> {
    let mut it = tc.split(':');
    let h: u32 = it.next()?.parse().ok()?;
    let mi: u32 = it.next()?.parse().ok()?;
    let s: u32 = it.next()?.parse().ok()?;
    (h < 24 && mi < 60 && s < 60).then_some((h, mi, s))
}

fn plausible_date(y: u32, mo: u32, d: u32) -> bool {
    (1990..=2100).contains(&y) && (1..=12).contains(&mo) && (1..=31).contains(&d)
}

// `YYYY-MM-DD`, `YYYY_MM_DD`, `YYYY.MM.DD` and bare `YYYYMMDD`, in that order.
// The separated forms are tried first: a bare 8-digit run is the most likely to
// collide with a counter, so it only gets a look once the explicit forms miss.
// Returns the date and the byte index just past it, so the time search can skip
// the digits the date already consumed.
fn find_date_in_filename(filename: &str) -> Option<(u32, u32, u32, usize)> {
    let b = filename.as_bytes();
    let digits_at = |i: usize, n: usize| -> Option<u32> {
        if i + n > b.len() || !b[i..i + n].iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        filename[i..i + n].parse().ok()
    };
    let is_sep = |i: usize| i < b.len() && matches!(b[i], b'-' | b'_' | b'.');

    for i in 0..b.len() {
        // Do not start mid-number, so `C0002_20260816` cannot match at an offset
        // that slices a longer digit run.
        if i > 0 && b[i - 1].is_ascii_digit() {
            continue;
        }
        if let (Some(y), true, Some(mo), true, Some(d)) = (
            digits_at(i, 4),
            is_sep(i + 4),
            digits_at(i + 5, 2),
            is_sep(i + 7),
            digits_at(i + 8, 2),
        ) {
            if plausible_date(y, mo, d) && digits_at(i + 10, 1).is_none() {
                return Some((y, mo, d, i + 10));
            }
        }
    }
    for i in 0..b.len() {
        if i > 0 && b[i - 1].is_ascii_digit() {
            continue;
        }
        // Exactly 8 digits: a longer run is a counter or a serial, not a date.
        if digits_at(i, 8).is_some() && digits_at(i + 8, 1).is_none() {
            let (y, mo, d) = (
                digits_at(i, 4)?,
                digits_at(i + 4, 2)?,
                digits_at(i + 6, 2)?,
            );
            if plausible_date(y, mo, d) {
                return Some((y, mo, d, i + 8));
            }
        }
    }
    None
}

#[cfg(test)]
mod derive_creation_date_tests {
    use super::derive_creation_date_from_filename as derive;

    #[test]
    fn bmd_name_plus_timecode_is_second_accurate() {
        // The reference clip: name carries 2026-08-16 and 1903, the DNG's 0xC763
        // carries the seconds.
        assert_eq!(
            derive("BMCC_2026-08-16_1903_C0002_000000.dng", "19:03:44:00").as_deref(),
            Some("2026:08:16 19:03:44")
        );
        assert_eq!(
            derive("2025-06-02_2252_C0000_000000.dng", "22:52:08:00").as_deref(),
            Some("2025:06:02 22:52:08")
        );
    }

    #[test]
    fn accepts_the_other_date_separators_and_the_bare_form() {
        for name in [
            "clip_2026_08_16_x.dng",
            "clip_2026.08.16_x.dng",
            "clip_20260816_x.dng",
        ] {
            assert_eq!(
                derive(name, "07:08:09:00").as_deref(),
                Some("2026:08:16 07:08:09"),
                "{name}"
            );
        }
    }

    #[test]
    fn no_date_in_the_name_yields_nothing() {
        assert_eq!(derive("A001_C0002_000000.dng", "19:03:44:00"), None);
        assert_eq!(derive("clip.dng", "19:03:44:00"), None);
    }

    #[test]
    fn a_longer_digit_run_is_not_read_as_a_date() {
        // Frame counters and serials must not be mistaken for a bare YYYYMMDD.
        assert_eq!(derive("SEQ_202608160001.dng", "01:02:03:00"), None);
        // ...and an implausible month/day is rejected rather than clamped.
        assert_eq!(derive("SEQ_20269999.dng", "01:02:03:00"), None);
    }

    #[test]
    fn an_unusable_timecode_yields_nothing() {
        // The timecode is the scoping mechanism: without a usable one there is
        // no derivation, so a dated filename alone never fabricates a date.
        for tc in ["", "not a timecode", "25:00:00:00", "19:03"] {
            assert_eq!(
                derive("BMCC_2026-08-16_1903_C0002_000000.dng", tc),
                None,
                "{tc:?}"
            );
        }
    }
}

#[cfg(test)]
mod image_sequence_resolve_tests {
    use super::{first_frame_candidates, resolve_image_sequence_first_frame};
    use gyroflow_core::filesystem::{get_filename, path_to_url};
    use std::path::{Path, PathBuf};

    // Each test owns a folder of its own so they can run concurrently.
    fn temp_seq_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("gyroflow_seq_resolve_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    // `path_to_url` percent-encodes the token's `%` into `%25`, so every test
    // below goes through the encoded form the app actually carries around.
    fn pattern_url(dir: &Path, name: &str) -> String {
        let url = path_to_url(&dir.join(name).to_string_lossy());
        assert!(url.contains("%25"), "expected an encoded pattern url, got {url}");
        url
    }

    #[test]
    fn non_pattern_url_returns_none() {
        let dir = temp_seq_dir("non_pattern");
        touch(&dir, "clip.mov");
        let url = path_to_url(&dir.join("clip.mov").to_string_lossy());
        assert_eq!(resolve_image_sequence_first_frame(&url, 0), None);
        assert_eq!(resolve_image_sequence_first_frame(&url, 7), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn candidate_order_and_dedup() {
        assert_eq!(first_frame_candidates(5), vec![5, 0, 1]);
        assert_eq!(first_frame_candidates(0), vec![0, 1]);
        assert_eq!(first_frame_candidates(1), vec![1, 0]);
        // Clamped, so it collapses onto the `0` candidate rather than adding one.
        assert_eq!(first_frame_candidates(-3), vec![0, 1]);
    }

    #[test]
    fn encoded_pattern_resolves_zero_based_sequence() {
        let dir = temp_seq_dir("zero_based");
        touch(&dir, "BMCC_C0002_000000.dng");
        touch(&dir, "BMCC_C0002_000001.dng");
        let url = pattern_url(&dir, "BMCC_C0002_%06d.dng");

        // Stale hint from a previously loaded clip still resolves via the `0`
        // candidate - the queue-Play / project-restore case.
        let resolved = resolve_image_sequence_first_frame(&url, 1994).unwrap();
        assert_eq!(get_filename(&resolved), "BMCC_C0002_000000.dng");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hint_wins_over_zero_when_both_exist() {
        let dir = temp_seq_dir("hint_wins");
        touch(&dir, "A001_000000.dng");
        touch(&dir, "A001_000005.dng");
        let url = pattern_url(&dir, "A001_%06d.dng");

        assert_eq!(
            get_filename(&resolve_image_sequence_first_frame(&url, 5).unwrap()),
            "A001_000005.dng"
        );
        assert_eq!(
            get_filename(&resolve_image_sequence_first_frame(&url, 0).unwrap()),
            "A001_000000.dng"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_scan_picks_lowest_number_and_filters_non_matches() {
        let dir = temp_seq_dir("dir_scan");
        // Numbered from 1994, so none of the {hint, 0, 1} candidates hit.
        touch(&dir, "SEQ_001996.dng");
        touch(&dir, "SEQ_001994.dng");
        touch(&dir, "SEQ_001995.dng");
        // Must all be filtered out: wrong prefix, wrong suffix, non-digit run,
        // empty digit run, and a folder that matches the name shape.
        touch(&dir, "OTHER_000001.dng");
        touch(&dir, "SEQ_000001.txt");
        touch(&dir, "SEQ_abcdef.dng");
        touch(&dir, "SEQ_.dng");
        std::fs::create_dir_all(dir.join("SEQ_000002.dng")).unwrap();
        let url = pattern_url(&dir, "SEQ_%06d.dng");

        let resolved = resolve_image_sequence_first_frame(&url, 42).unwrap();
        assert_eq!(get_filename(&resolved), "SEQ_001994.dng");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_scan_tolerates_padding_overflow() {
        let dir = temp_seq_dir("overflow");
        touch(&dir, "OVF_1000000.dng"); // 7 digits in a %06d sequence
        touch(&dir, "OVF_0999999.dng");
        let url = pattern_url(&dir, "OVF_%06d.dng");

        let resolved = resolve_image_sequence_first_frame(&url, 42).unwrap();
        assert_eq!(get_filename(&resolved), "OVF_0999999.dng");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_folder_returns_none() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("gyroflow_seq_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let url = path_to_url(&dir.join("NOPE_%06d.dng").to_string_lossy());
        assert_eq!(resolve_image_sequence_first_frame(&url, 3), None);
    }

    #[test]
    fn empty_folder_returns_none() {
        let dir = temp_seq_dir("empty");
        let url = pattern_url(&dir, "EMPTY_%06d.dng");
        assert_eq!(resolve_image_sequence_first_frame(&url, 3), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub fn peek_container_rotation_from_url(url: &str) -> i32 {
    let filename = gyroflow_core::filesystem::get_filename(url);
    if !filename.to_ascii_lowercase().ends_with(".r3d") {
        return 0;
    }
    let scheme = url.split("://").next().unwrap_or("");
    match gyroflow_core::filesystem::open_file(url, false, false) {
        Ok(mut file) => {
            let filesize = file.size;
            match gyroflow_core::util::get_video_metadata(file.get_file(), filesize, url) {
                Ok(md) => {
                    ::log::info!(
                        target: "video.load",
                        "peek_container_rotation: filename={} scheme={} rotation={}",
                        filename, scheme, md.rotation,
                    );
                    md.rotation as i32
                }
                Err(e) => {
                    ::log::warn!(
                        target: "video.load",
                        "peek_container_rotation: telemetry-parser failed for filename={} scheme={}: {}",
                        filename, scheme, e,
                    );
                    0
                }
            }
        }
        Err(e) => {
            ::log::warn!(
                target: "video.load",
                "peek_container_rotation: open_file failed for filename={} scheme={}: {}",
                filename, scheme, e,
            );
            0
        }
    }
}

// Probe the container frame size of an `.r3d` file via telemetry-parser's
// mp4parse path. Returns `Some((width, height))` from the MP4 `tkhd` track
// dimensions on success, `None` on any failure (open, parse, RED2 container,
// zero dimension).
//
// Used as a defensive fallback when MDK reports a clearly sub-native size for
// Nikon ZR `.r3d` (NR3D-codec in MP4 container) — e.g. proxy stream 249x140
// instead of native 3984x2240. Real RED2-container R3D files fail
// `parse_mp4` and we return `None`, so MDK's value persists for them.
pub fn peek_container_size_from_url(url: &str) -> Option<(u32, u32)> {
    let filename = gyroflow_core::filesystem::get_filename(url);
    if !filename.to_ascii_lowercase().ends_with(".r3d") {
        return None;
    }
    let scheme = url.split("://").next().unwrap_or("");
    match gyroflow_core::filesystem::open_file(url, false, false) {
        Ok(mut file) => {
            let filesize = file.size;
            match gyroflow_core::util::get_video_metadata(file.get_file(), filesize, url) {
                Ok(md) => {
                    if md.width > 0 && md.height > 0 {
                        ::log::info!(
                            target: "video.load",
                            "peek_container_size: filename={} scheme={} width={} height={}",
                            filename, scheme, md.width, md.height,
                        );
                        Some((md.width as u32, md.height as u32))
                    } else {
                        ::log::warn!(
                            target: "video.load",
                            "peek_container_size: telemetry-parser returned zero dimensions for filename={} scheme={} width={} height={}",
                            filename, scheme, md.width, md.height,
                        );
                        None
                    }
                }
                Err(e) => {
                    ::log::warn!(
                        target: "video.load",
                        "peek_container_size: telemetry-parser failed for filename={} scheme={}: {}",
                        filename, scheme, e,
                    );
                    None
                }
            }
        }
        Err(e) => {
            ::log::warn!(
                target: "video.load",
                "peek_container_size: open_file failed for filename={} scheme={}: {}",
                filename, scheme, e,
            );
            None
        }
    }
}

pub fn report_lens_profile_usage(_checksum: Option<String>) {
    // niyien fork: lens-profile usage telemetry to api.gyroflow.xyz is
    // disabled. The fork ships its own lens-data pipeline (niyien-lens-data)
    // and never reports usage upstream. Signature kept so the call sites
    // (export / render paths) stay untouched.
}
