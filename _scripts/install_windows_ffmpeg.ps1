# SPDX-License-Identifier: GPL-3.0-or-later
param(
    [Parameter(Mandatory = $true)][string]$ExtDir,
    [Parameter(Mandatory = $true)][string]$PackageName,
    [Parameter(Mandatory = $true)][string]$Url,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$Sha256
)

$ErrorActionPreference = 'Stop'
if ($PackageName -notmatch '^ffmpeg-[a-zA-Z0-9.-]+$') {
    throw 'Invalid FFmpeg package directory name'
}
$ExpectedHash = $Sha256.ToLowerInvariant()
$Root = [IO.Path]::GetFullPath($ExtDir)
$SdkDir = Join-Path $Root $PackageName
$Archive = Join-Path $Root "$PackageName.7z"
$Marker = Join-Path $SdkDir '.gyroflow-package-sha256'
$PatchInfo = Join-Path $SdkDir '.gyroflow-hevc-tile-patch.json'
$RequiredFiles = @(
    'include/libavcodec/avcodec.h',
    'include/libavutil/avutil.h',
    'bin/ffmpeg.exe',
    'bin/avcodec-62.dll',
    'bin/avformat-62.dll',
    'bin/avutil-60.dll',
    'bin/avdevice-62.dll',
    'bin/avfilter-11.dll',
    'bin/swresample-6.dll',
    'bin/swscale-9.dll',
    'lib/avcodec.lib',
    'lib/avformat.lib',
    'lib/avutil.lib',
    'lib/avdevice.lib',
    'lib/avfilter.lib',
    'lib/swresample.lib',
    'lib/swscale.lib',
    '.gyroflow-hevc-tile-patch.json'
)

function Test-SdkFiles {
    foreach ($RelativePath in $RequiredFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $SdkDir $RelativePath) -PathType Leaf)) { return $false }
    }
    try {
        $Info = [IO.File]::ReadAllText($PatchInfo) | ConvertFrom-Json
        if ($Info.avcodec_major -ne 62 -or -not $Info.tile_interleave_available) { return $false }
        $DllHash = (Get-FileHash -LiteralPath (Join-Path $SdkDir 'bin/avcodec-62.dll') -Algorithm SHA256).Hash
        return $DllHash.ToLowerInvariant() -eq $Info.dll_sha256
    } catch {
        return $false
    }
}

if ((Test-Path -LiteralPath $Marker) -and ([IO.File]::ReadAllText($Marker).Trim() -eq $ExpectedHash) -and (Test-SdkFiles)) {
    Write-Host "Verified FFmpeg SDK: $PackageName"
    exit 0
}

New-Item -ItemType Directory -Path $Root -Force | Out-Null
$ArchiveValid = (Test-Path -LiteralPath $Archive) -and ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant() -eq $ExpectedHash)
if (-not $ArchiveValid) {
    $Download = "$Archive.download"
    & curl.exe -fL --retry 3 --connect-timeout 30 -o $Download $Url
    if ($LASTEXITCODE -ne 0) { throw "Failed to download $PackageName" }
    if ((Get-FileHash -LiteralPath $Download -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ExpectedHash) {
        throw "SHA-256 mismatch for $PackageName"
    }
    Move-Item -LiteralPath $Download -Destination $Archive -Force
}

# An interrupted extraction must not retain a successful installation marker.
if (Test-Path -LiteralPath $Marker) { Remove-Item -LiteralPath $Marker }
& 7z x -y $Archive "-o$Root"
if ($LASTEXITCODE -ne 0) { throw "Failed to extract $PackageName" }
if (-not (Test-SdkFiles)) { throw "Incomplete or mismatched FFmpeg SDK: $PackageName" }
$StartInfo = New-Object System.Diagnostics.ProcessStartInfo
$StartInfo.FileName = Join-Path $SdkDir 'bin/ffmpeg.exe'
$StartInfo.Arguments = '-hide_banner -h decoder=hevc'
$StartInfo.UseShellExecute = $false
$StartInfo.CreateNoWindow = $true
$StartInfo.RedirectStandardOutput = $true
$StartInfo.RedirectStandardError = $true
$Probe = New-Object System.Diagnostics.Process
$Probe.StartInfo = $StartInfo
try {
    if (-not $Probe.Start()) { throw 'Failed to start FFmpeg SDK verification' }
    $OutputTask = $Probe.StandardOutput.ReadToEndAsync()
    $ErrorTask = $Probe.StandardError.ReadToEndAsync()
    if (-not $Probe.WaitForExit(30000)) {
        $Probe.Kill()
        throw 'FFmpeg SDK verification timed out'
    }
    $DecoderOptions = $OutputTask.GetAwaiter().GetResult() + $ErrorTask.GetAwaiter().GetResult()
    if ($Probe.ExitCode -ne 0 -or $DecoderOptions -notmatch 'tile_interleave') {
        throw "FFmpeg SDK does not expose the HEVC tile decoder option: $PackageName"
    }
} finally {
    $Probe.Dispose()
}
[IO.File]::WriteAllText($Marker, $ExpectedHash + "`n")
Write-Host "Installed verified FFmpeg SDK: $PackageName"
