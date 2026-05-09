import 'common.just'

NameSuffix := if Arch == "aarch64" { "windows-arm64" } else { "windows64" }

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Dependencies ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
install-deps:
    #!powershell
    $ProgressPreference = 'SilentlyContinue'
    $ErrorActionPreference = 'Stop'
    if ("{{Arch}}" -eq "aarch64") {
        rustup target add aarch64-pc-windows-msvc
    }

    mkdir "{{ExtDir}}" -ErrorAction SilentlyContinue
    cd {{ExtDir}}

    function Test-InnoSetupCompiler {
        param([Parameter(Mandatory=$true)] [string] $Path)
        if (-not (Test-Path -LiteralPath $Path)) { return $false }
        $Item = Get-Item -LiteralPath $Path
        foreach ($VersionText in @($Item.VersionInfo.ProductVersion, $Item.VersionInfo.FileVersion)) {
            if ($VersionText -match '(\d+)\.(\d+)\.(\d+)') {
                $Version = [version]"$($matches[1]).$($matches[2]).$($matches[3])"
                if ($Version -ge [version]"6.5.0" -and $Version -lt [version]"7.0.0") {
                    return $true
                }
            }
        }

        return (
            $Item.FullName -like "*\Inno Setup 6\ISCC.exe" -and
            $Item.VersionInfo.ProductName -eq "Inno Setup" -and
            $Item.VersionInfo.FileDescription -eq "Inno Setup Command-Line Compiler"
        )
    }

    function Get-InnoSetupCompiler {
        $Candidates = @()
        $Command = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
        if ($Command) { $Candidates += $Command.Source }
        $Candidates += @(
            "${Env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$Env:ProgramFiles\Inno Setup 6\ISCC.exe",
            "$Env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
        )

        foreach ($Candidate in $Candidates) {
            if (Test-InnoSetupCompiler $Candidate) {
                return (Resolve-Path -LiteralPath $Candidate).Path
            }
        }
        return $null
    }

    if (-not (Get-InnoSetupCompiler)) {
        $InnoInstaller = Join-Path (Resolve-Path "{{ExtDir}}") "innosetup-6.7.1.exe"
        if (-not (Test-Path -LiteralPath $InnoInstaller)) {
            echo "Downloading Inno Setup 6.7.1"
            Invoke-WebRequest -URI "https://github.com/jrsoftware/issrc/releases/download/is-6_7_1/innosetup-6.7.1.exe" -OutFile $InnoInstaller
        }
        Start-Process -FilePath $InnoInstaller -ArgumentList @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER") -Wait -WindowStyle Hidden
        if (-not (Get-InnoSetupCompiler)) {
            throw "Inno Setup 6.5+ compiler (ISCC.exe) was not found after installation"
        }
    }

    $Python = "python";
    if (Get-Command "python3.10.exe" -ErrorAction SilentlyContinue) {
        $Python = "python3.10";
    } elseif (Get-Command "python3.exe" -ErrorAction SilentlyContinue) {
        $Python = "python3";
    } elseif (Get-Command "python.exe" -ErrorAction SilentlyContinue) {
        $Python = "python";
    } else {
        if (-not (Test-Path -Path "{{ExtDir}}/WPy64-310100b3")) {
            echo "Downloading WinPython"
            wget https://github.com/winpython/winpython/releases/download/6.1.20230227/Winpython64-3.10.10.0dotb3.exe -outfile "winpython.exe"
            7z x -y winpython.exe
            del winpython.exe
        }
        $Python = resolve-path "{{ExtDir}}\WPy64-310100b3\python-3.10.10.amd64\python.exe"
    }

    # ---- Qt ----
    if (-not (Test-Path -Path "{{QtVersion}}/{{QtArch}}")) {
        echo "Downloading Qt {{QtVersion}}"
        & $Python -m pip install -U pip aqtinstall===3.2.0
        if (-not (Test-Path -Path "{{QtVersion}}/msvc2019_64")) {
            & $Python -m aqt install-qt windows desktop {{QtVersion}} win64_msvc2019_64 -m qtshadertools
        }
        if ("{{QtArch}}" -eq "msvc2019_arm64") {
            & $Python -m aqt install-qt windows desktop {{QtVersion}} win64_{{QtArch}}
        }
    }
    # ---- Qt ----

    # ---- FFmpeg ----
    if (-not (Test-Path -Path "{{FFmpeg}}")) {
        echo "Downloading {{FFmpeg}}"
        wget "https://master.dl.sourceforge.net/project/avbuild/windows-desktop/{{FFmpeg}}.7z?viasf=1" -outfile "{{FFmpeg}}.7z"
        7z x -y {{FFmpeg}}.7z
        del {{FFmpeg}}.7z
    }
    # ---- FFmpeg ----

    # ---- OpenCV ----
    if (-not (Test-Path -Path "vcpkg/installed/{{VcpkgArch}}/lib/opencv_core4.lib")) {
        echo "Downloading OpenCV"
        rm -Recurse -Force \vcpkg -ErrorAction SilentlyContinue
        if ("{{Arch}}" -eq "aarch64") {
            git clone --depth 1 --branch 2025.03.19 https://github.com/Microsoft/vcpkg.git
        } else {
            git clone --depth 1 https://github.com/Microsoft/vcpkg.git
        }
        .\vcpkg\bootstrap-vcpkg.bat -disableMetrics
        #Add-Content .\vcpkg\triplets\arm64-windows.cmake -Value  "`nset(VCPKG_CMAKE_SYSTEM_VERSION 10.0.22621.0)"
        #Add-Content .\vcpkg\triplets\arm64-windows.cmake -Value  "`nset(CMAKE_SYSTEM_VERSION 10.0.22621.0)"
        #Remove-Item -Path "C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0" -Recurse -Force
        #Remove-Item -Path "C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0" -Recurse -Force
        #Remove-Item -Path "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0" -Recurse -Force
        .\vcpkg\vcpkg install "flatbuffers:{{VcpkgArch}}" --cmake-args=-DVCPKG_BUILD_TYPE=release
        .\vcpkg\vcpkg install "opencv4[core,calib3d,opencl,thread,intrinsics]:{{VcpkgArch}}" --cmake-args=-DVCPKG_BUILD_TYPE=release
        .\vcpkg\vcpkg install "opencl:{{VcpkgArch}}" --cmake-args=-DVCPKG_BUILD_TYPE=release
        rm -Recurse -Force .\vcpkg\buildtrees
        rm -Recurse -Force .\vcpkg\downloads
    }
    # ---- OpenCV ----

    # ---- LLVM ----
    if (-not (Test-Path -Path "{{LLVMPath}}\libclang.dll")) {
        $Arch = "win64"
        if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") {
            $Arch = "woa64";
        }
        echo "Downloading LLVM"
        wget "https://github.com/llvm/llvm-project/releases/download/llvmorg-19.1.7/LLVM-19.1.7-$Arch.exe" -outfile "llvm.exe"
        7z x -y llvm.exe -ollvm
        del "llvm.exe"
    }
    # ---- LLVM ----

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Development ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
build *param:
    cargo build --release -- {{param}}

[no-cd]
build-debug *param:
    cargo build -- {{param}}

[no-cd]
run *param:
    cargo run --release -- {{param}}

[no-cd]
test *param:
    cargo test -- {{param}}

[no-cd]
test-core *param:
    cargo test -p gyroflow-core --lib {{param}}

[no-cd]
clippy *param:
    cargo clippy {{param}}

[no-cd]
debug *param:
    cargo run -- {{param}}

[no-cd]
profile *param:
    cargo run --profile profile -- {{param}}

# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~ Deployment ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
# ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

[no-cd]
deploy:
    #!powershell
    $ErrorActionPreference = 'Stop'

    function Test-InnoSetupCompiler {
        param([Parameter(Mandatory=$true)] [string] $Path)
        if (-not (Test-Path -LiteralPath $Path)) { return $false }
        $Item = Get-Item -LiteralPath $Path
        foreach ($VersionText in @($Item.VersionInfo.ProductVersion, $Item.VersionInfo.FileVersion)) {
            if ($VersionText -match '(\d+)\.(\d+)\.(\d+)') {
                $Version = [version]"$($matches[1]).$($matches[2]).$($matches[3])"
                if ($Version -ge [version]"6.5.0" -and $Version -lt [version]"7.0.0") {
                    return $true
                }
            }
        }

        return (
            $Item.FullName -like "*\Inno Setup 6\ISCC.exe" -and
            $Item.VersionInfo.ProductName -eq "Inno Setup" -and
            $Item.VersionInfo.FileDescription -eq "Inno Setup Command-Line Compiler"
        )
    }

    function Get-InnoSetupCompiler {
        $Candidates = @()
        $Command = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
        if ($Command) { $Candidates += $Command.Source }
        $Candidates += @(
            "${Env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$Env:ProgramFiles\Inno Setup 6\ISCC.exe",
            "$Env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
        )

        foreach ($Candidate in $Candidates) {
            if (Test-InnoSetupCompiler $Candidate) {
                return (Resolve-Path -LiteralPath $Candidate).Path
            }
        }
        return $null
    }

    function New-IsppStringDefine {
        param(
            [Parameter(Mandatory=$true)] [string] $Name,
            [Parameter(Mandatory=$true)] [string] $Value
        )
        $Escaped = $Value.Replace('"', '""')
        return "/D$Name=`"$Escaped`""
    }

    function Get-Sha256Hex {
        param([Parameter(Mandatory=$true)] [string] $Path)
        $ResolvedPath = (Resolve-Path -LiteralPath $Path).Path
        if (Get-Command "Get-FileHash" -ErrorAction SilentlyContinue) {
            return (Get-FileHash -Algorithm SHA256 -LiteralPath $ResolvedPath).Hash.ToLowerInvariant()
        }

        $Stream = [System.IO.File]::OpenRead($ResolvedPath)
        try {
            $Sha256 = [System.Security.Cryptography.SHA256]::Create()
            try {
                $HashBytes = $Sha256.ComputeHash($Stream)
                return -join ($HashBytes | ForEach-Object { $_.ToString("x2") })
            } finally {
                $Sha256.Dispose()
            }
        } finally {
            $Stream.Dispose()
        }
    }

    $ProjectDir = resolve-path "{{ProjectDir}}"
    cd $ProjectDir
    mkdir "$ProjectDir\_deployment\_binaries\win64" -Force

    if ("{{Arch}}" -eq "aarch64") {
        $Target = "aarch64-pc-windows-msvc"
    } else {
        $Target = "x86_64-pc-windows-msvc"
    }

    cargo build --profile deploy --target $Target

    $TargetDir   = resolve-path "{{ProjectDir}}\_deployment\_binaries\win64"
    $QtDir       = resolve-path "{{QtPath}}\.."
    $CargoTarget = resolve-path "{{ProjectDir}}\target\$Target\deploy"
    $FFmpegDir   = resolve-path "{{FFMPEG_DIR}}\bin"
    $OpenCVDir   = resolve-path "{{OpenCVPath}}\bin"

    $ffmpeg_files = Get-ChildItem "$FFmpegDir\*.dll"
    if (!$ffmpeg_files) {
        if ("{{Arch}}" -eq "aarch64") {
            $FFmpegDir = resolve-path "{{FFMPEG_DIR}}\bin\arm64"
        } else {
            $FFmpegDir = resolve-path "{{FFMPEG_DIR}}\bin\x64"
        }
    }

    # Copy Qt
    xcopy /Y "$QtDir\plugins\platforms\qwindows.dll"                                                 "$TargetDir\platforms\"
    xcopy /Y "$QtDir\plugins\iconengines\qsvgicon.dll"                                               "$TargetDir\iconengines\"
    xcopy /Y "$QtDir\plugins\imageformats\qsvg.dll"                                                  "$TargetDir\imageformats\"
    xcopy /Y "$QtDir\plugins\imageformats\qjpeg.dll"                                                 "$TargetDir\imageformats\"
    xcopy /Y "$QtDir\bin\Qt6Core.dll"                                                                "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6Gui.dll"                                                                 "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6Network.dll"                                                             "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6OpenGL.dll"                                                              "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6Qml.dll"                                                                 "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QmlCore.dll"                                                             "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QmlModels.dll"                                                           "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QmlWorkerScript.dll"                                                     "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6Quick.dll"                                                               "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickControls2.dll"                                                      "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickControls2Impl.dll"                                                  "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickControls2Material.dll"                                              "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickControls2MaterialStyleImpl.dll"                                     "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickControls2Basic.dll"                                                 "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickControls2BasicStyleImpl.dll"                                        "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickDialogs2.dll"                                                       "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickDialogs2QuickImpl.dll"                                              "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickDialogs2Utils.dll"                                                  "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickEffects.dll"                                                        "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickTemplates2.dll"                                                     "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6QuickLayouts.dll"                                                        "$TargetDir\"
    xcopy /Y "$QtDir\bin\Qt6Svg.dll"                                                                 "$TargetDir\"

    # Copy QtQuick
    xcopy /Y "$QtDir\qml\QtCore\qmldir"                                                              "$TargetDir\QtCore\"
    xcopy /Y "$QtDir\qml\QtCore\qtqmlcoreplugin.dll"                                                 "$TargetDir\QtCore\"
    xcopy /Y "$QtDir\qml\QtQml\qmldir"                                                               "$TargetDir\QtQml\"
    xcopy /Y "$QtDir\qml\QtQml\qmlplugin.dll"                                                        "$TargetDir\QtQml\"
    xcopy /Y "$QtDir\qml\QtQml\qmlmetaplugin.dll"                                                    "$TargetDir\QtQml\"
    xcopy /Y "$QtDir\qml\QtQml\Base\qmldir"                                                          "$TargetDir\QtQml\Base\"
    xcopy /Y "$QtDir\qml\QtQml\Base\qmlplugin.dll"                                                   "$TargetDir\QtQml\Base\"
    xcopy /Y "$QtDir\qml\QtQml\Models\qmldir"                                                        "$TargetDir\QtQml\Models\"
    xcopy /Y "$QtDir\qml\QtQml\Models\modelsplugin.dll"                                              "$TargetDir\QtQml\Models\"
    xcopy /Y "$QtDir\qml\QtQml\WorkerScript\qmldir"                                                  "$TargetDir\QtQml\WorkerScript\"
    xcopy /Y "$QtDir\qml\QtQml\WorkerScript\workerscriptplugin.dll"                                  "$TargetDir\QtQml\WorkerScript\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Basic\*.qml"                                               "$TargetDir\QtQuick\Controls\Basic\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Basic\qmldir"                                              "$TargetDir\QtQuick\Controls\Basic\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Basic\qtquickcontrols2basicstyleplugin.dll"                "$TargetDir\QtQuick\Controls\Basic\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Basic\impl\qmldir"                                         "$TargetDir\QtQuick\Controls\Basic\impl\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Basic\impl\qtquickcontrols2basicstyleimplplugin.dll"       "$TargetDir\QtQuick\Controls\Basic\impl\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\impl\qmldir"                                               "$TargetDir\QtQuick\Controls\impl\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\impl\qtquickcontrols2implplugin.dll"                       "$TargetDir\QtQuick\Controls\impl\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Material\*.qml"                                            "$TargetDir\QtQuick\Controls\Material\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Material\qmldir"                                           "$TargetDir\QtQuick\Controls\Material\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Material\qtquickcontrols2materialstyleplugin.dll"          "$TargetDir\QtQuick\Controls\Material\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Material\impl\*.qml"                                       "$TargetDir\QtQuick\Controls\Material\impl\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Material\impl\qmldir"                                      "$TargetDir\QtQuick\Controls\Material\impl\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\Material\impl\qtquickcontrols2materialstyleimplplugin.dll" "$TargetDir\QtQuick\Controls\Material\impl\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\qmldir"                                                    "$TargetDir\QtQuick\Controls\"
    xcopy /Y "$QtDir\qml\QtQuick\Controls\qtquickcontrols2plugin.dll"                                "$TargetDir\QtQuick\Controls\"
    xcopy /Y "$QtDir\qml\QtQuick\Dialogs\qmldir"                                                     "$TargetDir\QtQuick\Dialogs\"
    xcopy /Y "$QtDir\qml\QtQuick\Dialogs\qtquickdialogsplugin.dll"                                   "$TargetDir\QtQuick\Dialogs\"
    xcopy /Y "$QtDir\qml\QtQuick\Dialogs\quickimpl\qml\+Material\*.qml"                              "$TargetDir\QtQuick\Dialogs\quickimpl\qml\+Material\"
    xcopy /Y "$QtDir\qml\QtQuick\Dialogs\quickimpl\qml\*.qml"                                        "$TargetDir\QtQuick\Dialogs\quickimpl\qml\"
    xcopy /Y "$QtDir\qml\QtQuick\Dialogs\quickimpl\qmldir"                                           "$TargetDir\QtQuick\Dialogs\quickimpl\"
    xcopy /Y "$QtDir\qml\QtQuick\Dialogs\quickimpl\qtquickdialogs2quickimplplugin.dll"               "$TargetDir\QtQuick\Dialogs\quickimpl\"
    xcopy /Y "$QtDir\qml\QtQuick\Effects\effectsplugin.dll"                                          "$TargetDir\QtQuick\Effects\"
    xcopy /Y "$QtDir\qml\QtQuick\Effects\qmldir"                                                     "$TargetDir\QtQuick\Effects\"
    xcopy /Y "$QtDir\qml\QtQuick\Layouts\qmldir"                                                     "$TargetDir\QtQuick\Layouts\"
    xcopy /Y "$QtDir\qml\QtQuick\Layouts\qquicklayoutsplugin.dll"                                    "$TargetDir\QtQuick\Layouts\"
    xcopy /Y "$QtDir\qml\QtQuick\qmldir"                                                             "$TargetDir\QtQuick\"
    xcopy /Y "$QtDir\qml\QtQuick\qtquick2plugin.dll"                                                 "$TargetDir\QtQuick\"
    xcopy /Y "$QtDir\qml\QtQuick\Templates\qmldir"                                                   "$TargetDir\QtQuick\Templates\"
    xcopy /Y "$QtDir\qml\QtQuick\Templates\qtquicktemplates2plugin.dll"                              "$TargetDir\QtQuick\Templates\"
    xcopy /Y "$QtDir\qml\QtQuick\Window\qmldir"                                                      "$TargetDir\QtQuick\Window\"
    xcopy /Y "$QtDir\qml\QtQuick\Window\quickwindowplugin.dll"                                       "$TargetDir\QtQuick\Window\"

    # Copy ffmpeg
    xcopy /Y "$FFmpegDir\avcodec-*.dll"     "$TargetDir\"
    xcopy /Y "$FFmpegDir\avdevice-*.dll"    "$TargetDir\"
    xcopy /Y "$FFmpegDir\avfilter-*.dll"    "$TargetDir\"
    xcopy /Y "$FFmpegDir\avformat-*.dll"    "$TargetDir\"
    xcopy /Y "$FFmpegDir\avutil-*.dll"      "$TargetDir\"
    xcopy /Y "$FFmpegDir\swresample-*.dll"  "$TargetDir\"
    xcopy /Y "$FFmpegDir\swscale-*.dll"     "$TargetDir\"
    # xcopy /Y "$FFmpegDir\postproc-*.dll"  "$TargetDir\"

    # Copy OpenCV
    xcopy /Y "$OpenCVDir\opencv_calib*.dll"      "$TargetDir\"
    xcopy /Y "$OpenCVDir\opencv_cor*.dll"        "$TargetDir\"
    xcopy /Y "$OpenCVDir\opencv_features*.dll"   "$TargetDir\"
    xcopy /Y "$OpenCVDir\opencv_flan*.dll"       "$TargetDir\"
    xcopy /Y "$OpenCVDir\opencv_imgpro*.dll"     "$TargetDir\"
    xcopy /Y "$OpenCVDir\opencv_vide*.dll"       "$TargetDir\"
    xcopy /Y "$OpenCVDir\zlib*.dll"              "$TargetDir\"
    del "$TargetDir\opencv_*videoio*"

    # Copy Gyroflow
    xcopy /Y "$CargoTarget\mdk.dll"                                      "$TargetDir\"
    xcopy /Y "$CargoTarget\mdk-braw.dll"                                 "$TargetDir\"
    xcopy /Y "$CargoTarget\mdk-r3d.dll"                                  "$TargetDir\"
    echo F | xcopy /Y "$CargoTarget\gyroflow.exe"                        "$TargetDir\Gyroflow.exe"
    xcopy /Y "$ProjectDir\_deployment\windows\Gyroflow_with_console.bat" "$TargetDir\"
    # xcopy /Y /E "$ProjectDir\resources\camera_presets\*"                 "$TargetDir\camera_presets\"
    mkdir "$TargetDir\camera_presets\" -Force
    echo F | xcopy /Y "$ProjectDir\resources\camera_presets\profiles.cbor.gz"  "$TargetDir\camera_presets\profiles.cbor.gz"
    # Niyien: camera_db（由 src/core/build.rs 从 niyien-lens-data release 下载到 resources/）
    mkdir "$TargetDir\camera_db\" -Force
    xcopy /Y /E "$ProjectDir\resources\camera_db\*" "$TargetDir\camera_db\"

    # Other
    xcopy /Y "$QtDir\bin\d3dcompiler_47.dll"                             "$TargetDir\"
    if ("{{Arch}}" -eq "aarch64") {
        xcopy /Y "C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\arm64\d3dcompiler_47.dll" "$TargetDir\"
    } else {
        xcopy /Y "C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\x64\d3dcompiler_47.dll" "$TargetDir\"
    }

    # VC Redist
    if ("{{Arch}}" -eq "x86_64") {
        xcopy /Y "C:\Windows\System32\vcruntime140.dll"     "$TargetDir\"
        xcopy /Y "C:\Windows\System32\vcruntime140_1.dll"   "$TargetDir\"
        xcopy /Y "C:\Windows\System32\msvcp140.dll"         "$TargetDir\"
        xcopy /Y "C:\Windows\System32\msvcp140_1.dll"       "$TargetDir\"
        xcopy /Y "C:\Windows\System32\msvcp140_2.dll"       "$TargetDir\"
    }
    if (-not $Env:SDK_BASE) { $Env:SDK_BASE = "https://www.niyien.com/api/sdk" }

    # Vulkan shaderc
    if ("{{Arch}}" -eq "x86_64") {
        Invoke-WebRequest -URI "$Env:SDK_BASE/shaderc_shared.dll" -OutFile "$TargetDir\shaderc_shared.dll"
    }

    # Bundle BRAW and RED SDK so users don't hit a runtime download.
    # Mirrors upstream gyroflow `_scripts/windows.just::bundle`; placed before
    # the zip step here so the SDK DLLs ship inside the release archive.
    if ("{{Arch}}" -eq "x86_64") {
        if (-not (Test-Path -Path "$TargetDir\REDR3D-x64.dll")) {
            Invoke-WebRequest -URI "$Env:SDK_BASE/RED_SDK_Windows_9.1.2.tar.gz" -OutFile tmp.tar.gz
            7z x -aoa tmp.tar.gz
            7z x -aoa -o"$TargetDir" tmp.tar
            Remove-Item tmp.tar.gz, tmp.tar -Force
        }
        if (-not (Test-Path -Path "$TargetDir\BlackmagicRawAPI.dll")) {
            Invoke-WebRequest -URI "$Env:SDK_BASE/Blackmagic_RAW_SDK_Windows_5.0.0.tar.gz" -OutFile tmp.tar.gz
            7z x -aoa tmp.tar.gz
            7z x -aoa -o"$TargetDir" tmp.tar
            Remove-Item tmp.tar.gz, tmp.tar -Force
        }
    }

    $PackageName = "{{ArtifactPrefix}}-{{NameSuffix}}.zip"
    $PackagePath = "$ProjectDir\_deployment\_binaries\$PackageName"
    7z a $PackagePath $TargetDir\*

    if ("{{Arch}}" -eq "x86_64") {
        # Current release matrix produces gyroflow-niyien-windows64-setup.exe.
        $SetupBaseName = "{{ArtifactPrefix}}-{{NameSuffix}}-setup"
        $SetupPath = "$ProjectDir\_deployment\_binaries\$SetupBaseName.exe"
        $PackageSha256 = Get-Sha256Hex $PackagePath
        $PackageSize = (Get-Item -LiteralPath $PackagePath).Length
        $PackageExternalSize = [int64]((Get-ChildItem -Path $TargetDir -Recurse -File | Measure-Object -Property Length -Sum).Sum)
        $AppVersion = python "$ProjectDir\_scripts\niyien_version.py" base --repo-root "$ProjectDir"
        $AppFileVersion = python "$ProjectDir\_scripts\niyien_version.py" file --repo-root "$ProjectDir"
        $CanonicalVersion = python "$ProjectDir\_scripts\niyien_version.py" canonical --repo-root "$ProjectDir"
        $PackageTag = $Env:NIYIEN_SETUP_PACKAGE_TAG
        if (-not $PackageTag) {
            if ($Env:GITHUB_REF_TYPE -eq "tag" -and $Env:GITHUB_REF_NAME) {
                $PackageTag = $Env:GITHUB_REF_NAME
            } elseif ($Env:GITHUB_RUN_ID) {
                $PackageTag = "run-$($Env:GITHUB_RUN_ID)"
            } elseif ($CanonicalVersion.StartsWith("v")) {
                $PackageTag = $CanonicalVersion
            } else {
                $PackageTag = "v$CanonicalVersion"
            }
        }
        $DefaultPackageUrl = $Env:NIYIEN_SETUP_PACKAGE_URL
        if (-not $DefaultPackageUrl) {
            $DefaultPackageUrl = "https://www.niyien.com/api/download/app/$PackageTag/$PackageName"
        }
        Write-Host "Windows setup package URL: $DefaultPackageUrl"

        $Iscc = Get-InnoSetupCompiler
        if (-not $Iscc) {
            throw "Inno Setup 6.5+ compiler (ISCC.exe) is required to build $SetupBaseName.exe. Run `just install-deps` first."
        }

        Remove-Item -LiteralPath $SetupPath -Force -ErrorAction SilentlyContinue
        $IsccArgs = @(
            (New-IsppStringDefine "AppVersion" $AppVersion),
            "/DAppFileVersion=$AppFileVersion",
            (New-IsppStringDefine "PackageFilename" $PackageName),
            (New-IsppStringDefine "PackageUrl" $DefaultPackageUrl),
            (New-IsppStringDefine "PackageSha256" $PackageSha256),
            "/DPackageSize=$PackageSize",
            "/DPackageExternalSize=$PackageExternalSize",
            (New-IsppStringDefine "OutputBaseFilename" $SetupBaseName),
            "$ProjectDir\_deployment\windows\gyroflow-niyien-setup.iss"
        )
        & $Iscc @IsccArgs
        if (-not (Test-Path -LiteralPath $SetupPath)) {
            throw "Inno Setup did not produce $SetupPath"
        }
    } else {
        echo "Skipping Windows setup.exe build for Arch={{Arch}}"
    }

[no-cd]
bundle:
    #!powershell
    $ProgressPreference = 'SilentlyContinue'
    $ErrorActionPreference = 'Stop'
    function Invoke-Environment { param([Parameter(Mandatory=$true)] [string] $Command)
        $Command = "`"" + $Command + "`""
        cmd /c "$Command > nul 2>&1 && set" | . { process {
            if ($_ -match '^([^=]+)=(.*)') { [System.Environment]::SetEnvironmentVariable($matches[1], $matches[2]) }
        }}
    }

    $TargetDir  = resolve-path "{{ProjectDir}}\_deployment\_binaries\win64"
    $ProjectDir = resolve-path "{{ProjectDir}}"
    cd $ProjectDir\_deployment\windows\

    if (-Not (Get-Command "makeappx.exe" -ErrorAction SilentlyContinue)) {
        $VSPath = "`"${Env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe`" -latest -property installationPath"
        $VSPath = iex "& $VSPath"
        Invoke-Environment "$VSPath\VC\Auxiliary\Build\vcvars64.bat"
    }

    # Bundle BRAW and RED SDK
    if ("{{Arch}}" -eq "x86_64") {
        if (-not $Env:SDK_BASE) { $Env:SDK_BASE = "https://www.niyien.com/api/sdk" }
        if (-not (Test-Path -Path "$TargetDir\REDR3D-x64.dll")) {
            Invoke-WebRequest -URI "$Env:SDK_BASE/RED_SDK_Windows_9.1.2.tar.gz" -OutFile tmp.tar.gz
            7z x tmp.tar.gz
            7z x -o"$TargetDir\" tmp.tar
            Remove-Item -Path tmp.ta* -Force
        }
        if (-not (Test-Path -Path "$TargetDir\BlackmagicRawAPI.dll")) {
            Invoke-WebRequest -URI "$Env:SDK_BASE/Blackmagic_RAW_SDK_Windows_5.0.0.tar.gz" -OutFile tmp.tar.gz
            7z x tmp.tar.gz
            7z x -o"$TargetDir\" tmp.tar
            Remove-Item -Path tmp.ta* -Force
        }
    }

    # Generate self-signed key
    # $cert = New-SelfSignedCertificate -Subject "CN=10F1B828-8298-48D5-8164-662492DBDECB" -DnsName 29160AdrianRoss.Gyroflow -Type CodeSigning -CertStoreLocation Cert:\CurrentUser\My
    # $CertPassword = ConvertTo-SecureString -String "$Env:MS_STORE_SIGNING_PASS" -Force -AsPlainText
    # Export-PfxCertificate -Cert "cert:\CurrentUser\My\$($cert.Thumbprint)" -FilePath "msix_signing_key.pfx" -Password $CertPassword

    # Build .msi installer
    # msbuild.exe /p:Configuration=Release -Restore
    # MsixPackagingTool.exe create-package --template installer_msix.xml

    # Build .msix
    Remove-Item -Path "..\_binaries\{{ArtifactPrefix}}-{{NameSuffix}}.msix" -ErrorAction SilentlyContinue
    mkdir -Force "bundle"
    cp -Recurse -Force $TargetDir\* bundle\
    cp -Recurse -Force Assets bundle\Assets
    cp -Force AppxManifest.xml bundle\

    $AppFileVersion = python "$ProjectDir\_scripts\niyien_version.py" file --repo-root "$ProjectDir"
    (Get-Content bundle\AppxManifest.xml) -replace 'Identity Version="[0-9\.]+"', "Identity Version=`"$AppFileVersion`"" | Out-File bundle\AppxManifest.xml -Encoding Utf8

    if ("{{Arch}}" -eq "aarch64") {
        (Get-Content bundle\AppxManifest.xml) -replace 'ProcessorArchitecture="x64"', 'ProcessorArchitecture="arm64"' | Out-File bundle\AppxManifest.xml -Encoding Utf8
    }

    makepri.exe createconfig /cf priconfig.xml /dq en-us
    makepri.exe new /pr bundle\Assets\ /cf priconfig.xml /mn bundle\AppxManifest.xml /of bundle\resources.pri /o

    makeappx.exe pack /d bundle /p ..\_binaries\{{ArtifactPrefix}}-{{NameSuffix}}.msix
    signtool.exe sign /f msix_signing_key.pfx /p $Env:MS_STORE_SIGNING_PASS /fd sha256 ..\_binaries\{{ArtifactPrefix}}-{{NameSuffix}}.msix

    # To bundle
    # makeappx.exe bundle /d bundle /p Gyroflow.msixbundle
    # signtool.exe sign /fd sha256 /a /f msix_signing_key.pfx /p $Env:MS_STORE_SIGNING_PASS Gyroflow.msixbundle

    Remove-Item -Path "priconfig.xml" -ErrorAction SilentlyContinue
    Remove-Item -Path "bundle" -Recurse -ErrorAction SilentlyContinue
