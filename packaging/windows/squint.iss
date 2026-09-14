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
