# SPDX-License-Identifier: GPL-3.0-or-later
# Set MDK_SDK to the SDK used by the application.
QT += quick quick-private gui-private
CONFIG += console c++17 release
CONFIG -= debug debug_and_release
TARGET = mdk-listener-lifetime
SOURCES += $$PWD/mdk_listener_lifetime.cpp
mdkSdk = $$(MDK_SDK)
isEmpty(mdkSdk): error(Set MDK_SDK to the host MDK SDK directory)
INCLUDEPATH += $$quote($$PWD/../../vendor/qml-video-rs/src/cpp) $$quote($$mdkSdk/include)
win32 {
    LIBS += $$quote($$mdkSdk/lib/x64/mdk.lib) dxguid.lib
    DEFINES += _DISABLE_CONSTEXPR_MUTEX_CONSTRUCTOR
} else {
    error(This host regression currently requires Windows x64 Qt and MDK)
}
