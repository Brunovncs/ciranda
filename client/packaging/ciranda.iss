; The Windows installer, built with Inno Setup 6 from client\:
;   iscc /DVersion=4.3.0 /DBuild=target\release packaging\ciranda.iss
; It installs for the current user, without administrator rights, and asks whether to add
; shortcuts to the Start menu and the desktop. Updates from inside Ciranda swap the program in
; place, and the program keeps the version in the installed apps list current (core/install.rs).
; Settings live in %APPDATA%\Ciranda; the uninstaller asks whether to keep them or delete them.

#ifndef Version
  #error Pass the version: /DVersion=x.y.z
#endif
#ifndef Build
  #define Build "target\release"
#endif
; Where Ciranda keeps its settings (data_dir in core/settings.rs); a test build points it elsewhere.
#ifndef DataDir
  #define DataDir "{userappdata}\Ciranda"
#endif
#define Root ".."

[Setup]
; core/install.rs names this key; change both together.
AppId={{A913E7F2-9D27-43F1-8B65-4F2E8D2DF207}
AppName=Ciranda
AppVersion={#Version}
AppVerName=Ciranda {#Version}
AppPublisher=Brunovncs
AppPublisherURL=https://github.com/Brunovncs/ciranda
AppSupportURL=https://github.com/Brunovncs/ciranda/issues
AppUpdatesURL=https://github.com/Brunovncs/ciranda/releases/latest
VersionInfoVersion={#Version}.0
DefaultDirName={localappdata}\Programs\Ciranda
DisableDirPage=auto
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
ShowLanguageDialog=auto
SetupIconFile={#Root}\assets\icon.ico
UninstallDisplayIcon={app}\ciranda.exe
UninstallDisplayName=Ciranda
CloseApplications=no
RestartApplications=no
Compression=lzma2/max
SolidCompression=yes
OutputDir={#Root}\..\dist
; It must not end in -windows-x64.exe: the in-app updater takes the first asset that does.
OutputBaseFilename=ciranda-{#Version}-windows-x64-setup

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "pt"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

; In Portuguese the app is just "Ciranda", with no article and no pronoun; Inno Setup's own
; messages say "o [name]", and adjectives would need a gender too, so these are reworded.
[Messages]
en.WizardSelectTasks=Shortcuts
en.SelectTasksDesc=Where should Ciranda be one click away?
en.SelectTasksLabel2=Choose where to add shortcuts to [name], then click Next.
pt.WizardSelectTasks=Atalhos
pt.SelectTasksDesc=Onde Ciranda deve ficar a um clique?
pt.SetupAppRunningError=O instalador detectou que %1 está em execução.%n%nFeche %1 e clique em OK para continuar, ou em Cancelar para sair.
pt.UninstallAppRunningError=O desinstalador detectou que %1 está em execução.%n%nFeche %1 e clique em OK para continuar, ou em Cancelar para sair.
pt.SelectDirDesc=Onde instalar [name]?
pt.SelectDirLabel3=O instalador vai instalar [name] na pasta abaixo.
pt.SelectTasksLabel2=Escolha onde criar atalhos para [name] e clique em Avançar.
pt.ReadyLabel1=O instalador está pronto para instalar [name] no seu computador.
pt.PreparingDesc=O instalador está se preparando para instalar [name] no seu computador.
pt.InstallingLabel=Aguarde enquanto o instalador instala [name] no seu computador.
pt.FinishedHeadingLabel=Instalação de [name] concluída
pt.FinishedLabelNoIcons=O instalador terminou de instalar [name] no seu computador.
pt.FinishedLabel=O instalador terminou de instalar [name] no seu computador. Use os atalhos criados para abrir [name].
pt.FinishedRestartLabel=Para concluir a instalação de [name], o computador precisa ser reiniciado. Reiniciar agora?
pt.FinishedRestartMessage=Para concluir a instalação de [name], o computador precisa ser reiniciado.%n%nReiniciar agora?
pt.PreviousInstallNotCompleted=A instalação ou remoção de um programa anterior não terminou. Reinicie o computador e rode o instalador de novo para concluir a instalação de [name].
pt.ConfirmUninstall=Tem certeza de que deseja remover %1 do seu computador?
pt.UninstallStatusLabel=Aguarde, removendo %1 do seu computador.
pt.UninstalledAll=Desinstalação de %1 concluída.
pt.UninstalledMost=A desinstalação de %1 terminou.%n%nAlguns itens não puderam ser removidos e podem ser apagados manualmente.
pt.UninstalledAndNeedsRestart=Para concluir a desinstalação de %1, o computador precisa ser reiniciado.%n%nReiniciar agora?
pt.StatusUninstalling=Desinstalando %1...
pt.ShutdownBlockReasonInstallingApp=Instalando %1.
pt.ShutdownBlockReasonUninstallingApp=Desinstalando %1.

[CustomMessages]
pt.LaunchProgram=Abrir %1
en.StartMenuIcon=Add Ciranda to the Start menu
pt.StartMenuIcon=Adicionar Ciranda ao menu Iniciar
en.DesktopIcon=Add Ciranda to the desktop
pt.DesktopIcon=Adicionar Ciranda à área de trabalho
en.RemoveTitle=Uninstall Ciranda
pt.RemoveTitle=Desinstalar Ciranda
en.RemoveAsk=What should happen to your settings?
pt.RemoveAsk=O que fazer com as suas configurações?
en.RemoveKeep=Keep my settings
pt.RemoveKeep=Manter minhas configurações
en.RemoveKeepNote=Servers, sign-ins, preferences and the keys to your private messages stay on this computer, ready for when Ciranda is installed again.
pt.RemoveKeepNote=Servidores, logins, preferências e as chaves das mensagens privadas ficam neste computador, prontos para uma nova instalação de Ciranda.
en.RemoveAll=Remove everything
pt.RemoveAll=Remover tudo
en.RemoveAllNote=Also deletes %1. Without your recovery code, your private messages can no longer be read.
pt.RemoveAllNote=Também apaga %1. Sem o código de recuperação, as mensagens privadas não poderão mais ser lidas.
en.RemoveGo=Uninstall
pt.RemoveGo=Desinstalar

[Tasks]
Name: "startmenuicon"; Description: "{cm:StartMenuIcon}"
Name: "desktopicon"; Description: "{cm:DesktopIcon}"

[Files]
Source: "{#Root}\{#Build}\ciranda.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[InstallDelete]
; A shortcut left out on reinstall goes, so the choices made now are the ones that hold.
Type: files; Name: "{userprograms}\Ciranda.lnk"
Type: files; Name: "{userdesktop}\Ciranda.lnk"

[UninstallDelete]
; What an update leaves beside the program until the next start.
Type: files; Name: "{app}\ciranda.exe.old"
Type: files; Name: "{app}\ciranda.exe.new"

[Icons]
Name: "{userprograms}\Ciranda"; Filename: "{app}\ciranda.exe"; WorkingDir: "{app}"; Tasks: startmenuicon
Name: "{userdesktop}\Ciranda"; Filename: "{app}\ciranda.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
; Also after a silent install, so a Ciranda closed to install comes back.
Filename: "{app}\ciranda.exe"; Description: "{cm:LaunchProgram,Ciranda}"; Flags: nowait postinstall

[Code]
var
  RemoveEverything: Boolean;

function DataDir: String;
begin
  Result := ExpandConstant('{#DataDir}');
end;

// Asks a running Ciranda to quit the way its menu does, leaving a call properly; one that is
// still there after 15 seconds is closed.
procedure QuitCiranda(Exe: String);
var
  Code: Integer;
begin
  if not FileExists(Exe) then
    exit;
  if Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0) then
    exit;
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM ciranda.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

// Shortcuts named Harmony that start Ciranda under its old name. Pedro's Electron Harmony,
// installed in its own @harmonyclient folder, keeps its own.
procedure RemoveHarmonyShortcut(Link: String);
var
  Shell, Shortcut: Variant;
  Target: String;
begin
  if not FileExists(Link) then
    exit;
  try
    Shell := CreateOleObject('WScript.Shell');
    Shortcut := Shell.CreateShortcut(Link);
    Target := Shortcut.TargetPath;
  except
    exit;
  end;
  if (CompareText(ExtractFileName(Target), 'harmony.exe') = 0) and (Pos('@harmonyclient', Lowercase(Target)) = 0) then
    DeleteFile(Link);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  QuitCiranda(ExpandConstant('{app}\ciranda.exe'));
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    RemoveHarmonyShortcut(ExpandConstant('{userprograms}\Harmony.lnk'));
    RemoveHarmonyShortcut(ExpandConstant('{userdesktop}\Harmony.lnk'));
  end;
end;

// A line under a choice, wrapped to the width given and as tall as its text.
function Note(Form: TSetupForm; Top, Width: Integer; Text: String): TNewStaticText;
begin
  Result := TNewStaticText.Create(Form);
  Result.Parent := Form;
  Result.Left := ScaleX(42);
  Result.Top := Top;
  Result.AutoSize := False;
  Result.WordWrap := True;
  Result.Width := Width - ScaleX(18);
  Result.Caption := Text;
  Result.AdjustHeight;
end;

function Choice(Form: TSetupForm; Top, Width: Integer; Text: String): TNewRadioButton;
begin
  Result := TNewRadioButton.Create(Form);
  Result.Parent := Form;
  Result.Left := ScaleX(24);
  Result.Top := Top;
  Result.Width := Width;
  Result.Caption := Text;
end;

// The uninstaller's first window: keep the settings or remove everything. Keeping them is the
// default, and what a silent uninstall does.
function InitializeUninstall: Boolean;
var
  Form: TSetupForm;
  Ask, KeepNote, AllNote: TNewStaticText;
  Keep, All: TNewRadioButton;
  Go, Cancel: TNewButton;
  Width: Integer;
begin
  RemoveEverything := False;
  Result := True;
  if UninstallSilent then
    exit;
  Form := CreateCustomForm(ScaleX(440), ScaleY(240), True, True);
  try
    Form.Caption := CustomMessage('RemoveTitle');
    Width := Form.ClientWidth - ScaleX(48);

    Ask := TNewStaticText.Create(Form);
    Ask.Parent := Form;
    Ask.Left := ScaleX(24);
    Ask.Top := ScaleY(20);
    Ask.Caption := CustomMessage('RemoveAsk');
    Ask.Font.Style := [fsBold];

    Keep := Choice(Form, Ask.Top + Ask.Height + ScaleY(14), Width, CustomMessage('RemoveKeep'));
    Keep.Checked := True;
    KeepNote := Note(Form, Keep.Top + Keep.Height + ScaleY(2), Width, CustomMessage('RemoveKeepNote'));
    All := Choice(Form, KeepNote.Top + KeepNote.Height + ScaleY(14), Width, CustomMessage('RemoveAll'));
    AllNote := Note(Form, All.Top + All.Height + ScaleY(2), Width, FmtMessage(CustomMessage('RemoveAllNote'), [DataDir]));

    Cancel := TNewButton.Create(Form);
    Cancel.Parent := Form;
    Cancel.Width := ScaleX(90);
    Cancel.Height := ScaleY(26);
    Form.ClientHeight := AllNote.Top + AllNote.Height + ScaleY(28) + Cancel.Height + ScaleY(16);
    Cancel.Left := Form.ClientWidth - ScaleX(24) - Cancel.Width;
    Cancel.Top := Form.ClientHeight - ScaleY(16) - Cancel.Height;
    Cancel.Caption := SetupMessage(msgButtonCancel);
    Cancel.ModalResult := mrCancel;
    Cancel.Cancel := True;

    Go := TNewButton.Create(Form);
    Go.Parent := Form;
    Go.Width := ScaleX(90);
    Go.Height := Cancel.Height;
    Go.Left := Cancel.Left - ScaleX(8) - Go.Width;
    Go.Top := Cancel.Top;
    Go.Caption := CustomMessage('RemoveGo');
    Go.ModalResult := mrOk;
    Go.Default := True;

    Form.ActiveControl := Go;
    Result := Form.ShowModal = mrOk;
    RemoveEverything := All.Checked;
  finally
    Form.Free;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    QuitCiranda(ExpandConstant('{app}\ciranda.exe'));
  if (CurUninstallStep = usPostUninstall) and RemoveEverything and DirExists(DataDir) then
    DelTree(DataDir, True, True, True);
end;
