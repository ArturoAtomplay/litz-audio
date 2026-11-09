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
UninstallDisplayIcon={app}\litzd.exe
[Files]
Source: "{#BinDir}\litzd.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\model\DeepFilterNet3_onnx.tar.gz"; DestDir: "{app}"
Source: "..\..\packaging\android\icon-512.png"; DestDir: "{app}"; DestName: "litz.png"
Source: "..\..\LICENSE"; DestDir: "{app}"
Source: "..\..\THIRD_PARTY_NOTICES"; DestDir: "{app}"
[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Litz Audio"; ValueData: """{app}\litzd.exe"""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\LitzAudio"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Classes\CLSID\{{BFD99F34-90AE-4D6D-A33A-6EB4D6FCC2A4}"; Flags: uninsdeletekey dontcreatekey
[Run]
Filename: "{app}\litzd.exe"; Flags: nowait runhidden
Filename: "https://vb-audio.com/Cable/"; Description: "Open the virtual microphone driver download (needed only for phone microphone)"; Flags: shellexec postinstall skipifsilent unchecked
[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM litzd.exe /F"; Flags: runhidden; RunOnceId: "StopLitzd"
[Code]
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
      'Litz is installed and starts in the background at sign-in. To pair a phone, open Litz on the phone, tap this computer in the Nearby list, then click Allow when the prompt appears on this computer.' + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
      'Allow UDP 47165 on your private-network firewall. If your phone cannot find this PC, use Pair by IP address in the phone app.' + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
      'For phone microphone use, install VB-CABLE once and choose CABLE Output in your calling app.';
end;
