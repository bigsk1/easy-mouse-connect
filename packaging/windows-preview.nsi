; Small per-user installer for an already-built Windows test executable.
; Normal native releases use the Tauri installer in scripts/build-windows.ps1.
Unicode true
!include "MUI2.nsh"
!include "x64.nsh"

!ifndef APP_BINARY
  !error "APP_BINARY must name the compiled Windows x64 executable"
!endif
!ifndef SETUP_OUT
  !error "SETUP_OUT must name the installer output"
!endif
!ifndef TEST_README
  !error "TEST_README must name the test instructions"
!endif
!ifndef APP_ICON
  !error "APP_ICON must name the application .ico"
!endif
!ifndef APP_VERSION
  !error "APP_VERSION must match the compiled application version"
!endif

Name "Easy Mouse Connect Preview"
OutFile "${SETUP_OUT}"
InstallDir "$LOCALAPPDATA\Programs\EasyMouseConnectPreview"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "${APP_ICON}"
UninstallIcon "${APP_ICON}"
VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey "ProductName" "Easy Mouse Connect Preview"
VIAddVersionKey "FileDescription" "Easy Mouse Connect Preview Setup"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "LegalCopyright" ""
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_TEXT "The preview is installed for your Windows account. Microsoft Edge WebView2 Runtime is required. See START-HERE.txt before pairing your two PCs. Use Quit Easy Mouse Connect in the tray before installing an update; closing its window hides it."
!define MUI_FINISHPAGE_RUN "$INSTDIR\easy-mouse-connect.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open Easy Mouse Connect"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "This preview requires 64-bit Windows."
    Abort
  ${EndIf}
FunctionEnd

Section "Application"
  SectionIn RO
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File /oname=easy-mouse-connect.exe "${APP_BINARY}"
  File /oname=START-HERE.txt "${TEST_README}"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Easy Mouse Connect Preview"
  CreateShortcut "$SMPROGRAMS\Easy Mouse Connect Preview\Easy Mouse Connect.lnk" "$INSTDIR\easy-mouse-connect.exe"
  CreateShortcut "$SMPROGRAMS\Easy Mouse Connect Preview\Test instructions.lnk" "$INSTDIR\START-HERE.txt"
  CreateShortcut "$SMPROGRAMS\Easy Mouse Connect Preview\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview" "DisplayName" "Easy Mouse Connect Preview"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview" "DisplayIcon" "$INSTDIR\easy-mouse-connect.exe"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview" "NoRepair" 1
SectionEnd

Section "Desktop shortcut"
  SetShellVarContext current
  CreateShortcut "$DESKTOP\Easy Mouse Connect.lnk" "$INSTDIR\easy-mouse-connect.exe"
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$DESKTOP\Easy Mouse Connect.lnk"
  Delete "$SMPROGRAMS\Easy Mouse Connect Preview\Easy Mouse Connect.lnk"
  Delete "$SMPROGRAMS\Easy Mouse Connect Preview\Test instructions.lnk"
  Delete "$SMPROGRAMS\Easy Mouse Connect Preview\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Easy Mouse Connect Preview"
  Delete "$INSTDIR\easy-mouse-connect.exe"
  Delete "$INSTDIR\START-HERE.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\EasyMouseConnectPreview"
  ; Deliberately preserve user identity/settings and received documents.
SectionEnd
