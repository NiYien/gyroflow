#ifndef AppVersion
#define AppVersion "0.0.0"
#endif

#ifndef AppFileVersion
#define AppFileVersion "0.0.0.0"
#endif

#ifndef PackageFilename
#define PackageFilename "gyroflow-niyien-windows64.zip"
#endif

#ifndef PackageUrl
#define PackageUrl ""
#endif

#ifndef PackageSha256
#define PackageSha256 ""
#endif

#ifndef PackageSize
#define PackageSize "0"
#endif

#ifndef PackageExternalSize
#define PackageExternalSize "0"
#endif

#ifndef OutputBaseFilename
#define OutputBaseFilename "gyroflow-niyien-windows64-setup"
#endif

#define AppDisplayName "Gyroflow(NiYien)"

[Setup]
AppId={{8890709B-FA77-4CFB-9779-F06D6E7B7296}
AppName={#AppDisplayName}
AppVersion={#AppVersion}
AppVerName={#AppDisplayName} {#AppVersion}
AppPublisher=Niyien
AppPublisherURL=https://www.niyien.com/
AppSupportURL=https://www.niyien.com/
AppUpdatesURL=https://www.niyien.com/
DefaultDirName={localappdata}\Programs\{#AppDisplayName}
DefaultGroupName={#AppDisplayName}
AllowNoIcons=yes
DisableProgramGroupPage=no
PrivilegesRequired=lowest
OutputDir=..\_binaries
OutputBaseFilename={#OutputBaseFilename}
SetupIconFile=..\..\resources\app_icon.ico
UninstallDisplayIcon={app}\Gyroflow.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=no
RestartApplications=no
SetupLogging=yes
ArchiveExtraction=full
VersionInfoVersion={#AppFileVersion}
VersionInfoCompany=Niyien
VersionInfoDescription={#AppDisplayName} web installer
VersionInfoProductName={#AppDisplayName}
VersionInfoProductVersion={#AppFileVersion}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "arabic"; MessagesFile: "compiler:Languages\Arabic.isl"
Name: "armenian"; MessagesFile: "compiler:Languages\Armenian.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "bulgarian"; MessagesFile: "compiler:Languages\Bulgarian.isl"
Name: "catalan"; MessagesFile: "compiler:Languages\Catalan.isl"
Name: "corsican"; MessagesFile: "compiler:Languages\Corsican.isl"
Name: "czech"; MessagesFile: "compiler:Languages\Czech.isl"
Name: "danish"; MessagesFile: "compiler:Languages\Danish.isl"
Name: "dutch"; MessagesFile: "compiler:Languages\Dutch.isl"
Name: "finnish"; MessagesFile: "compiler:Languages\Finnish.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "hebrew"; MessagesFile: "compiler:Languages\Hebrew.isl"
Name: "hungarian"; MessagesFile: "compiler:Languages\Hungarian.isl"
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "norwegian"; MessagesFile: "compiler:Languages\Norwegian.isl"
Name: "polish"; MessagesFile: "compiler:Languages\Polish.isl"
Name: "portuguese"; MessagesFile: "compiler:Languages\Portuguese.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "slovak"; MessagesFile: "compiler:Languages\Slovak.isl"
Name: "slovenian"; MessagesFile: "compiler:Languages\Slovenian.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "swedish"; MessagesFile: "compiler:Languages\Swedish.isl"
Name: "tamil"; MessagesFile: "compiler:Languages\Tamil.isl"
Name: "thai"; MessagesFile: "compiler:Languages\Thai.isl"
Name: "turkish"; MessagesFile: "compiler:Languages\Turkish.isl"
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl"
Name: "zh_CN"; MessagesFile: "compiler:Default.isl,languages\ChineseSimplified.isl"
Name: "zh_TW"; MessagesFile: "compiler:Default.isl,languages\ChineseTraditional.isl"

[CustomMessages]
SetupDownloadTitle=Downloading Gyroflow(NiYien)
SetupDownloadDescription=Please wait while setup downloads the application package.
SetupMissingPackageUrl=Missing package URL. Provide /PACKAGEURL=<zip_url> or build setup with PackageUrl.
SetupMissingPackageSha256=Missing package SHA256. Provide /PACKAGESHA256=<zip_sha256> or build setup with PackageSha256.
SetupDownloadVerifyFailed=Failed to download or verify Gyroflow(NiYien) package.
SetupMissingPackageFile=Local package file was not found.
SetupPackageFileVerifyFailed=Failed to verify local Gyroflow(NiYien) package.
SetupVerifyTitle=Verifying Gyroflow(NiYien)
SetupVerifyDescription=Please wait while setup verifies the integrity of the application package.
SetupVerifyProgress=Verifying package integrity (SHA-256)...

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; Extract the downloaded archive natively via Inno's full ArchiveExtraction (is7z engine; zip-capable).
; full is required for .zip (enhanced uses is7zxr.dll which fails on .zip with "Cannot get class object").
; This shows Inno's own extraction progress and avoids the slow, progress-less PowerShell Expand-Archive.
Source: "{tmp}\{#PackageFilename}"; DestDir: "{app}"; ExternalSize: {#PackageExternalSize}; Flags: external extractarchive recursesubdirs createallsubdirs ignoreversion; Check: PackageWasDownloaded

[Icons]
Name: "{userprograms}\{#AppDisplayName}"; Filename: "{app}\Gyroflow.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\{#AppDisplayName}"; Filename: "{app}\Gyroflow.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\Gyroflow.exe"; Description: "{cm:LaunchProgram,{#AppDisplayName}}"; Flags: nowait postinstall skipifsilent; Check: ShouldShowLaunchTask
Filename: "{app}\Gyroflow.exe"; Flags: nowait skipifsilent; Check: ShouldLaunchFromSwitch

[UninstallDelete]
Type: dirifempty; Name: "{app}"

[Code]
// Command-line switches: /UPDATE=1 /WAITHANDLE=<handle> /WAITPID=<pid> /WAITSTART=<filetime_hex> /DIR=<install_dir> /PACKAGEFILE=<local_zip> /PACKAGEURL=<zip_url> /PACKAGESHA256=<zip_sha256> /PACKAGESIZE=<zip_size> /LAUNCH=1
type
  TWinFileTime = record
    dwLowDateTime: LongWord;
    dwHighDateTime: LongWord;
  end;

const
  SYNCHRONIZE = $00100000;
  PROCESS_QUERY_LIMITED_INFORMATION = $1000;
  WAIT_FAILED = $FFFFFFFF;
  INFINITE = $FFFFFFFF;
  PROV_RSA_AES = 24;
  CRYPT_VERIFYCONTEXT = $F0000000;
  CALG_SHA_256 = $0000800C;
  HP_HASHVAL = 2;
  SHA256_CHUNK_SIZE = 1048576;
  GENERIC_READ = $80000000;
  FILE_SHARE_READ = $00000001;
  OPEN_EXISTING = 3;
  INVALID_HANDLE_VALUE = $FFFFFFFF;

var
  DownloadPage: TDownloadWizardPage;
  VerifyPage: TOutputProgressWizardPage;
  IsUpdateMode: Boolean;
  PackageWasFetched: Boolean;
  LaunchAfterInstall: Boolean;
  ActiveInstallDir: String;
  ActivePackageUrl: String;
  ActivePackageFile: String;
  ActivePackageSha256: String;
  ActivePackageSize: Int64;
  WaitHandleValue: String;
  WaitPidValue: String;
  WaitStartValue: String;

function WaitForSingleObject(hHandle: LongWord; dwMilliseconds: LongWord): LongWord;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(hObject: LongWord): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function OpenProcess(dwDesiredAccess: LongWord; bInheritHandle: Boolean; dwProcessId: LongWord): LongWord;
  external 'OpenProcess@kernel32.dll stdcall';
function GetProcessTimes(hProcess: LongWord; var lpCreationTime: TWinFileTime; var lpExitTime: TWinFileTime; var lpKernelTime: TWinFileTime; var lpUserTime: TWinFileTime): Boolean;
  external 'GetProcessTimes@kernel32.dll stdcall';
function CryptAcquireContext(var phProv: LongWord; pszContainer: LongWord; pszProvider: LongWord; dwProvType: LongWord; dwFlags: LongWord): Boolean;
  external 'CryptAcquireContextW@advapi32.dll stdcall';
function CryptCreateHash(hProv: LongWord; Algid: LongWord; hKey: LongWord; dwFlags: LongWord; var phHash: LongWord): Boolean;
  external 'CryptCreateHash@advapi32.dll stdcall';
function CryptHashData(hHash: LongWord; const pbData: array of Byte; dwDataLen: LongWord; dwFlags: LongWord): Boolean;
  external 'CryptHashData@advapi32.dll stdcall';
// NB: CryptoAPI byte-array params must NOT be 'var array of Byte' — Inno passes a
// bad pointer for 'var', causing ERROR_NOACCESS. Plain 'array of Byte' passes the
// raw data pointer (writable), which is what these out-buffers need.
function CryptGetHashParam(hHash: LongWord; dwParam: LongWord; pbData: array of Byte; var pdwDataLen: LongWord; dwFlags: LongWord): Boolean;
  external 'CryptGetHashParam@advapi32.dll stdcall';
function CryptDestroyHash(hHash: LongWord): Boolean;
  external 'CryptDestroyHash@advapi32.dll stdcall';
function CryptReleaseContext(hProv: LongWord; dwFlags: LongWord): Boolean;
  external 'CryptReleaseContext@advapi32.dll stdcall';
// File reading via Win32 (TStream.Read is not usable for byte buffers in Pascal Script).
function CreateFileW(lpFileName: String; dwDesiredAccess: LongWord; dwShareMode: LongWord; lpSecurityAttributes: LongWord; dwCreationDisposition: LongWord; dwFlagsAndAttributes: LongWord; hTemplateFile: LongWord): LongWord;
  external 'CreateFileW@kernel32.dll stdcall';
function ReadFile(hFile: LongWord; lpBuffer: array of Byte; nNumberOfBytesToRead: LongWord; var lpNumberOfBytesRead: LongWord; lpOverlapped: LongWord): Boolean;
  external 'ReadFile@kernel32.dll stdcall';

function StartsWithText(const Value, Prefix: String): Boolean;
begin
  Result := Pos(UpperCase(Prefix), UpperCase(Value)) = 1;
end;

function GetSwitchValue(const Name, DefaultValue: String): String;
var
  I: Integer;
  Param: String;
  Prefix: String;
begin
  Result := DefaultValue;
  Prefix := '/' + UpperCase(Name) + '=';
  for I := 1 to ParamCount do
  begin
    Param := ParamStr(I);
    if StartsWithText(Param, Prefix) then
    begin
      Result := Copy(Param, Length(Prefix) + 1, Length(Param));
      Exit;
    end;
  end;
end;

function HasSwitch(const Name: String): Boolean;
var
  I: Integer;
  Param: String;
  Flag: String;
begin
  Result := False;
  Flag := '/' + UpperCase(Name);
  for I := 1 to ParamCount do
  begin
    Param := UpperCase(ParamStr(I));
    if (Param = Flag) or StartsWithText(Param, Flag + '=') then
    begin
      Result := True;
      Exit;
    end;
  end;
end;

function IsSwitchEnabled(const Name, DefaultValue: String): Boolean;
var
  Value: String;
begin
  Value := UpperCase(GetSwitchValue(Name, DefaultValue));
  Result := (Value <> '') and (Value <> '0') and (Value <> 'FALSE') and (Value <> 'NO');
end;

function LongWordToHex(Value: LongWord): String;
var
  I: Integer;
  Nibble: LongWord;
  HexDigits: String;
begin
  Result := '';
  HexDigits := '0123456789ABCDEF';
  for I := 7 downto 0 do
  begin
    Nibble := (Value shr (I * 4)) and $F;
    Result := Result + Copy(HexDigits, Integer(Nibble) + 1, 1);
  end;
end;

function FileTimeToHex(const Value: TWinFileTime): String;
begin
  Result := LongWordToHex(Value.dwHighDateTime) + LongWordToHex(Value.dwLowDateTime);
end;

function ParseHandle(const Value: String): LongWord;
var
  Parsed: Int64;
begin
  Parsed := StrToInt64Def(Value, 0);
  if Parsed < 0 then
    Parsed := 0;
  Result := LongWord(Parsed);
end;

procedure WaitForInheritedHandle(const Value: String);
var
  Handle: LongWord;
  WaitResult: LongWord;
begin
  Handle := ParseHandle(Value);
  if Handle = 0 then
  begin
    Log('Ignoring empty /WAITHANDLE value.');
    Exit;
  end;

  Log('Waiting for inherited /WAITHANDLE.');
  WaitResult := WaitForSingleObject(Handle, INFINITE);
  if WaitResult = WAIT_FAILED then
    Log('WaitForSingleObject(/WAITHANDLE) failed; continuing installation.')
  else
    Log('Finished waiting for /WAITHANDLE.');
  CloseHandle(Handle);
end;

procedure WaitForPidWithStartTime(const PidValue, StartValue: String);
var
  Pid: Int64;
  ProcessHandle: LongWord;
  CreationTime: TWinFileTime;
  ExitTime: TWinFileTime;
  KernelTime: TWinFileTime;
  UserTime: TWinFileTime;
  CreationHex: String;
begin
  if (PidValue = '') or (StartValue = '') then
  begin
    if PidValue <> '' then
      Log('Ignoring bare /WAITPID without /WAITSTART to avoid PID reuse.');
    Exit;
  end;

  Pid := StrToInt64Def(PidValue, 0);
  if Pid <= 0 then
  begin
    Log('Ignoring invalid /WAITPID value.');
    Exit;
  end;

  ProcessHandle := OpenProcess(SYNCHRONIZE or PROCESS_QUERY_LIMITED_INFORMATION, False, LongWord(Pid));
  if ProcessHandle = 0 then
  begin
    Log('OpenProcess(/WAITPID) failed; process may already be gone.');
    Exit;
  end;

  try
    if not GetProcessTimes(ProcessHandle, CreationTime, ExitTime, KernelTime, UserTime) then
    begin
      Log('GetProcessTimes(/WAITPID) failed; skipping wait.');
      Exit;
    end;

    CreationHex := FileTimeToHex(CreationTime);
    if CompareText(CreationHex, WaitStartValue) <> 0 then
    begin
      Log('Skipping /WAITPID because /WAITSTART does not match process creation time.');
      Exit;
    end;

    Log('Waiting for validated /WAITPID target.');
    WaitForSingleObject(ProcessHandle, INFINITE);
    Log('Finished waiting for /WAITPID target.');
  finally
    CloseHandle(ProcessHandle);
  end;
end;

procedure WaitForUpdateTarget;
begin
  if not IsUpdateMode then
    Exit;

  if WaitHandleValue <> '' then
  begin
    WaitForInheritedHandle(WaitHandleValue);
    Exit;
  end;

  WaitForPidWithStartTime(WaitPidValue, WaitStartValue);
end;

function OnDownloadProgress(const Url, Filename: String; const Progress, ProgressMax: Int64): Boolean;
var
  MaxValue: Int64;
  Percent: Integer;
begin
  Result := True;
  MaxValue := ProgressMax;
  if (MaxValue <= 0) and (ActivePackageSize > 0) then
    MaxValue := ActivePackageSize;

  if MaxValue > 0 then
  begin
    Percent := Integer((Progress * 100) div MaxValue);
    if Percent > 100 then
      Percent := 100;
    DownloadPage.SetProgress(Percent, 100);
  end
  else
    DownloadPage.SetProgress(0, 0);
end;

// Compute SHA-256 of a file in 1 MB chunks so a real progress bar can be shown.
// GetSHA256OfFile is a single blocking call with no progress; CryptoAPI lets us
// feed the hash incrementally and update VerifyPage per chunk.
function ComputeSha256WithProgress(const FileName: String): String;
var
  hFile, Prov, Hash: LongWord;
  Buffer: array of Byte;
  BytesRead: LongWord;
  HashBytes: array of Byte;
  HashLen: LongWord;
  Total, Done: Integer;
  I: Integer;
begin
  Result := '';
  hFile := INVALID_HANDLE_VALUE;
  Prov := 0;
  Hash := 0;
  try
    hFile := CreateFileW(FileName, GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
    if hFile = INVALID_HANDLE_VALUE then
      RaiseException('CreateFile failed');
    if not CryptAcquireContext(Prov, 0, 0, PROV_RSA_AES, CRYPT_VERIFYCONTEXT) then
      RaiseException('CryptAcquireContext failed');
    if not CryptCreateHash(Prov, CALG_SHA_256, 0, 0, Hash) then
      RaiseException('CryptCreateHash failed');

    if not FileSize(FileName, Total) then
      Total := 0;
    Done := 0;
    SetLength(Buffer, SHA256_CHUNK_SIZE);
    VerifyPage.SetProgress(0, 100);
    repeat
      if not ReadFile(hFile, Buffer, SHA256_CHUNK_SIZE, BytesRead, 0) then
        RaiseException('ReadFile failed');
      if BytesRead > 0 then
      begin
        if not CryptHashData(Hash, Buffer, BytesRead, 0) then
          RaiseException('CryptHashData failed');
        Done := Done + Integer(BytesRead);
        if Total > 0 then
          VerifyPage.SetProgress(Done, Total);
      end;
    until BytesRead = 0;

    SetLength(HashBytes, 32);
    HashLen := 32;
    if not CryptGetHashParam(Hash, HP_HASHVAL, HashBytes, HashLen, 0) then
      RaiseException('CryptGetHashParam failed');
    for I := 0 to 31 do
      Result := Result + Copy('0123456789abcdef', (HashBytes[I] shr 4) + 1, 1) + Copy('0123456789abcdef', (HashBytes[I] and 15) + 1, 1);
  finally
    if Hash <> 0 then
      CryptDestroyHash(Hash);
    if Prov <> 0 then
      CryptReleaseContext(Prov, 0);
    if hFile <> INVALID_HANDLE_VALUE then
      CloseHandle(hFile);
  end;
end;

// Verify a file's SHA-256 against the expected value while showing a progress bar.
function VerifyFileSha256WithProgress(const FileName, ExpectedSha: String): Boolean;
var
  Actual: String;
begin
  VerifyPage.SetText(ExpandConstant('{cm:SetupVerifyProgress}'), '');
  VerifyPage.Show;
  try
    Actual := ComputeSha256WithProgress(FileName);
  finally
    VerifyPage.Hide;
  end;
  Result := (Actual = LowerCase(ExpectedSha));
end;

function StageLocalPackageFile: Boolean;
var
  ZipPath: String;
begin
  Result := False;
  if not FileExists(ActivePackageFile) then
  begin
    SuppressibleMsgBox(ExpandConstant('{cm:SetupMissingPackageFile}') + #13#10 + ActivePackageFile, mbCriticalError, MB_OK, IDOK);
    Exit;
  end;

  ZipPath := ExpandConstant('{tmp}\{#PackageFilename}');
  Log('Using local Gyroflow package file ' + ActivePackageFile);
  try
    if not VerifyFileSha256WithProgress(ActivePackageFile, ActivePackageSha256) then
      RaiseException('Local package SHA256 mismatch.');
    if not FileCopy(ActivePackageFile, ZipPath, False) then
      RaiseException('Failed to stage local package file.');
    PackageWasFetched := True;
    Result := True;
  except
    SuppressibleMsgBox(ExpandConstant('{cm:SetupPackageFileVerifyFailed}') + #13#10 + GetExceptionMessage, mbCriticalError, MB_OK, IDOK);
  end;
end;

function DownloadAndVerifyPackage: Boolean;
var
  ZipPath: String;
begin
  Result := False;

  if (ActivePackageUrl = '') and (ActivePackageFile = '') then
  begin
    SuppressibleMsgBox(ExpandConstant('{cm:SetupMissingPackageUrl}'), mbCriticalError, MB_OK, IDOK);
    Exit;
  end;

  if ActivePackageSha256 = '' then
  begin
    SuppressibleMsgBox(ExpandConstant('{cm:SetupMissingPackageSha256}'), mbCriticalError, MB_OK, IDOK);
    Exit;
  end;

  if ActivePackageFile <> '' then
  begin
    Result := StageLocalPackageFile;
    Exit;
  end;

  ZipPath := ExpandConstant('{tmp}\{#PackageFilename}');
  Log('Downloading Gyroflow package from ' + ActivePackageUrl);
  DownloadPage.Show;
  try
    try
      // Download without Inno's built-in verify (empty hash), then verify ourselves
      // with a progress bar so the integrity check is not a silent stall.
      DownloadTemporaryFile(ActivePackageUrl, '{#PackageFilename}', '', @OnDownloadProgress);
      if not VerifyFileSha256WithProgress(ZipPath, ActivePackageSha256) then
        RaiseException('Downloaded package SHA256 mismatch.');
      PackageWasFetched := True;
      Result := True;
    except
      SuppressibleMsgBox(ExpandConstant('{cm:SetupDownloadVerifyFailed}') + #13#10 + GetExceptionMessage, mbCriticalError, MB_OK, IDOK);
    end;
  finally
    DownloadPage.Hide;
  end;
end;

function InitializeSetup: Boolean;
begin
  IsUpdateMode := IsSwitchEnabled('UPDATE', '0');
  ActiveInstallDir := GetSwitchValue('DIR', '');
  ActivePackageUrl := GetSwitchValue('PACKAGEURL', '{#PackageUrl}');
  ActivePackageFile := GetSwitchValue('PACKAGEFILE', '');
  ActivePackageSha256 := GetSwitchValue('PACKAGESHA256', '{#PackageSha256}');
  ActivePackageSize := StrToInt64Def(GetSwitchValue('PACKAGESIZE', '{#PackageSize}'), 0);
  WaitHandleValue := GetSwitchValue('WAITHANDLE', '');
  WaitPidValue := GetSwitchValue('WAITPID', '');
  WaitStartValue := GetSwitchValue('WAITSTART', '');
  LaunchAfterInstall := (not IsUpdateMode) or IsSwitchEnabled('LAUNCH', '0');
  if HasSwitch('LAUNCH') then
    LaunchAfterInstall := IsSwitchEnabled('LAUNCH', '1');

  Result := True;
end;

procedure InitializeWizard;
begin
  DownloadPage := CreateDownloadPage(ExpandConstant('{cm:SetupDownloadTitle}'), ExpandConstant('{cm:SetupDownloadDescription}'), @OnDownloadProgress);
  DownloadPage.ShowBaseNameInsteadOfUrl := True;
  VerifyPage := CreateOutputProgressPage(ExpandConstant('{cm:SetupVerifyTitle}'), ExpandConstant('{cm:SetupVerifyDescription}'));
  if ActiveInstallDir <> '' then
    WizardForm.DirEdit.Text := ActiveInstallDir;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = wpReady then
  begin
    WaitForUpdateTarget;
    // Inno extracts the downloaded archive itself via the [Files] extractarchive flag.
    Result := DownloadAndVerifyPackage;
  end;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := False;
  if IsUpdateMode then
    Result := (PageID = wpSelectDir) or (PageID = wpSelectProgramGroup) or (PageID = wpSelectTasks);
end;

function PackageWasDownloaded: Boolean;
begin
  Result := PackageWasFetched;
end;

function ShouldShowLaunchTask: Boolean;
begin
  Result := (not IsUpdateMode) and LaunchAfterInstall;
end;

function ShouldLaunchFromSwitch: Boolean;
begin
  Result := IsUpdateMode and LaunchAfterInstall;
end;
