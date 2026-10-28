#ifndef RepoRoot
  #error RepoRoot must point to the repository root
#endif
#ifndef BinaryPath
  #error BinaryPath must point to the release executable
#endif
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.1.0.0"
#endif

[Setup]
AppId={{9773C9F4-7258-44A0-A5E8-A7E7CC3D185A}
AppName=rumstsc
AppVersion={#AppVersion}
AppPublisher=Cattens-Roses
AppPublisherURL=https://github.com/Cattens-Roses/rumstsc
AppSupportURL=https://github.com/Cattens-Roses/rumstsc/issues
DefaultDirName={localappdata}\Programs\rumstsc
DefaultGroupName=rumstsc
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile={#RepoRoot}\assets\rumstsc.ico
UninstallDisplayIcon={app}\rumstsc.exe
LicenseFile={#RepoRoot}\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
VersionInfoVersion={#NumericVersion}
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#BinaryPath}"; DestDir: "{app}"; DestName: "rumstsc.exe"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\rumstsc"; Filename: "{app}\rumstsc.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\rumstsc"; Filename: "{app}\rumstsc.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\rumstsc.exe"; Description: "Launch rumstsc"; Flags: nowait postinstall skipifsilent
