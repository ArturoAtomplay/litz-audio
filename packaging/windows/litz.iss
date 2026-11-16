; Build with Inno Setup 6 after `cargo build --profile dist -p litzd` (MSVC) and
; `packaging/fetch-model.sh target/model`. CI passes /DAppVersion=X.Y.Z.
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\dist"
#endif
[Setup]
AppId=LitzAudio
AppName=Litz Audio
AppVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Litz Audio
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
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
[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Litz Audio"; ValueData: """{app}\litzdw.exe"""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\LitzAudio"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Classes\CLSID\{{BFD99F34-90AE-4D6D-A33A-6EB4D6FCC2A4}"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NotOnPath
[Run]
Filename: "{app}\litzdw.exe"; Flags: nowait
Filename: "https://vb-audio.com/Cable/"; Description: "Open the virtual microphone driver download (needed only for phone microphone)"; Flags: shellexec postinstall skipifsilent unchecked
[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM litzd.exe /F"; Flags: runhidden; RunOnceId: "StopLitzd"
[Code]
function PathPosition(Paths: String): Integer;
begin
  Result := Pos(';' + Uppercase(ExpandConstant('{app}')) + ';', ';' + Uppercase(Paths) + ';');
end;

function NotOnPath: Boolean;
var
  Paths: String;
begin
  Result := not RegQueryStringValue(HKCU, 'Environment', 'Path', Paths) or (PathPosition(Paths) = 0);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths: String;
  Position: Integer;
begin
  if (CurUninstallStep <> usUninstall) or not RegQueryStringValue(HKCU, 'Environment', 'Path', Paths) then
    exit;
  Position := PathPosition(Paths);
  if Position = 0 then
    exit;
  Paths := ';' + Paths + ';';
  Delete(Paths, Position, Length(ExpandConstant('{app}')) + 1);
  RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Copy(Paths, 2, Length(Paths) - 2));
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM litzd.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if ResultCode = 0 then
    Sleep(500);
  Result := '';
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
    WizardForm.FinishedLabel.Caption :=
      'Litz is installed and starts in the background at sign-in. Run litzd --help in a new terminal to see its commands. To pair a phone, open Litz on the phone, tap this computer in the Nearby list, then click Allow when the prompt appears on this computer.' + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
      'Allow UDP 47165 on your private-network firewall. If your phone cannot find this PC, use Pair by IP address in the phone app.' + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
      'For phone microphone use, install VB-CABLE once and choose CABLE Output in your calling app.';
end;
