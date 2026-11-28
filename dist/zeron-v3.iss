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
;   /LOG=<data>\logs\installer.log   what it did, the only account of an
;                              install that stopped partway

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
; A running v3 window is closed before its files are replaced, if Restart
; Manager can; one it can't is moved aside (MoveAside, below). The engine is
; not closed: it may be running agents, so it is moved aside too and the app
; restarts it into the new one once it is idle.
CloseApplications=yes
CloseApplicationsFilter=zeron.exe
RestartApplications=no
; Inno Setup 6.7+ turns on Windows' RedirectionGuard for itself, and a
; per-user Setup passes it to the zeron.exe it opens, then to the engine and
; every agent: none of them can follow a junction or symlink a user made,
; such as skills linked into ~/.claude/skills (error 448, "untrusted mount
; point"). The updater also passes /NOREDIRECTIONGUARD (patches/0005).
RedirectionGuard=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; The scripts are replaced whole, so a file a newer build dropped is gone:
; but never deleted before the new ones are in. They go into ui.new and
; plugins-builtin.new, and swap in only once every file is copied
; (SwapIn, below). An install that stops partway (its log in
; <data>\logs\installer.log) leaves the working ones as they were; on
; 2026-11-18 deleting ui here first left an install with no interface.
; What a stopped install left behind goes first.
Type: filesandordirs; Name: "{app}\ui.new"
Type: filesandordirs; Name: "{app}\ui.old"
Type: filesandordirs; Name: "{app}\plugins-builtin.new"
Type: filesandordirs; Name: "{app}\plugins-builtin.old"

[Files]
Source: "{#PackageDir}\*"; DestDir: "{app}"; Excludes: "\ui,\plugins-builtin"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#PackageDir}\ui\*"; DestDir: "{app}\ui.new"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#PackageDir}\plugins-builtin\*"; DestDir: "{app}\plugins-builtin.new"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\Zeron v3"; Filename: "{app}\zeron.exe"; IconFilename: "{app}\zeron.ico"
Name: "{autodesktop}\Zeron v3"; Filename: "{app}\zeron.exe"; IconFilename: "{app}\zeron.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\zeron.exe"; Description: "{cm:LaunchProgram,Zeron v3}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\zeron.exe"; Flags: nowait; Check: Relaunch

[UninstallDelete]
Type: files; Name: "{app}\zeron-engine*.old"
Type: files; Name: "{app}\zeron-app*.old"
Type: filesandordirs; Name: "{app}\.zeron-update-*"
; Installed as ui.new and plugins-builtin.new, then renamed: the uninstall
; log names the .new paths, so the folders they became go by name.
Type: filesandordirs; Name: "{app}\ui"
Type: filesandordirs; Name: "{app}\ui.new"
Type: filesandordirs; Name: "{app}\ui.old"
Type: filesandordirs; Name: "{app}\plugins-builtin"
Type: filesandordirs; Name: "{app}\plugins-builtin.new"
Type: filesandordirs; Name: "{app}\plugins-builtin.old"

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

var
  ClosedWindows: Boolean;

// A window this update closed (StopStrayWindows) opens again after it, even
// for an install on quit, which doesn't ask to.
function Relaunch(): Boolean;
begin
  Result := WizardSilent and ((ExpandConstant('{param:RELAUNCH|0}') = '1') or ClosedWindows);
end;

// A silent update starts once the app that asked for it exits, but another
// Zeron window from this folder may run on: one opened as the update began
// (2026-11-28, 3 ms after), or a second one. Restart Manager can't close it
// (error 350), and it holds ui's files, so ui can't swap and the new app
// would start on the old scripts. Those are ended here; the update opens
// the app again. (Only zeron.exe: the engine runs agents, and is moved
// aside instead. Never in an install someone runs themselves.)
procedure StopStrayWindows();
var
  Locator, Service, Found, Process: Variant;
  Folder, Path: String;
  I: Integer;
begin
  if not WizardSilent then
    Exit;
  Folder := Lowercase(AddBackslash(ExpandConstant('{app}')));
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Found := Service.ExecQuery('SELECT ProcessId, ExecutablePath FROM Win32_Process WHERE Name = ''zeron.exe''');
    for I := 0 to Found.Count - 1 do
    begin
      Process := Found.ItemIndex(I);
      if VarIsNull(Process.ExecutablePath) then
        Continue;
      Path := Process.ExecutablePath;
      if Pos(Folder, Lowercase(Path)) <> 1 then
        Continue;
      Log('Ending a Zeron window still running: ' + Path + ' (' + IntToStr(Process.ProcessId) + ')');
      Process.Terminate();
      ClosedWindows := True;
    end;
  except
    Log('Could not look for Zeron windows still running: ' + GetExceptionMessage);
  end;
  // (Terminate returns before the process is gone, and its files with it.)
  if ClosedWindows then
    Sleep(1000);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopStrayWindows();
  Result := '';
end;

// A folder can't be renamed while anything holds a file in it without
// sharing delete: an antivirus scanning the files just written does, for a
// moment. So a rename is tried for up to 10 s before it counts as failed.
function RenameDir(Source, Dest: String): Boolean;
var
  Tries: Integer;
begin
  Tries := 0;
  repeat
    Result := RenameFile(Source, Dest);
    if Result then
      Exit;
    Tries := Tries + 1;
    Sleep(250);
  until Tries >= 40;
  Log('Could not rename ' + Source + ' to ' + Dest + ' in ' + IntToStr(Tries) + ' tries');
end;

// Name.new, every file of it copied, becomes Name; the one it replaces goes
// aside first and is deleted after. A swap that fails puts the old one back,
// so there is always one whole folder under Name.
procedure SwapIn(Name: String);
var
  Current, Incoming, Aside: String;
begin
  Current := ExpandConstant('{app}\' + Name);
  Incoming := Current + '.new';
  Aside := Current + '.old';
  if not DirExists(Incoming) then
    Exit;
  // One a stopped earlier swap couldn't delete would block the move aside.
  if DirExists(Aside) then
    DelTree(Aside, True, True, True);
  if DirExists(Current) and not RenameDir(Current, Aside) then
  begin
    Log('Could not move ' + Current + ' aside; it stays, and ' + Incoming + ' with it');
    Exit;
  end;
  if not RenameDir(Incoming, Current) then
  begin
    Log('Could not move ' + Incoming + ' into place; putting the old one back');
    if DirExists(Aside) and not RenameDir(Aside, Current) then
      Log('Could not put ' + Aside + ' back: ' + Current + ' is missing');
    Exit;
  end;
  DelTree(Aside, True, True, True);
  Log('Swapped in ' + Current);
end;

// The last word before Setup ends, however it went: the app shows nothing
// without its ui folder (as after 2026-11-18's update), so if Name is
// missing, whichever whole copy is left, the new or the old, becomes it.
procedure EnsureDir(Name: String);
var
  Current: String;
begin
  Current := ExpandConstant('{app}\' + Name);
  if DirExists(Current) then
    Exit;
  if DirExists(Current + '.new') and RenameDir(Current + '.new', Current) then
    Log('Put ' + Current + '.new in place of the missing ' + Current)
  else if DirExists(Current + '.old') and RenameDir(Current + '.old', Current) then
    Log('Put ' + Current + '.old back in place of the missing ' + Current)
  else
    Log(Current + ' is missing and nothing could take its place');
end;

var
  // What MoveAside moved, and where to: put back if the install stops.
  MovedFrom, MovedTo: array of String;
  Installed: Boolean;

// A running executable can't be overwritten but can be renamed: move it to
// <Stem>N.old so the new one goes in place, whether or not it runs. Earlier
// ones still running keep their names. Inno Setup doesn't know of the move,
// so its rollback of a stopped install deletes the new file and leaves the
// place empty: DeinitializeSetup puts this one back then. (On 2026-11-28 a
// zeron.exe opened as the update began couldn't be replaced, the install
// rolled back, and the engine it had moved aside was gone with it.)
procedure MoveAside(FileName, Stem: String);
var
  Current, Aside: String;
  N: Integer;
begin
  Current := ExpandConstant('{app}\' + FileName);
  if not FileExists(Current) then
    Exit;
  N := 0;
  repeat
    Aside := ExpandConstant('{app}\' + Stem) + IntToStr(N) + '.old';
    DeleteFile(Aside);
    N := N + 1;
  until not FileExists(Aside) or (N > 20);
  if not RenameFile(Current, Aside) then
  begin
    Log('Could not move ' + Current + ' aside');
    Exit;
  end;
  Log('Moved ' + Current + ' aside to ' + Aside);
  N := GetArrayLength(MovedFrom);
  SetArrayLength(MovedFrom, N + 1);
  SetArrayLength(MovedTo, N + 1);
  MovedFrom[N] := Current;
  MovedTo[N] := Aside;
end;

// The app downloads each update's installer to {app}\.zeron-update-*\ (60 MB)
// and nothing removed it once installed. All go but the one this Setup runs
// from, which can't while it runs: the next update's download clears it.
procedure DeleteOldDownloads();
var
  Find: TFindRec;
  Own, Dir: String;
begin
  Own := Lowercase(RemoveBackslash(ExtractFilePath(ExpandConstant('{srcexe}'))));
  if not FindFirst(ExpandConstant('{app}\.zeron-update-*'), Find) then
    Exit;
  try
    repeat
      Dir := ExpandConstant('{app}\') + Find.Name;
      if (Find.Attributes and FILE_ATTRIBUTE_DIRECTORY <> 0) and (Lowercase(Dir) <> Own) then
        if DelTree(Dir, True, True, True) then
          Log('Deleted the old download ' + Dir);
    until not FindNext(Find);
  finally
    FindClose(Find);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  I: Integer;
begin
  if CurStep = ssInstall then
  begin
    // The engine may be running agents. A window Restart Manager couldn't
    // close (it fails on a zeron.exe that has just started) would otherwise
    // stop the whole install: under /SUPPRESSMSGBOXES a file in use aborts.
    MoveAside('zeron-engine.exe', 'zeron-engine');
    MoveAside('zeron.exe', 'zeron-app');
  end;
  if CurStep = ssPostInstall then
  begin
    Installed := True;
    SwapIn('ui');
    SwapIn('plugins-builtin');
    // Those not running any more go now; the rest, on a later update.
    for I := 0 to GetArrayLength(MovedTo) - 1 do
      DeleteFile(MovedTo[I]);
    DeleteOldDownloads();
  end;
end;

// An install that stopped (rolled back, or never got to its files): what was
// moved aside goes back where nothing new took its place, and with
// /RELAUNCH=1 the app opens again, as it would have after the update.
procedure DeinitializeSetup();
var
  I, ErrorCode: Integer;
begin
  // (Not for a first install that never got its files in.)
  if FileExists(ExpandConstant('{app}\zeron.exe')) then
  begin
    EnsureDir('ui');
    EnsureDir('plugins-builtin');
  end;
  if Installed then
    Exit;
  for I := 0 to GetArrayLength(MovedFrom) - 1 do
  begin
    if FileExists(MovedFrom[I]) then
      Continue;
    if RenameFile(MovedTo[I], MovedFrom[I]) then
      Log('Put ' + MovedFrom[I] + ' back')
    else
      Log('Could not put ' + MovedTo[I] + ' back: ' + MovedFrom[I] + ' is missing');
  end;
  if Relaunch() and FileExists(ExpandConstant('{app}\zeron.exe')) then
    ShellExec('', ExpandConstant('{app}\zeron.exe'), '', '', SW_SHOWNORMAL, ewNoWait, ErrorCode);
end;
