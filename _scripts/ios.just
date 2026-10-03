import 'common.just'

# Important! For development/debugging the provisioning profile needs to be "Development"
# For distribution - it needs to be a distribution provisioning profile.

# Make sure to unlock the keychain if using ssh: security -v unlock-keychain -p 1234 login.keychain
# To list available certificates: security find-identity -v -p codesigning

TeamID                        := "H59FJRN2AM"
BundleIdentifier              := "com.niyien.stabilizer"
DevelopmentCertificateSHA     := "F4722C6FA73CC1749FFFDECFD1B04599513757B4"
DevelopmentProvisioning       := ProjectDir / "_deployment/ios/NiYien_dev.mobileprovision"
DevelopmentProvisioningName   := "Gyroflow dev"
DistributionCertificateSHA    := "91B8D990131A6DE4525121FC14D56B39633ED71F"
DistributionProvisioning      := ProjectDir / "_deployment/ios/NiYien_dist.mobileprovision"
DistributionProvisioningName  := "Gyroflow dist"

AppBundle := ProjectDir / "_deployment/_binaries/ios/Payload/NiYien.app"

QtVersionIOS := "6.7.3"

IPHONESDK := `xcode-select -p` + "/Platforms/iPhoneOS.platform/Developer/SDKs/iPhoneOS.sdk"
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_ios := "--target=arm64-apple-ios -arch arm64 -miphoneos-version-min=15 -isysroot " + IPHONESDK
export CFLAGS_aarch64_apple_darwin := "-mmacosx-version-min=10.15"
export IPHONEOS_DEPLOYMENT_TARGET := "15.0"

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Dependencies ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
install-deps:
    #!/bin/bash
    set -e
    brew install p7zip pkg-config ios-deploy
    brew list python &>/dev/null || brew install python
    rustup target add aarch64-apple-ios
    xcode-select --install || true
    xcodebuild -runFirstLaunch
    xcodebuild -downloadPlatform iOS || true

    export DYLD_FALLBACK_LIBRARY_PATH=""
    #export MACOSX_DEPLOYMENT_TARGET=""

    mkdir -p {{ExtDir}}
    cd {{ExtDir}}

    # Install vcpkg
    git clone --depth 1 https://github.com/Microsoft/vcpkg.git || true
    ./vcpkg/bootstrap-vcpkg.sh -disableMetrics

    if [ ! -d "{{ExtDir}}/{{QtVersionIOS}}/ios" ] || [ ! -d "{{ExtDir}}/{{QtVersionIOS}}/macos" ]; then
        echo "Setting up python venv in {{ExtDir}}/venv"
        python3 -m venv "{{ExtDir}}/venv"
        source "{{ExtDir}}/venv/bin/activate"
        # Install Qt
        python3 -m pip install -U pip aqtinstall
        python3 -m aqt install-qt mac desktop {{QtVersionIOS}}
        python3 -m aqt install-qt mac ios {{QtVersionIOS}}

        # Replace the Qt Quick Dialogs file with a patch to fix the file selection bug
        rm -f {{ExtDir}}/{{QtVersionIOS}}/ios/lib/libQt6QuickDialogs2.a
        : "${SDK_BASE:=https://www.niyien.com/api/sdk}"
        curl -L "$SDK_BASE/Qt-ios-{{QtVersionIOS}}-libQt6QuickDialogs2.a" -o {{ExtDir}}/{{QtVersionIOS}}/ios/lib/libQt6QuickDialogs2.a
    fi

    if [ ! -f "{{ExtDir}}/vcpkg/installed/arm64-ios/lib/libopencv_core4.a" ]; then
        echo "Installing OpenCV from vcpkg"
        # Install OpenCV
        ./vcpkg/vcpkg install "flatbuffers:arm64-ios" --cmake-args=-DVCPKG_BUILD_TYPE=release
        ./vcpkg/vcpkg install "opencv4[core,calib3d,thread,intrinsics]:arm64-ios" --cmake-args=-DVCPKG_BUILD_TYPE=release
        rm -rf ./vcpkg/buildtrees
        rm -rf ./vcpkg/downloads
    fi

    if [ ! -d "ffmpeg-8.1-iOS-gpl-lite" ]; then
        echo "Downloading ffmpeg for iOS"

        # Download and extract ffmpeg
        curl -L https://sourceforge.net/projects/avbuild/files/iOS/ffmpeg-8.1-iOS-gpl-lite.tar.xz/download -o ffmpeg.tar.xz
        7z x -aoa ffmpeg.tar.xz
        tar -xf ffmpeg.tar
        rm ffmpeg.tar ffmpeg.tar.xz
    fi

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Development ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
run:
    #!/bin/bash
    just -f {{justfile()}} build-and-package release development

    ios-deploy --bundle {{AppBundle}} -d -W

[no-cd]
debug:
    #!/bin/bash
    just -f {{justfile()}} build-and-package debug development

    ios-deploy --bundle {{AppBundle}} --debug

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Deployment ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
build-and-package profile is_dist:
    #!/bin/bash
    set -e

    python3 "{{ProjectDir}}/_scripts/ios_metadata.py" > /dev/null
    export CARGO_PROFILE_DEPLOY_DEBUG=1
    export FFMPEG_DIR="{{ProjectDir}}/ext/ffmpeg-8.1-iOS-gpl-lite"
    export PATH="{{ProjectDir}}/ext/{{QtVersionIOS}}/ios/bin:/usr/libexec/:$PATH"
    export QMAKE="{{ProjectDir}}/ext/{{QtVersionIOS}}/ios/bin/qmake"
    if [ -d "{{ProjectDir}}/ext/mdk-sdk-ios" ]; then
        export MDK_SDK="{{ProjectDir}}/ext/mdk-sdk-ios/"
    fi

    export OPENCV_LINK_LIBS="opencv_core4,opencv_calib3d4,opencv_features2d4,opencv_imgproc4,opencv_video4,opencv_flann4,opencv_stitching4"
    export OPENCV_LINK_PATHS={{ProjectDir}}/ext/vcpkg/installed/arm64-ios/lib
    export OPENCV_INCLUDE_PATHS={{ProjectDir}}/ext/vcpkg/installed/arm64-ios/include/opencv4/

    if [ "{{profile}}" == "debug" ]; then
        cargo build --target aarch64-apple-ios
    else
        cargo build --target aarch64-apple-ios --profile {{profile}}
    fi

    python3 "{{ProjectDir}}/_scripts/package_ios.py" --profile {{profile}} --signing {{is_dist}}

[no-cd]
deploy *param:
    #!/bin/bash
    set -e
    just -f {{justfile()}} build-and-package deploy distribution

[no-cd]
build-ipa:
    #!/bin/bash
    set -e
    just -f {{justfile()}} build-and-package deploy distribution

[no-cd]
package profile="deploy" signing="development":
    python3 "{{ProjectDir}}/_scripts/package_ios.py" --profile {{profile}} --signing {{signing}}
