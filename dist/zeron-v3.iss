; Zeron v3 for Windows: per-user installer (Inno Setup 6), built by
; scripts/package-v3-windows.ps1 from its staged folder.
;
; Installs into %LOCALAPPDATA%\Programs\Zeron v3, apart from the v2 app
; (Zeron, its own AppId), so both can be installed and either uninstalled.
; They share Zeron's data in %LOCALAPPDATA%\Zeron and its engine on port
; 27654: v3 uses an engine that already runs. No zeron:// links here; the
; v2 app keeps them.

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
CloseApplications=yes
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
