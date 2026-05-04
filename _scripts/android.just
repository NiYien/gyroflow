import 'common.just'

export ANDROID_NDK_HOME := env_var_or_default("ANDROID_NDK_HOME", "D:\\Programy\\Android\\sdk\\ndk\\android-ndk-r27d")
export ANDROID_SDK_ROOT := env_var_or_default("ANDROID_SDK_ROOT", "D:\\Programy\\Android\\sdk\\")
export JAVA_HOME        := env_var_or_default("JAVA_HOME",        "D:\\Programy\\Java\\jdk-20.0.2")
export BUILD_PROFILE    := env_var_or_default("BUILD_PROFILE",    "release") # use "deploy" for final apk

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Dependencies ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
install-deps:
    #!powershell
    $ProgressPreference = 'SilentlyContinue'
    $ErrorActionPreference = 'Stop'
    mkdir "{{ExtDir}}" -ErrorAction SilentlyContinue
    cd {{ExtDir}}

    $Python = "python";
    if (Get-Command "python.exe" -ErrorAction SilentlyContinue) {
        $Python = "python";
    } elseif (Get-Command "python3.exe" -ErrorAction SilentlyContinue) {
        $Python = "python3";
    } else {
        if (-not (Test-Path -Path "{{ExtDir}}/WPy64-310100b3")) {
            echo "Downloading WinPython"
            wget https://github.com/winpython/winpython/releases/download/6.1.20230227/Winpython64-3.10.10.0dotb3.exe -outfile "winpython.exe"
            7z x -y winpython.exe
            del winpython.exe
        }
        $Python = resolve-path "{{ExtDir}}\WPy64-310100b3\python-3.10.10.amd64\python.exe"
    }

    cargo install --git https://github.com/rust-mobile/cargo-apk.git cargo-apk
    rustup target add aarch64-linux-android

    # ――――――――――――――――――――――――――――――――――――――――― Qt ――――――――――――――――――――――――――――――――――――――――――
    if (-not (Test-Path -Path "{{QtVersion}}/android_arm64_v8a")) {
        echo "Downloading Qt {{QtVersion}} for Android"
        & $Python -m pip install -U pip aqtinstall===3.2.0
        & $Python -m aqt install-qt windows desktop {{QtVersion}} win64_mingw
        & $Python -m aqt install-qt windows android {{QtVersion}} android_arm64_v8a
    }
    # ――――――――――――――――――――――――――――――――――――――――― Qt ――――――――――――――――――――――――――――――――――――――――――

    # ――――――――――――――――――――――――――――――――――――――― FFmpeg ――――――――――――――――――――――――――――――――――――――――
    $FFmpeg = "ffmpeg-8.1-android-gpl-lite"
    if (-not (Test-Path -Path "$FFmpeg")) {
        echo "Downloading $FFmpeg"
        wget "https://master.dl.sourceforge.net/project/avbuild/android/$FFmpeg.tar.xz?viasf=1" -outfile "ffmpeg.tar.xz"
        7z x -aoa ffmpeg.tar.xz
        del ffmpeg.tar.xz
        7z x -aoa ffmpeg.tar
        del ffmpeg.tar
    }
    # ――――――――――――――――――――――――――――――――――――――― FFmpeg ――――――――――――――――――――――――――――――――――――――――

    # ――――――――――――――――――――――――――――――――――――――― OpenCV ――――――――――――――――――――――――――――――――――――――――
    if (-not (Test-Path -Path "OpenCV-android-sdk/")) {
        echo "Downloading OpenCV for Android"
        wget "https://github.com/opencv/opencv/releases/download/4.11.0/opencv-4.11.0-android-sdk.zip" -outfile "opencv.zip"
        7z x -aoa opencv.zip
        del opencv.zip
    }
    # ――――――――――――――――――――――――――――――――――――――― OpenCV ――――――――――――――――――――――――――――――――――――――――

    # ――――――――――――――――――――――――――――――――――――――― LLVM ――――――――――――――――――――――――――――――――――――――――
    if (-not (Test-Path -Path "{{LLVMPath}}\libclang.dll")) {
        echo "Downloading LLVM"
        wget "https://github.com/llvm/llvm-project/releases/download/llvmorg-19.1.7/LLVM-19.1.7-win64.exe" -outfile "llvm-win64.exe"
        7z x -y llvm-win64.exe -ollvm
        del "llvm-win64.exe"
    }
    # ――――――――――――――――――――――――――――――――――――――― LLVM ――――――――――――――――――――――――――――――――――――――――

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Development ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

#[no-cd]
#run *param:
#    cargo run --release -- {{param}}
#
#[no-cd]
#debug *param:
#    cargo run -- {{param}}

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Deployment ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
deploy *param:
    #!powershell
    $ErrorActionPreference = 'Stop'

    $ProjectDir = (resolve-path "{{ProjectDir}}").ToString()
    cd $ProjectDir

    $Env:CARGO_TARGET_DIR = "$ProjectDir\target"

    $CargoTomlPath = "$ProjectDir\Cargo.toml"
    $ModalQmlPath = "$ProjectDir\src\ui\components\Modal.qml"
    $AndroidManifestPath = "$ProjectDir\_deployment\android\AndroidManifest.xml"
    $OriginalCargoToml = [System.IO.File]::ReadAllText($CargoTomlPath)
    $OriginalModalQml = [System.IO.File]::ReadAllText($ModalQmlPath)
    $OriginalAndroidManifest = [System.IO.File]::ReadAllText($AndroidManifestPath)

    function Restore-TemporarySourceEdits {
        [System.IO.File]::WriteAllText($CargoTomlPath, $OriginalCargoToml)
        [System.IO.File]::WriteAllText($ModalQmlPath, $OriginalModalQml)
        [System.IO.File]::WriteAllText($AndroidManifestPath, $OriginalAndroidManifest)
    }

    function Assert-TemporarySourceEditsRestored {
        if ([System.IO.File]::ReadAllText($CargoTomlPath) -ne $OriginalCargoToml) {
            throw "Temporary Android source edits were not restored: Cargo.toml"
        }
        if ([System.IO.File]::ReadAllText($ModalQmlPath) -ne $OriginalModalQml) {
            throw "Temporary Android source edits were not restored: src/ui/components/Modal.qml"
        }
        if ([System.IO.File]::ReadAllText($AndroidManifestPath) -ne $OriginalAndroidManifest) {
            throw "Temporary Android source edits were not restored: _deployment/android/AndroidManifest.xml"
        }
    }

    function Require-AndroidReleaseSigning {
        $missing = @()
        if (-not $Env:KEY_STORE_PATH) { $missing += "KEY_STORE_PATH" }
        if (-not $Env:KEY_STORE_ALIAS) { $missing += "KEY_STORE_ALIAS" }
        if (-not $Env:KEY_STORE_PASS) { $missing += "KEY_STORE_PASS" }
        if ($missing.Count -gt 0) {
            throw "Android release signing requires: $($missing -join ', ')"
        }
        if (-not (Test-Path -LiteralPath $Env:KEY_STORE_PATH)) {
            throw "Android release keystore does not exist: $Env:KEY_STORE_PATH"
        }
    }

    function Set-CargoApkDebugSigningEnvironment {
        param([string]$Profile)
        $DebugKeystorePath = Join-Path $Env:USERPROFILE ".android\debug.keystore"
        if (-not (Test-Path -LiteralPath $DebugKeystorePath)) {
            New-Item -Type Directory (Split-Path -Parent $DebugKeystorePath) -ErrorAction SilentlyContinue | Out-Null
            & keytool -genkeypair -v -keystore $DebugKeystorePath -storepass android -alias androiddebugkey -keypass android -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
            Assert-NativeCommandSucceeded "keytool debug keystore"
        }
        $ProfileEnv = $Profile.ToUpperInvariant().Replace("-", "_")
        [System.Environment]::SetEnvironmentVariable("CARGO_APK_${ProfileEnv}_KEYSTORE", $DebugKeystorePath, "Process")
        [System.Environment]::SetEnvironmentVariable("CARGO_APK_${ProfileEnv}_KEYSTORE_PASSWORD", "android", "Process")
    }

    function Assert-NativeCommandSucceeded {
        param([string]$Step)
        if ($LASTEXITCODE -ne 0) {
            throw "$Step failed with exit code $LASTEXITCODE"
        }
    }

    $job = $null

    $QT_LIBS = "$ProjectDir\ext\{{QtVersion}}\android_arm64_v8a\lib"
    $Env:Path = "$ProjectDir\ext\{{QtVersion}}\android_arm64_v8a\bin;$ProjectDir\ext\{{QtVersion}}\mingw_64\bin\;$Env:Path"
    $Env:QMAKE = "$ProjectDir\ext\{{QtVersion}}\android_arm64_v8a\bin\qmake.bat"
    $Env:FFMPEG_DIR = "$ProjectDir\ext\ffmpeg-8.1-android-gpl-lite"
    $Env:OPENCV_LINK_LIBS = "opencv_stitching,opencv_calib3d,opencv_features2d,opencv_imgproc,opencv_video,opencv_flann,opencv_core,tegra_hal,tbb,ittnotify,z,kleidicv,kleidicv_hal,kleidicv_thread"
    $Env:OPENCV_LINK_PATHS = "$ProjectDir\ext\OpenCV-android-sdk\sdk\native\staticlibs\arm64-v8a,$ProjectDir\ext\OpenCV-android-sdk\sdk\native\3rdparty\libs\arm64-v8a"
    $Env:OPENCV_INCLUDE_PATHS = "$ProjectDir\ext\OpenCV-android-sdk\sdk\native\jni\include"

    $CLANG_LIB = $Env:LIBCLANG_PATH.replace('\', '/').replace('/bin', '/lib');
    $NDK_REPLACED = $Env:ANDROID_NDK_HOME.replace('\', '/');
    $SDK_REPLACED = $Env:ANDROID_SDK_ROOT.replace('\', '/');
    $PROJECT_DIR_UNIX = $ProjectDir.replace('\', '/');
    $Env:BINDGEN_EXTRA_CLANG_ARGS = "-I$CLANG_LIB/clang/16.0.0/include --sysroot=$NDK_REPLACED/toolchains/llvm/prebuilt/windows-x86_64/sysroot"
    echo $Env:BINDGEN_EXTRA_CLANG_ARGS

    $IsTagRelease = "$Env:GITHUB_REF_TYPE" -eq "tag" -or "$Env:GITHUB_REF" -like "refs/tags/*"
    $HasReleaseSigningEnv = $Env:KEY_STORE_PATH -and $Env:KEY_STORE_ALIAS -and $Env:KEY_STORE_PASS
    $UseReleaseSigning = $false
    if ($IsTagRelease -or $HasReleaseSigningEnv) {
        Require-AndroidReleaseSigning
        $UseReleaseSigning = $true
    }
    $CargoApkProfile = $Env:BUILD_PROFILE

    try {
        if ("$Env:GITHUB_RUN_NUMBER") {
            $runNumber = $Env:GITHUB_RUN_NUMBER
            [System.IO.File]::WriteAllText($AndroidManifestPath, [regex]::Replace([System.IO.File]::ReadAllText($AndroidManifestPath), 'versionCode="\d+"', "versionCode=""$runNumber"""))
        }

        Copy-Item -Path "$QT_LIBS\libQt6Core_arm64-v8a.so"           -Destination "$QT_LIBS\libQt6Core.so"           -ErrorAction SilentlyContinue
        Copy-Item -Path "$QT_LIBS\libQt6Gui_arm64-v8a.so"            -Destination "$QT_LIBS\libQt6Gui.so"            -ErrorAction SilentlyContinue
        Copy-Item -Path "$QT_LIBS\libQt6Widgets_arm64-v8a.so"        -Destination "$QT_LIBS\libQt6Widgets.so"        -ErrorAction SilentlyContinue
        Copy-Item -Path "$QT_LIBS\libQt6Quick_arm64-v8a.so"          -Destination "$QT_LIBS\libQt6Quick.so"          -ErrorAction SilentlyContinue
        Copy-Item -Path "$QT_LIBS\libQt6Qml_arm64-v8a.so"            -Destination "$QT_LIBS\libQt6Qml.so"            -ErrorAction SilentlyContinue
        Copy-Item -Path "$QT_LIBS\libQt6QuickControls2_arm64-v8a.so" -Destination "$QT_LIBS\libQt6QuickControls2.so" -ErrorAction SilentlyContinue

        # Make sure qmlimportscanner finds QtQuick.Effects
        [System.IO.File]::WriteAllText($ModalQmlPath, [System.IO.File]::ReadAllText($ModalQmlPath).Replace("`n// import QtQuick.Effects", "`nimport QtQuick.Effects"))

        # Replace [[bin]] with [lib]
        [System.IO.File]::WriteAllText($CargoTomlPath, [System.IO.File]::ReadAllText($CargoTomlPath).Replace("[[bin]]", "[lib]`ncrate-type = [""cdylib""]"))

        # Replace gradle version
        $GradleWrapperPropertiesPath = "$QT_LIBS\..\src\3rdparty\gradle\gradle\wrapper\gradle-wrapper.properties"
        [System.IO.File]::WriteAllText($GradleWrapperPropertiesPath, [System.IO.File]::ReadAllText($GradleWrapperPropertiesPath).Replace("gradle-8.3-bin", "gradle-8.6-bin").Replace("networkTimeout=10000", "networkTimeout=120000"))
        [System.IO.File]::WriteAllText("$QT_LIBS\..\src\android\templates\build.gradle", [System.IO.File]::ReadAllText("$QT_LIBS\..\src\android\templates\build.gradle").Replace("7.4.1", "8.4.2"))

        # cargo apk only needs a signing key for the intermediate build.
        # Keep the release key for the final androiddeployqt signing step.
        Set-CargoApkDebugSigningEnvironment $CargoApkProfile
        cargo apk build --profile $CargoApkProfile
        Assert-NativeCommandSucceeded "cargo apk build"

        mkdir "$ProjectDir\target\android-build" -ErrorAction SilentlyContinue
        mkdir "$ProjectDir\target\android-build\libs" -ErrorAction SilentlyContinue
        Copy-Item -Path "$ProjectDir\target\$CargoApkProfile\apk\lib\*" -Destination "$ProjectDir\target\android-build\libs\" -Recurse -Force
        # Copy-Item -Path "$ProjectDir\_deployment\android\src" -Destination "$ProjectDir\target\android-build\" -Recurse -Force
        Copy-Item -Path "$ProjectDir\target\aarch64-linux-android\$CargoApkProfile\libffmpeg.so" -Destination "$ProjectDir\target\android-build\libs\arm64-v8a\" -Force
        # Copy-Item -Path "$ProjectDir\target\aarch64-linux-android\$CargoApkProfile\libqtav-mediacodec.so" -Destination "$ProjectDir\target\android-build\libs\arm64-v8a\" -Force
        Move-Item -Path "$ProjectDir\target\android-build\libs\arm64-v8a\libgyroflow.so" -Destination "$ProjectDir\target\android-build\libs\arm64-v8a\libgyroflow_arm64-v8a.so" -Force

        $qtlibs = @(
            "libQt6Core_arm64-v8a.so",
            "libQt6Gui_arm64-v8a.so",
            "libQt6LabsFolderListModel_arm64-v8a.so",
            "libQt6Network_arm64-v8a.so",
            "libQt6OpenGL_arm64-v8a.so",
            "libQt6Qml_arm64-v8a.so",
            "libQt6QmlLocalStorage_arm64-v8a.so",
            "libQt6QmlModels_arm64-v8a.so",
            "libQt6QmlWorkerScript_arm64-v8a.so",
            "libQt6QmlXmlListModel_arm64-v8a.so",
            "libQt6Quick_arm64-v8a.so",
            "libQt6QuickControls2_arm64-v8a.so",
            "libQt6QuickControls2Impl_arm64-v8a.so",
            "libQt6QuickControls2Material_arm64-v8a.so",
            "libQt6QuickControls2MaterialStyleImpl_arm64-v8a.so",
            "libQt6QuickControls2Basic_arm64-v8a.so",
            "libQt6QuickControls2BasicStyleImpl_arm64-v8a.so",
            "libQt6QuickDialogs2_arm64-v8a.so",
            "libQt6QuickDialogs2QuickImpl_arm64-v8a.so",
            "libQt6QuickDialogs2Utils_arm64-v8a.so",
            "libQt6QuickLayouts_arm64-v8a.so",
            "libQt6QuickParticles_arm64-v8a.so",
            "libQt6QuickShapes_arm64-v8a.so",
            "libQt6QuickTemplates2_arm64-v8a.so",
            "libQt6QuickEffects_arm64-v8a.so",
            "libQt6Sql_arm64-v8a.so",
            "libQt6Svg_arm64-v8a.so",
            "libQt6Widgets_arm64-v8a.so",
            "..\plugins\iconengines\libplugins_iconengines_qsvgicon_arm64-v8a.so",
            "..\plugins\imageformats\libplugins_imageformats_qsvg_arm64-v8a.so",
            "..\plugins\sqldrivers\libplugins_sqldrivers_qsqlite_arm64-v8a.so",
            "..\qml\Qt\labs\folderlistmodel\libqml_Qt_labs_folderlistmodel_qmlfolderlistmodelplugin_arm64-v8a.so",
            "..\qml\QtCore\libqml_QtCore_qtqmlcoreplugin_arm64-v8a.so",
            "..\qml\QtQml\Base\libqml_QtQml_Base_qmlplugin_arm64-v8a.so",
            "..\qml\QtQml\libqml_QtQml_qmlmetaplugin_arm64-v8a.so",
            "..\qml\QtQml\Models\libqml_QtQml_Models_modelsplugin_arm64-v8a.so",
            "..\qml\QtQml\WorkerScript\libqml_QtQml_WorkerScript_workerscriptplugin_arm64-v8a.so",
            "..\qml\QtQml\XmlListModel\libqml_QtQml_XmlListModel_qmlxmllistmodelplugin_arm64-v8a.so",
            "..\qml\QtQuick\Controls\Basic\impl\libqml_QtQuick_Controls_Basic_impl_qtquickcontrols2basicstyleimplplugin_arm64-v8a.so",
            "..\qml\QtQuick\Controls\Basic\libqml_QtQuick_Controls_Basic_qtquickcontrols2basicstyleplugin_arm64-v8a.so",
            "..\qml\QtQuick\Controls\impl\libqml_QtQuick_Controls_impl_qtquickcontrols2implplugin_arm64-v8a.so",
            "..\qml\QtQuick\Controls\libqml_QtQuick_Controls_qtquickcontrols2plugin_arm64-v8a.so",
            "..\qml\QtQuick\Controls\Material\impl\libqml_QtQuick_Controls_Material_impl_qtquickcontrols2materialstyleimplplugin_arm64-v8a.so",
            "..\qml\QtQuick\Controls\Material\libqml_QtQuick_Controls_Material_qtquickcontrols2materialstyleplugin_arm64-v8a.so",
            "..\qml\QtQuick\Dialogs\libqml_QtQuick_Dialogs_qtquickdialogsplugin_arm64-v8a.so",
            "..\qml\QtQuick\Dialogs\quickimpl\libqml_QtQuick_Dialogs_quickimpl_qtquickdialogs2quickimplplugin_arm64-v8a.so",
            "..\qml\QtQuick\Effects\libqml_QtQuick_Effects_effectsplugin_arm64-v8a.so",
            "..\qml\QtQuick\Layouts\libqml_QtQuick_Layouts_qquicklayoutsplugin_arm64-v8a.so",
            "..\qml\QtQuick\libqml_QtQuick_qtquick2plugin_arm64-v8a.so",
            "..\qml\QtQuick\LocalStorage\libqml_QtQuick_LocalStorage_qmllocalstorageplugin_arm64-v8a.so",
            "..\qml\QtQuick\NativeStyle\libqml_QtQuick_NativeStyle_qtquickcontrols2nativestyleplugin_arm64-v8a.so",
            "..\qml\QtQuick\Particles\libqml_QtQuick_Particles_particlesplugin_arm64-v8a.so",
            "..\qml\QtQuick\Shapes\libqml_QtQuick_Shapes_qmlshapesplugin_arm64-v8a.so",
            "..\qml\QtQuick\Templates\libqml_QtQuick_Templates_qtquicktemplates2plugin_arm64-v8a.so",
            "..\qml\QtQuick\tooling\libqml_QtQuick_tooling_quicktoolingplugin_arm64-v8a.so",
            "..\qml\QtQuick\Window\libqml_QtQuick_Window_quickwindowplugin_arm64-v8a.so"
        );
        foreach ($x in $qtlibs) {
            Copy-Item -Path "$QT_LIBS\$x" -Destination "$ProjectDir\target\android-build\libs\arm64-v8a\" -Force -ErrorAction SilentlyContinue
        }

        # Strip all libs
        . "$Env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-strip.exe" "$ProjectDir\target\android-build\libs\arm64-v8a\*.so"
        Assert-NativeCommandSucceeded "llvm-strip"

        # Copy unstripped libgyroflow.so
        Copy-Item -Path "$ProjectDir\target\$CargoApkProfile\apk\lib\arm64-v8a\libgyroflow.so" -Destination "$ProjectDir\target\android-build\libs\arm64-v8a\libgyroflow_arm64-v8a.so" -Force

        $androiddeploy = @"
    {
        "description": "",
        "qt": "$PROJECT_DIR_UNIX/ext/{{QtVersion}}/android_arm64_v8a",
        "qtDataDirectory": ".",
        "qtLibExecsDirectory": "libexec",
        "qtLibsDirectory": "lib",
        "qtPluginsDirectory": "plugins",
        "qtQmlDirectory": "qml",
        "sdk": "$SDK_REPLACED",
        "sdkBuildToolsRevision": "36.0.0",
        "ndk": "$NDK_REPLACED",
        "toolchain-prefix": "llvm",
        "tool-prefix": "llvm",
        "ndk-host": "windows-x86_64",
        "architectures": {"arm64-v8a":"aarch64-linux-android"},
        "android-min-sdk-version": "26",
        "android-package-source-directory": "$PROJECT_DIR_UNIX/_deployment/android",
        "android-target-sdk-version": "36",
        "qml-importscanner-binary": "$PROJECT_DIR_UNIX/ext/{{QtVersion}}/mingw_64/bin/qmlimportscanner.exe",
        "rcc-binary": "$PROJECT_DIR_UNIX/ext/{{QtVersion}}/mingw_64/bin/rcc.exe",
        "qml-root-path": "$PROJECT_DIR_UNIX/src",
        "stdcpp-path": "$NDK_REPLACED/toolchains/llvm/prebuilt/windows-x86_64/sysroot/usr/lib",
        "qrcFiles": "",
        "application-binary": "gyroflow"
    }
    "@
        $androiddeploy | Out-File -encoding utf8 -FilePath "$ProjectDir\target\android-build\android-deploy.json"

        $job = Start-Job -Name "Cleanup" -ArgumentList "$ProjectDir\target\android-build\libs\arm64-v8a" -ScriptBlock {
        for ($num = 1; $num -le 800 ; $num++) {
            try {
                $c = [System.IO.File]::ReadAllText("${args}\..\..\res\values\libs.xml") -replace "<item.+?(Fusion|Imagine|Universal).+?item>", ""
                [System.IO.File]::WriteAllText("${args}\..\..\res\values\libs.xml", $c)
                Remove-Item "${args}\*Universal*"
                Remove-Item "${args}\*Imagine*"
                Remove-Item "${args}\*Fusion*"
                Remove-Item "${args}\*Particles*"
                Remove-Item "${args}\*tooling*"
                Remove-Item "${args}\*qgif*"
                Remove-Item "${args}\*qico*"
            } catch {

            }
            Start-Sleep -Milliseconds 150;
        }
        }
        Get-job | Receive-Job

        $Sign = ""
        $ApkName = "apk\debug\android-build-debug.apk"
        if ($UseReleaseSigning) {
            Require-AndroidReleaseSigning
            $Sign = "--release --sign `"$Env:KEY_STORE_PATH`" `"$Env:KEY_STORE_ALIAS`" --storepass `"$Env:KEY_STORE_PASS`""
            $ApkName = "apk\release\android-build-release-signed.apk"
        }

        $cmd = "androiddeployqt --input `"$ProjectDir\target\android-build\android-deploy.json`" --output `"$ProjectDir\target\android-build`" --deployment bundled $Sign --android-platform android-36 --jdk ${Env:JAVA_HOME} --gradle"

        iex "$cmd"
        Assert-NativeCommandSucceeded "androiddeployqt apk"
        if ("$Env:BUILD_APP_STORE_BUNDLE" -eq "true") {
            iex "$cmd --aab"
            Assert-NativeCommandSucceeded "androiddeployqt aab"
        }

        if ($job) {
            Stop-Job $job
            Remove-Job $job
            $job = $null
        }

        New-Item -Type dir "$ProjectDir\_deployment\_binaries\" -ErrorAction SilentlyContinue
        $BuiltApkPath = "$ProjectDir\target\android-build\build\outputs\$ApkName"
        $FinalApkPath = "$ProjectDir\_deployment\_binaries\{{ArtifactPrefix}}.apk"
        if (-not (Test-Path -LiteralPath $BuiltApkPath)) {
            throw "Expected APK was not produced: $BuiltApkPath"
        }
        Copy-Item -Path $BuiltApkPath -Destination $FinalApkPath -Force
        if (-not (Test-Path -LiteralPath $FinalApkPath)) {
            throw "Expected APK was not copied: $FinalApkPath"
        }
        Copy-Item -Path "$ProjectDir\target\android-build\build\outputs\bundle\release\android-build-release.aab" -Destination "$ProjectDir\_deployment\_binaries\{{ArtifactPrefix}}.aab" -Force -ErrorAction SilentlyContinue

        if ("{{param}}" -eq "install") {
            adb install "$ProjectDir\_deployment\_binaries\{{ArtifactPrefix}}.apk"
            Assert-NativeCommandSucceeded "adb install"
            adb logcat -c
            Assert-NativeCommandSucceeded "adb logcat clear"
            adb shell am start -n com.niyien.gyroflow/com.niyien.gyroflow.MainActivity
            Assert-NativeCommandSucceeded "adb start"
            adb logcat -s Gyroflow
            Assert-NativeCommandSucceeded "adb logcat"
        }
    } finally {
        if ($job) {
            Stop-Job $job -ErrorAction SilentlyContinue
            Remove-Job $job -ErrorAction SilentlyContinue
        }
        Restore-TemporarySourceEdits
        Assert-TemporarySourceEditsRestored
    }
