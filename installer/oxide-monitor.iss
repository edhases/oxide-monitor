; Oxide Monitor installer (Inno Setup 6.7+)
; Per-user install: no administrator/UAC is required.

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName "Oxide Monitor"
#define MyAppPublisher "Oxide Monitor contributors"
#define MyAppURL "https://github.com/edhases/oxide-monitor"
#define MyAppExeName "oxide-monitor.exe"
#define MyAppId "{{8F3C2B1A-7D4E-4A9B-9C2D-1E5F6A7B8C9D}"

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={localappdata}\OxideMonitor
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
DisableWelcomePage=no
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=OxideMonitor-{#MyAppVersion}-Setup
SetupIconFile=..\resources\oxide.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
RestartApplications=no
AllowNoIcons=yes
UsePreviousAppDir=yes
VersionInfoVersion={#MyAppVersion}
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription={#MyAppName} installer
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppVersion}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\oxide-monitor.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\oxide-settings.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\oxide-pawnio-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.uk.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\NOTICE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\crates\oxide-pawnio-host\assets\NOTICE.md"; DestDir: "{app}"; DestName: "PawnIO-NOTICE.md"; Flags: ignoreversion
Source: "..\crates\oxide-pawnio-host\assets\COPYING-PawnIO-Modules.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\AUTOSTART.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "..\docs\PROVIDERS.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "..\docs\SETTINGS.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "..\resources\config.toml"; DestDir: "{app}\resources"; DestName: "config.toml"; Flags: ignoreversion
Source: "..\resources\oxide.ico"; DestDir: "{app}\resources"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"
Name: "{group}\{#MyAppName} Settings"; Filename: "{app}\oxide-settings.exe"; WorkingDir: "{app}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /TN ""Oxide Monitor"" /F"; Flags: runhidden; RunOnceId: "RemoveOxideAutostartTask"

[UninstallDelete]
Type: filesandordirs; Name: "{app}\docs"
Type: filesandordirs; Name: "{app}\resources"
