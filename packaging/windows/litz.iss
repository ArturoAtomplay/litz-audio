; Build with Inno Setup 6 after `cargo build --profile dist -p litzd` (MSVC)
; and `packaging/fetch-model.sh target/model`. CI passes /DAppVersion=X.Y.Z.
; DriverDir holds the Microsoft-signed driver package, which changes only with
; the driver's own version.
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\dist"
#endif
#ifndef DriverDir
  #define DriverDir "driver"
#endif
#define EnvironmentKey "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"
#define LegacyUninstallKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\LitzAudio_is1"
[Setup]
AppId=LitzAudio
AppName=Litz Audio
AppVersion={#AppVersion}
DefaultDirName={autopf}\Litz Audio
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\..\dist
OutputBaseFilename=Litz-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=no
RestartApplications=no
ChangesEnvironment=yes
UninstallDisplayIcon={app}\litzd.exe
[Files]
Source: "{#BinDir}\litzd.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\litzdw.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\model\DeepFilterNet3_onnx.tar.gz"; DestDir: "{app}"
Source: "..\..\fastlane\metadata\android\en-US\images\icon.png"; DestDir: "{app}"; DestName: "litz.png"
Source: "..\..\LICENSE"; DestDir: "{app}"
Source: "..\..\THIRD_PARTY_NOTICES"; DestDir: "{app}"
Source: "..\..\licenses\desktop.txt"; DestDir: "{app}"; DestName: "THIRD_PARTY_LICENSES.txt"
Source: "{#DriverDir}\litzaudio.inf"; DestDir: "{app}\driver"; Flags: ignoreversion
Source: "{#DriverDir}\litzaudio.sys"; DestDir: "{app}\driver"; Flags: ignoreversion
Source: "{#DriverDir}\litzaudio.cat"; DestDir: "{app}\driver"; Flags: ignoreversion
Source: "..\..\crates\desktop\driver\LICENSE"; DestDir: "{app}\driver"; DestName: "LICENSE.txt"
[Registry]
Root: HKLM; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Litz Audio"; ValueData: """{app}\litzdw.exe"""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\LitzAudio"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Classes\CLSID\{{BFD99F34-90AE-4D6D-A33A-6EB4D6FCC2A4}"; Flags: uninsdeletekey dontcreatekey
Root: HKLM; Subkey: "{#EnvironmentKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NotOnPath
[Run]
Filename: "{app}\litzdw.exe"; Flags: nowait runasoriginaluser
[Code]
var
  DriverNeedsRestart: Boolean;

function PathPosition(Paths: String): Integer;
begin
  Result := Pos(';' + Uppercase(ExpandConstant('{app}')) + ';', ';' + Uppercase(Paths) + ';');
end;

function NotOnPath: Boolean;
var
  Paths: String;
begin
  Result := not RegQueryStringValue(HKLM, '{#EnvironmentKey}', 'Path', Paths) or (PathPosition(Paths) = 0);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths: String;
  Position: Integer;
  ResultCode: Integer;
begin
  if CurUninstallStep <> usUninstall then
    exit;
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM litzd.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if Exec(ExpandConstant('{app}\litzd.exe'), 'uninstall-driver', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    DriverNeedsRestart := ResultCode = 3010;
  if not RegQueryStringValue(HKLM, '{#EnvironmentKey}', 'Path', Paths) then
    exit;
  Position := PathPosition(Paths);
  if Position = 0 then
    exit;
  Paths := ';' + Paths + ';';
  Delete(Paths, Position, Length(ExpandConstant('{app}')) + 1);
  RegWriteExpandStringValue(HKLM, '{#EnvironmentKey}', 'Path', Copy(Paths, 2, Length(Paths) - 2));
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
  Uninstaller: String;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM litzd.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if ResultCode = 0 then
    Sleep(500);
  if RegQueryStringValue(HKCU, '{#LegacyUninstallKey}', 'UninstallString', Uninstaller) then
    Exec(RemoveQuotes(Uninstaller), '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep <> ssPostInstall then
    exit;
  if not Exec(ExpandConstant('{app}\litzd.exe'), 'install-driver "' + ExpandConstant('{app}\driver\litzaudio.inf') + '"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) or ((ResultCode <> 0) and (ResultCode <> 3010)) then
    MsgBox('Litz Microphone could not be installed (error ' + IntToStr(ResultCode) + '). Listening to this computer on the phone still works; run the installer again to retry.', mbError, MB_OK)
  else
    DriverNeedsRestart := ResultCode = 3010;
end;

function NeedRestart: Boolean;
begin
  Result := DriverNeedsRestart;
end;

function UninstallNeedRestart: Boolean;
begin
  Result := DriverNeedsRestart;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
    WizardForm.FinishedLabel.Caption :=
      'Litz is installed and starts in the background at sign-in. Run litzd --help in a new terminal to see its commands. To pair a phone, open Litz on the phone, tap this computer in the Nearby list, then click Allow when the prompt appears on this computer.' + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
      'Allow UDP 47165 on your private-network firewall. If your phone cannot find this PC, use Pair by IP address in the phone app.' + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
      'To use the phone as a microphone, choose Litz Microphone in your calling app.';
end;
