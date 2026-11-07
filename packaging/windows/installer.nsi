!include "MUI2.nsh"

Name "Local Player RS"
OutFile "..\..\local-player-rs-setup.exe"
InstallDir "$PROGRAMFILES64\Local Player RS"
RequestExecutionLevel admin

!define VERSION "0.1.0"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

; ESSENCIAL: sem isto as páginas MUI ficam em branco (sem strings de idioma).
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"
  File "..\..\target\release\local-player-rs.exe"

  ; Atalhos no contexto do USUÁRIO (não do admin elevado).
  SetShellVarContext current

  ; Menu Iniciar
  CreateDirectory "$SMPROGRAMS\Local Player RS"
  CreateShortCut "$SMPROGRAMS\Local Player RS\Local Player RS.lnk" "$INSTDIR\local-player-rs.exe"
  ; Desktop
  CreateShortCut "$DESKTOP\Local Player RS.lnk" "$INSTDIR\local-player-rs.exe"

  ; Registro de desinstalação (Adicionar/Remover programas)
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS" "DisplayName" "Local Player RS"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS" "Publisher" "Local Player RS contributors"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS" "DisplayIcon" "$INSTDIR\local-player-rs.exe"
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS" "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS" "NoRepair" 1

  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  ; Remove atalhos no contexto do usuário
  SetShellVarContext current
  Delete "$SMPROGRAMS\Local Player RS\Local Player RS.lnk"
  Delete "$DESKTOP\Local Player RS.lnk"
  RMDir "$SMPROGRAMS\Local Player RS"

  ; Remove arquivos
  Delete "$INSTDIR\local-player-rs.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  ; Remove registro
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Local Player RS"
SectionEnd
