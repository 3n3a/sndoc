#define AppName "sndoc"
#define AppVersion "0.2.0"
#define AppPublisher "3n3a"
#define AppExeName "sndoc.exe"
; The Claude skill is now a directory (.claude\skills\sndoc\SKILL.md), not a
; single command file.
#define SkillSrc "..\\.claude\\skills\\sndoc\\SKILL.md"

[Setup]
; Unique to sndoc — do NOT reuse the old sn-doc-cli AppId.
AppId={{6F2A1B7C-3D84-4E15-9A6B-2C7E0F9D4A31}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
OutputDir=..\dist
OutputBaseFilename=sndoc-setup
Compression=lzma
SolidCompression=yes
WizardStyle=modern
PrivilegesRequiredOverridesAllowed=dialog

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "addtopath";    Description: "Add sndoc to PATH";                                            GroupDescription: "Shell integration:"
Name: "installskill"; Description: "Install Claude Code skill (~\.claude\skills\sndoc\SKILL.md)";  GroupDescription: "Claude integration:"
Name: "deletecache";  Description: "Remove cached data on uninstall ({localappdata}\sndoc)";       GroupDescription: "Uninstall options:"; Flags: unchecked

[Files]
Source: "..\dist\{#AppExeName}"; DestDir: "{app}";                                Flags: ignoreversion
Source: {#SkillSrc};             DestDir: "{%USERPROFILE}\.claude\skills\sndoc";   Flags: ignoreversion; Tasks: installskill

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; \
  Check: NeedsAddPath(ExpandConstant('{app}')); \
  Tasks: addtopath

; sndoc keeps its clone + index under platformdirs.user_data_dir("sndoc"),
; i.e. %LOCALAPPDATA%\sndoc. The ~123 MB Hugging Face model cache lives
; separately under %USERPROFILE%\.cache\huggingface and is intentionally NOT
; removed here (it may be shared with other tools).
[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\sndoc"; Tasks: deletecache

[Code]
function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Param + ';', ';' + OrigPath + ';') = 0;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ClaudeSkillDir: string;
begin
  if CurStep = ssInstall then
  begin
    if IsTaskSelected('installskill') then
    begin
      ClaudeSkillDir := ExpandConstant('{%USERPROFILE}') + '\.claude\skills\sndoc';
      if not DirExists(ClaudeSkillDir) then
        ForceDirectories(ClaudeSkillDir);
    end;
  end;
end;
