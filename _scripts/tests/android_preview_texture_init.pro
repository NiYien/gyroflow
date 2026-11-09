# SPDX-License-Identifier: GPL-3.0-or-later
# Build with the host Qt used by qml-video-rs and MDK_SDK pointing to its matching SDK.
# MDK_PREVIEW_SOURCE_DIR can select an older production cpp directory for comparison.
QT += quick quick-private gui-private
CONFIG += console c++17 release
CONFIG -= debug debug_and_release
TARGET = android-preview-texture-init
SOURCES += $$PWD/android_preview_texture_init.cpp
mdkSdk = $$(MDK_SDK)
isEmpty(mdkSdk): error(Set MDK_SDK to the host MDK SDK directory)
previewSource = $$(MDK_PREVIEW_SOURCE_DIR)
isEmpty(previewSource): previewSource = $$PWD/../../vendor/qml-video-rs/src/cpp
INCLUDEPATH += $$quote($$previewSource) $$quote($$mdkSdk/include)
win32 {
    LIBS += $$quote($$mdkSdk/lib/x64/mdk.lib) dxguid.lib
    DEFINES += _DISABLE_CONSTEXPR_MUTEX_CONSTRUCTOR
} else {
    error(This host regression currently requires Windows x64 Qt and MDK)
}
