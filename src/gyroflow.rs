// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#![recursion_limit = "4096"]
#![windows_subsystem = "windows"]

use cpp::*;
use qmetaobject::*;
use qml_video_rs::video_item::MDKVideoItem;
use std::cell::RefCell;

pub use gyroflow_core as core;
mod cli;
pub mod controller;
pub mod distribution;
pub mod external_sdk;
pub mod network;
pub mod niyien_device;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub mod nle_plugins;
pub mod rendering;
mod resources;
#[cfg(not(compiled_qml))]
mod resources_qml;
pub mod util;
pub use gyroflow_core::log_context;
pub mod logger;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod crash;
pub mod feedback;
pub mod ui {
    pub mod ui_tools;
    pub mod components {
        pub mod FrequencyGraph;
        pub mod Settings;
        pub mod TimelineGyroChart;
        pub mod TimelineKeyframesView;
    }
}
pub mod qt_gpu {
    pub mod qrhi_undistort;
}

use ui::components::FrequencyGraph::FrequencyGraph;
use ui::components::Settings::Settings;
use ui::components::TimelineGyroChart::TimelineGyroChart;
use ui::components::TimelineKeyframesView::TimelineKeyframesView;
use ui::ui_tools::{UITools, theme_name_from_index};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

cpp! {{
    #include <QQuickStyle>
    #include <QQuickWindow>
    #include <QQmlContext>
    #include <QLoggingCategory>
    #include <QtGui/QGuiApplication>
    #include <QIcon>

    #include "src/ui_live_reload.cpp"

    #ifdef Q_OS_ANDROID
    #   include <QtCore/private/qandroidextras_p.h>
    #endif
}}

fn entry() {
    // 本地 QML 开发调试时临时改为 true 可开启热重载（引擎从 CARGO_MANIFEST_DIR 读 QML 源文件）。
    // Release / CI build 必须为 false，否则 QML 路径会硬编码为构建机磁盘路径，
    // 导致用户运行时 UI 加载失败（见 gyroflow.log: "QQmlApplicationEngine failed to load component"）。
    let ui_live_reload = false;

    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::System::Console::*;
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() && cli::will_run_in_console() {
            let _ = AllocConsole();
        }
    }

    let _ = util::install_crash_handler();
    util::init_logging();
    // Wipe Qt RHI / QML caches when CARGO_PKG_VERSION changes; stale caches across
    // builds can corrupt heap and crash V4 in unrelated places. See util.rs for details.
    util::invalidate_qt_cache_if_version_changed();
    util::update_rlimit();
    util::set_android_context();
    log_panics::init();

    // Rust-panic crash dump (Phase 1 of feedback system). Installed AFTER
    // log_panics so this becomes the outermost wrapper: capture zip first,
    // then delegate to log_panics (which logs via log::error!) and finally
    // the default hook (terminal backtrace).
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        crash::register_panic_hook();
        crash::maybe_trigger_test_panic();
    }

    // Phase 4: retry any pending feedback uploads from a prior session in
    // the background. Idempotent — failed retries leave files in place.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        std::thread::Builder::new().name("feedback-retry-pending".into()).spawn(|| {
            feedback::uploader::retry_pending();
        }).ok();
    }

    // Enable cubecl SPIR-V pipeline cache before any wgpu device probe so the
    // cubecl GlobalConfig can still accept `set()`. Without this, NeuFlow Burn
    // warmup recompiles every kernel on every launch (~4-5 s).
    #[cfg(feature = "neuflow-burn")]
    core::neuflow_burn::init_cubecl_cache();

    let brand = gyroflow_core::distribution::config().brand.clone();
    let organization_name = QString::from(brand.organization_name.as_str());
    let organization_domain = QString::from(brand.organization_domain.as_str());
    let application_name = QString::from(brand.application_name.as_str());
    cpp!(unsafe [organization_name as "QString", organization_domain as "QString", application_name as "QString"] {
        qApp->setOrganizationName(organization_name);
        qApp->setOrganizationDomain(organization_domain);
        qApp->setApplicationName(application_name);

        QMessageLogger("", 0, "main").debug(QLoggingCategory("gyroflow")) << "Qt version:" << qVersion();
    });
    ::log::debug!("{} {}", brand.display_name, util::get_version());

    let mut open_file = String::new();
    let mut open_preset = String::new();
    if cli::run(&mut open_file, &mut open_preset) {
        return;
    }

    if cfg!(compiled_qml) {
        // For some reason on some devices QML detects that debugger is connected and fails to load pre-compiled qml files
        cpp!(unsafe [] { qputenv("QML_FORCE_DISK_CACHE", "1"); });
    }

    crate::resources::rsrc();
    #[cfg(not(compiled_qml))]
    crate::resources_qml::rsrc_qml();

    qml_video_rs::register_qml_types();
    qml_register_type::<TimelineGyroChart>(
        cstr::cstr!("Gyroflow"),
        1,
        0,
        cstr::cstr!("TimelineGyroChart"),
    );
    qml_register_type::<TimelineKeyframesView>(
        cstr::cstr!("Gyroflow"),
        1,
        0,
        cstr::cstr!("TimelineKeyframesView"),
    );
    qml_register_type::<FrequencyGraph>(
        cstr::cstr!("Gyroflow"),
        1,
        0,
        cstr::cstr!("FrequencyGraph"),
    );

    let icons_path = if ui_live_reload {
        QString::from(format!("{}/resources/icons/", env!("CARGO_MANIFEST_DIR")))
    } else {
        QString::from(":/resources/icons/")
    };
    cpp!(unsafe [icons_path as "QString"] {
        qputenv("QT_QUICK_FLICKABLE_WHEEL_DECELERATION", "5000");
        QQuickStyle::setStyle("Material");
        QIcon::setThemeName(QStringLiteral("Gyroflow"));
        QIcon::setThemeSearchPaths(QStringList() << icons_path);

        #ifdef Q_OS_ANDROID
            // QQuickWindow::setGraphicsApi(QSGRendererInterface::Vulkan);
            int av_jni_set_java_vm(void *vm, void *log_ctx);
            av_jni_set_java_vm(QJniEnvironment::javaVM(), nullptr);

            // FFmpeg 7.0
            // int av_jni_set_android_app_ctx(void *app_ctx, void *log_ctx);
            // av_jni_set_android_app_ctx(QNativeInterface::QAndroidApplication::context(), nullptr);
        #endif

        // QQuickWindow::setGraphicsApi(QSGRendererInterface::OpenGL);
        // QQuickWindow::setGraphicsApi(QSGRendererInterface::Vulkan);
        // QQuickWindow::setGraphicsApi(QSGRendererInterface::Direct3D12);
    });

    #[cfg(any(target_os = "android", target_os = "ios"))]
    if !gyroflow_core::settings::contains("defaultCodec") {
        gyroflow_core::settings::set("defaultCodec", 0.into()); // default to H.264 on mobile
    }

    util::save_exe_location();
    let sdk_path = external_sdk::SDK_PATH
        .as_ref()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_default();
    ::log::debug!(
        "Executable path: {:?}",
        gyroflow_core::settings::try_get("exeLocation")
    );
    ::log::debug!("SDK path: {:?}", sdk_path);

    //crate::core::util::rename_calib_videos();

    if cfg!(target_os = "windows") {
        MDKVideoItem::setGlobalOption(
            "MDK_KEY",
            "A51C879208C229AB56F271CB0376F1B18FC585991A505B4CF5C7C1F0853EA5500E8E626194AAE45149133B3980C3CEE691E004B023B6D17A4976F41786F73403BD1C786DF73DD654A90D8E34FC890E4E8DC592DE6322342A99A8B6D8CB57FC396BE04B4EDAC3BD382C7D49164DF638E677D115806AC5E99DBB3F8124",
        );
        MDKVideoItem::setGlobalOption("plugins", "mdk-braw:mdk-r3d");
    } else if cfg!(any(target_os = "macos", target_os = "ios", target_os = "android")) {
        MDKVideoItem::setGlobalOption(
            "MDK_KEY",
            "7A4BF7E7567EF19A279958461EB785798D83A8697B3F9D67FE13ABB7C8F487B6E15B9349945E786E01BB8ACFEC65AD9277DD1126F4FDE129632074E9866A4F065D4F0818A9810E65D866A7B9E1487A868F83BB0A1452B309976AC2D2A6DAE0CF9334F525FB2957E00C8DE8041D563F3B289AC8F281A490098A666116",
        );
        if cfg!(target_os = "ios") {
            MDKVideoItem::setGlobalOption("plugins", "mdk-braw");
        } else if cfg!(target_os = "macos") {
            MDKVideoItem::setGlobalOption("plugins", "mdk-braw:mdk-r3d");
        }
    } else if cfg!(target_os = "linux") {
        MDKVideoItem::setGlobalOption(
            "MDK_KEY",
            "44BDFE04425DC70DEB77DE87B40CF07EB251597FF7B5A9D436390D25FF53BECDA0E292DBDC50FC0F16445DD79E9C318C324077CEDCE8BA7A6A92E66E417DE00E04BD01FBBDA238F2148821784BF30F81B05156188EC7C6B25A567A08913AC7A4C58CE2D1DF663855B445EF2D6AD53D6E4432FE722EB2BE8F5DE4D878",
        );
        MDKVideoItem::setGlobalOption("plugins", "mdk-braw:mdk-r3d");
    }

    if cfg!(target_os = "linux") {
        // Init wgpu before Qt because of a bug in `khronos-egl`
        gyroflow_core::gpu::wgpu::WgpuWrapper::list_devices();
    }

    let _ = external_sdk::cleanup();

    let ctl = RefCell::new(controller::Controller::new());
    let ctlpinned = unsafe { QObjectPinned::new(&ctl) };

    let ui_tools = RefCell::new(UITools::default());
    let ui_tools_pinned = unsafe { QObjectPinned::new(&ui_tools) };

    let settings = RefCell::new(Settings::default());
    let settings_pinned = unsafe { QObjectPinned::new(&settings) };

    let rq = RefCell::new(rendering::render_queue::RenderQueue::new(
        ctl.borrow().stabilizer.clone(),
    ));
    let rqpinned = unsafe { QObjectPinned::new(&rq) };

    let fs = RefCell::new(controller::Filesystem::default());
    let fspinned = unsafe { QObjectPinned::new(&fs) };

    util::set_url_catcher(fspinned.get_or_create_cpp_object());
    util::register_url_handlers();

    let mut engine = QmlEngine::new();
    util::catch_qt_file_open(|url| {
        engine.set_property("openFileOnStart".into(), url.into());
    });
    let screen_size_inch = cpp!(unsafe[] -> f64 as "double" { auto size = QGuiApplication::primaryScreen()->physicalSize(); return std::sqrt(std::pow(size.width(), 2.0) + std::pow(size.height(), 2.0)) / (2.54 * 10.0); });
    let mut dpi = cpp!(unsafe[] -> f64 as "double" { return QGuiApplication::primaryScreen()->logicalDotsPerInch() / 96.0; });
    if cfg!(any(target_os = "android", target_os = "ios")) {
        dpi *= 1.2;
    }
    engine.set_property("screenSize".into(), QVariant::from(screen_size_inch));
    engine.set_property("dpiScale".into(), QVariant::from(dpi));
    engine.set_property("version".into(), QString::from(util::get_version()).into());
    engine.set_property(
        "brandDisplayName".into(),
        QString::from(
            gyroflow_core::distribution::config()
                .brand
                .display_name
                .as_str(),
        )
        .into(),
    );
    engine.set_property("graphics_api".into(), util::qt_graphics_api().into());
    engine.set_object_property("main_controller".into(), ctlpinned);
    engine.set_object_property("ui_tools".into(), ui_tools_pinned);
    engine.set_object_property("settings".into(), settings_pinned);
    engine.set_object_property("render_queue".into(), rqpinned);
    engine.set_object_property("filesystem".into(), fspinned);
    {
        let mut ui = ui_tools.borrow_mut();
        ui.engine_ptr = Some(&mut engine as *mut _);
        let theme = gyroflow_core::settings::get_u64("theme", 1);
        ui.set_theme(theme_name_from_index(theme).into());
    }

    engine.set_property("isStorePackage".into(), util::is_store_package().into());
    engine.set_property(
        "isMobile".into(),
        cfg!(any(target_os = "android", target_os = "ios")).into(),
    );
    engine.set_property(
        "isSandboxed".into(),
        gyroflow_core::filesystem::is_sandboxed().into(),
    );

    // Get smoothing algorithms
    engine.set_property(
        "smoothingAlgorithms".into(),
        QVariant::from(ctl.borrow().get_smoothing_algs()),
    );

    let engine_ptr = engine.cpp_ptr();

    // Load main UI
    if !ui_live_reload {
        use std::path::PathBuf;
        // Try to load from disk first
        let path = (|| -> Option<String> {
            let path = if cfg!(any(target_os = "macos", target_os = "ios")) {
                PathBuf::from("../Resources/ui/main_window.qml")
            } else {
                PathBuf::from("./ui/main_window.qml")
            };
            let final_path = std::env::current_exe().ok()?.parent()?.join(path);
            if final_path.exists() {
                Some(String::from(final_path.to_str()?))
            } else {
                None
            }
        })();
        if let Some(path) = path {
            engine.load_file(path.into());
        } else {
            // Load from resources
            engine.load_url(QString::from("qrc:/src/ui/main_window.qml").into());
        }
    } else {
        engine.load_file(format!("{}/src/ui/main_window.qml", env!("CARGO_MANIFEST_DIR")).into());
        let ui_path = QString::from(format!("{}/src/ui", env!("CARGO_MANIFEST_DIR")));
        cpp!(unsafe [engine_ptr as "QQmlApplicationEngine *", ui_path as "QString"] { init_live_reload(engine_ptr, ui_path); });
    }

    cpp!(unsafe [] {
        #ifdef Q_OS_ANDROID
            QtAndroidPrivate::requestPermission("android.permission.READ_EXTERNAL_STORAGE").result();
            QtAndroidPrivate::requestPermission("android.permission.WRITE_EXTERNAL_STORAGE").result();
            QtAndroidPrivate::requestPermission("android.permission.READ_MEDIA_VIDEO").result();
        #endif
    });

    ctl.borrow_mut()
        .stabilizer
        .params
        .write()
        .framebuffer_inverted = util::is_opengl();

    rendering::init_log();

    engine.set_property(
        "openFileOnStart".into(),
        QUrl::from(QString::from(gyroflow_core::filesystem::path_to_url(
            &open_file,
        )))
        .into(),
    );
    engine.set_property(
        "loadPresetOnStart".into(),
        QString::from(open_preset).into(),
    );

    engine.set_property("defaultInitializedDevice".into(), QString::default().into());
    if let Some((name, list_name)) = core::gpu::initialize_contexts() {
        rendering::set_gpu_type_from_name(&name);
        engine.set_property(
            "defaultInitializedDevice".into(),
            QString::from(list_name).into(),
        );
    }

    // Pre-load NeuFlow sessions in background while user interacts with UI
    #[cfg(feature = "neuflow-ort")]
    std::thread::spawn(|| core::neuflow::ensure_ready());
    #[cfg(feature = "neuflow-burn")]
    std::thread::spawn(|| core::neuflow_burn::ensure_ready());

    engine.exec();

    gyroflow_core::settings::flush();
    util::unregister_url_handlers();
}

#[unsafe(no_mangle)]
#[cfg(target_os = "android")]
pub extern "C" fn main(_argc: i32, _argv: *mut *mut i8) -> i32 {
    entry();
    0
}

#[cfg(not(target_os = "android"))]
fn main() {
    entry();
}
