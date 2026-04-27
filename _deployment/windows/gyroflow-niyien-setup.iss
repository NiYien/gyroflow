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
ArchiveExtraction=basic
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
SetupExtractPackageFailed=Failed to extract Gyroflow(NiYien) package.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{tmp}\package\*"; DestDir: "{app}"; ExternalSize: {#PackageExternalSize}; Flags: external recursesubdirs createallsubdirs ignoreversion; Check: PackageWasDownloaded

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

var
  DownloadPage: TDownloadWizardPage;
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

function StageLocalPackageFile: Boolean;
var
  ZipPath: String;
  ActualSha256: String;
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
    ActualSha256 := LowerCase(GetSHA256OfFile(ActivePackageFile));
    if ActualSha256 <> LowerCase(ActivePackageSha256) then
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
  ActualSha256: String;
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
      DownloadTemporaryFile(ActivePackageUrl, '{#PackageFilename}', ActivePackageSha256, @OnDownloadProgress);
      ActualSha256 := LowerCase(GetSHA256OfFile(ZipPath));
      if ActualSha256 <> LowerCase(ActivePackageSha256) then
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

function PowerShellSingleQuotedLiteral(const Value: String): String;
var
  I: Integer;
  Ch: String;
begin
  Result := #39;
  for I := 1 to Length(Value) do
  begin
    Ch := Copy(Value, I, 1);
    if Ch = #39 then
      Result := Result + #39 + #39
    else
      Result := Result + Ch;
  end;
  Result := Result + #39;
end;

function ExtractPackageToTempDir: Boolean;
var
  ZipPath: String;
  ExtractDir: String;
  PowerShellPath: String;
  Params: String;
  ResultCode: Integer;
begin
  Result := False;
  ZipPath := ExpandConstant('{tmp}\{#PackageFilename}');
  ExtractDir := ExpandConstant('{tmp}\package');
  DelTree(ExtractDir, True, True, True);
  if not ForceDirectories(ExtractDir) then
  begin
    SuppressibleMsgBox(ExpandConstant('{cm:SetupExtractPackageFailed}') + #13#10 + ExtractDir, mbCriticalError, MB_OK, IDOK);
    Exit;
  end;

  PowerShellPath := ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe');
  Params := '-NoProfile -ExecutionPolicy Bypass -Command "Expand-Archive -LiteralPath ' + PowerShellSingleQuotedLiteral(ZipPath) + ' -DestinationPath ' + PowerShellSingleQuotedLiteral(ExtractDir) + ' -Force"';
  Log('Extracting Gyroflow package to ' + ExtractDir);
  if Exec(PowerShellPath, Params, '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0) then
  begin
    PackageWasFetched := True;
    Result := True;
  end
  else
    SuppressibleMsgBox(ExpandConstant('{cm:SetupExtractPackageFailed}') + #13#10 + 'Exit code: ' + IntToStr(ResultCode), mbCriticalError, MB_OK, IDOK);
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
  if ActiveInstallDir <> '' then
    WizardForm.DirEdit.Text := ActiveInstallDir;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = wpReady then
  begin
    WaitForUpdateTarget;
    Result := DownloadAndVerifyPackage and ExtractPackageToTempDir;
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
