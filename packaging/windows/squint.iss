; Inno Setup script for squint's Windows installer, compiled by the release
; workflow as:  iscc /DVersion=1.2.3 /DArch=x64 /DSource=<dir with squint.exe> /O<out> squint.iss
; Arch is x64 or arm64.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Arch
  #define Arch "x64"
#endif
#ifndef Source
  #define Source "."
#endif

[Setup]
AppId={{6B0C7E0A-5F8E-4E43-9C4A-2B3C5D0E7A11}
AppName=squint
AppVersion={#Version}
AppPublisher=André Biseth
AppPublisherURL=https://github.com/bisand/squint
DefaultDirName={autopf}\squint
DefaultGroupName=squint
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputBaseFilename=squint-{#Version}-windows-{#Arch}-setup
SetupIconFile=..\icons\squint.ico
UninstallDisplayIcon={app}\squint.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequiredOverridesAllowed=dialog
ChangesEnvironment=yes
ChangesAssociations=yes
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add squint to PATH"; GroupDescription: "Other:"; Flags: unchecked
Name: "openwith"; Description: "Add ""Open with squint"" to the Explorer menu"; GroupDescription: "Other:"

[Files]
Source: "{#Source}\squint.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{group}\squint"; Filename: "{app}\squint.exe"
Name: "{autodesktop}\squint"; Filename: "{app}\squint.exe"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Classes\*\shell\squint"; ValueType: string; ValueName: ""; ValueData: "Open with squint"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\*\shell\squint"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\squint.exe"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\*\shell\squint\command"; ValueType: string; ValueName: ""; ValueData: """{app}\squint.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\squint.exe"" ""%1"""; Flags: uninsdeletekey
; A file type of squint's own, which the extensions below name as one they
; open with, and the registration that puts squint in Settings → Default apps.
; Windows lets only the user choose the default; squint can only be offered.
Root: HKA; Subkey: "Software\Classes\squint.file"; ValueType: string; ValueName: ""; ValueData: "Text file"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\squint.file\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\squint.exe"",0"
Root: HKA; Subkey: "Software\Classes\squint.file\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\squint.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "squint"
Root: HKA; Subkey: "Software\squint"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\squint\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "squint"
Root: HKA; Subkey: "Software\squint\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Opens huge text files instantly, lets you look, tweak and save"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "squint"; ValueData: "Software\squint\Capabilities"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".txt"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".txt"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.txt\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".log"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".log"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.log\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".out"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".out"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.out\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".csv"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".csv"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.csv\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".tsv"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".tsv"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.tsv\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".json"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".json"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.json\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".jsonl"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".jsonl"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.jsonl\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".ndjson"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".ndjson"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.ndjson\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".xml"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".xml"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.xml\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".yaml"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".yaml"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.yaml\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".yml"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".yml"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.yml\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".toml"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".toml"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.toml\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".ini"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".ini"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.ini\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".conf"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".conf"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.conf\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".cfg"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".cfg"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.cfg\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".md"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".md"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.md\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\squint\Capabilities\FileAssociations"; ValueType: string; ValueName: ".sql"; ValueData: "squint.file"
Root: HKA; Subkey: "Software\Classes\Applications\squint.exe\SupportedTypes"; ValueType: string; ValueName: ".sql"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.sql\OpenWithProgids"; ValueType: string; ValueName: "squint.file"; ValueData: ""; Flags: uninsdeletevalue

[Run]
Filename: "{app}\squint.exe"; Description: "{cm:LaunchProgram,squint}"; Flags: nowait postinstall skipifsilent

[Code]
// PATH is the user's or the machine's, whichever the install was made for.
function PathKeyRoot: Integer;
begin
  if IsAdminInstallMode then Result := HKEY_LOCAL_MACHINE else Result := HKEY_CURRENT_USER;
end;

function PathKey: String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Path: String;
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
  begin
    if not RegQueryStringValue(PathKeyRoot, PathKey, 'Path', Path) then Path := '';
    if Pos(';' + Uppercase(ExpandConstant('{app}')) + ';', ';' + Uppercase(Path) + ';') = 0 then
    begin
      if (Path <> '') and (Copy(Path, Length(Path), 1) <> ';') then Path := Path + ';';
      RegWriteExpandStringValue(PathKeyRoot, PathKey, 'Path', Path + ExpandConstant('{app}'));
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, App: String;
  P: Integer;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    if RegQueryStringValue(PathKeyRoot, PathKey, 'Path', Path) then
    begin
      App := ExpandConstant('{app}');
      P := Pos(';' + Uppercase(App), ';' + Uppercase(Path));
      if P > 0 then
      begin
        Delete(Path, P, Length(App) + 1);
        StringChangeEx(Path, ';;', ';', True);
        RegWriteExpandStringValue(PathKeyRoot, PathKey, 'Path', Path);
      end;
    end;
  end;
end;
