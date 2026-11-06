; Zeron v3 for Windows: per-user installer (Inno Setup 6), built by
; scripts/package-v3-windows.ps1 from its staged folder.
;
; Installs into %LOCALAPPDATA%\Programs\Zeron v3, apart from the v2 app
; (Zeron, its own AppId), so both can be installed and either uninstalled.
; They share Zeron's data in %LOCALAPPDATA%\Zeron and its engine on port
; 27654: v3 uses an engine that already runs. No zeron:// links here; the
; v2 app keeps them.
;
; It is also v3's updater: the app downloads it and runs it silently over
; its folder (patches/0005, crates/update/src/windows.rs) with
;   /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOCANCEL /DIR=<folder>
;   /WAITPID=<the app's pid>   wait for the app to exit first
;   /RELAUNCH=1                open the app again when done

#ifndef AppVersion
  #error AppVersion must be defined
#endif
#ifndef NumericVersion
  #define NumericVersion AppVersion
#endif
#ifndef Arch
  #error Arch must be defined
#endif
#ifndef PackageDir
  #error PackageDir must be defined
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif
#ifndef IconFile
  #error IconFile must be defined
#endif

#if Arch == "aarch64"
  #define ArchAllowed "arm64"
#else
  #define ArchAllowed "x64compatible"
#endif

[Setup]
; Never change AppId: it identifies v3 installs across upgrades.
AppId={{6F0E2B3C-9A41-4C7D-B5E2-3D8A1F27C9E4}
AppName=Zeron v3
AppVersion={#AppVersion}
AppVerName=Zeron v3 {#AppVersion}
AppPublisher=Zeron nightly
AppPublisherURL=https://github.com/hoangvu12/zeron-nightly
VersionInfoVersion={#NumericVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Zeron v3
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=yes
ArchitecturesAllowed={#ArchAllowed}
ArchitecturesInstallIn64BitMode={#ArchAllowed}
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=zeron-{#AppVersion}-windows-{#Arch}-setup
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\zeron.ico
UninstallDisplayName=Zeron v3
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; A running v3 window is closed before its files are replaced. The engine is
; not: it may be running agents, so it is moved aside instead (below) and
; the app restarts it into the new one once it is idle.
CloseApplications=yes
CloseApplicationsFilter=zeron.exe
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; The scripts are replaced whole, so a file a newer build dropped is gone.
Type: filesandordirs; Name: "{app}\ui"
Type: filesandordirs; Name: "{app}\plugins-builtin"

[Files]
Source: "{#PackageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Zeron v3"; Filename: "{app}\zeron.exe"; IconFilename: "{app}\zeron.ico"
Name: "{autodesktop}\Zeron v3"; Filename: "{app}\zeron.exe"; IconFilename: "{app}\zeron.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\zeron.exe"; Description: "{cm:LaunchProgram,Zeron v3}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\zeron.exe"; Flags: nowait; Check: Relaunch

[UninstallDelete]
Type: files; Name: "{app}\zeron-engine*.old"
Type: filesandordirs; Name: "{app}\.zeron-update-*"

[Code]
const
  SYNCHRONIZE = $00100000;

function OpenProcess(Access: Cardinal; Inherit: Boolean; Pid: Cardinal): Cardinal;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: Cardinal; Milliseconds: Cardinal): Cardinal;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: Cardinal): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

// An update started by the app: wait (up to a minute) for it to exit.
function InitializeSetup(): Boolean;
var
  Pid, Process: Cardinal;
begin
  Pid := StrToIntDef(ExpandConstant('{param:WAITPID|0}'), 0);
  if Pid <> 0 then
  begin
    Process := OpenProcess(SYNCHRONIZE, False, Pid);
    if Process <> 0 then
    begin
      WaitForSingleObject(Process, 60000);
      CloseHandle(Process);
    end;
  end;
  Result := True;
end;

function Relaunch(): Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:RELAUNCH|0}') = '1');
end;

// A running engine can't be overwritten but can be renamed: move it aside so
// the new one goes in place. Earlier ones still running keep their names.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Engine, Aside: String;
  N: Integer;
begin
  if CurStep <> ssInstall then
    Exit;
  Engine := ExpandConstant('{app}\zeron-engine.exe');
  if not FileExists(Engine) then
    Exit;
  if DeleteFile(Engine) then
    Exit;
  N := 0;
  repeat
    Aside := ExpandConstant('{app}\zeron-engine') + IntToStr(N) + '.old';
    DeleteFile(Aside);
    N := N + 1;
  until not FileExists(Aside) or (N > 20);
  if not RenameFile(Engine, Aside) then
    Log('Could not move the running engine aside: ' + Engine);
end;
