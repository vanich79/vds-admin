; NSIS installer for VDS Admin.
;
; Built by scripts/build-windows.sh --installer, which supplies VERSION, BINARY and
; OUTDIR on the command line:
;
;   makensis -DVERSION=0.1.0 -DBINARY=... -DOUTDIR=... packaging/windows/installer.nsi
;
; NSIS rather than WiX/MSI: the application is a single self-contained executable with no
; services, no COM registration and no shared components. An MSI's transactional install
; buys nothing here, and NSIS cross-compiles from Linux, which keeps a CI option open.

Unicode true

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef BINARY
  !error "BINARY was not defined; run this through scripts/build-windows.sh"
!endif
!ifndef OUTDIR
  !define OUTDIR "."
!endif

!define APP_NAME    "VDS Admin"
!define APP_EXE     "vds-admin.exe"
!define PUBLISHER   "VDS Admin"
!define REG_KEY     "Software\Microsoft\Windows\CurrentVersion\Uninstall\VDSAdmin"

Name "${APP_NAME}"
OutFile "${OUTDIR}\vds-admin-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\VDS Admin"
InstallDirRegKey HKCU "Software\VDSAdmin" "InstallDir"

; Per-user by default: no elevation prompt, and the application needs no machine-wide
; state. An administrator who wants it for everyone can still run it elevated.
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "FileDescription" "Server, website and traffic monitoring"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "MIT licensed"

!include "MUI2.nsh"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP_NAME}"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ; Installing over a running copy leaves a locked file and a half-updated install.
  FindWindow $0 "" "${APP_NAME}"
  StrCmp $0 0 continue
    MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION \
      "${APP_NAME} is running. Close it before continuing." IDOK continue
    Abort
  continue:
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR"
  File /oname=${APP_EXE} "${BINARY}"

  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"

  WriteRegStr HKCU "Software\VDSAdmin" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${REG_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${REG_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${REG_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${REG_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "${REG_KEY}" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegDWORD HKCU "${REG_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${REG_KEY}" "NoRepair" 1

  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"

  DeleteRegKey HKCU "${REG_KEY}"
  DeleteRegKey HKCU "Software\VDSAdmin"

  ; The database, configuration and stored credentials in %APPDATA%\vds-admin are
  ; deliberately left alone. Uninstalling an application must not silently destroy the
  ; user's server list, and the secrets in Credential Manager are not ours to remove
  ; without asking.
  MessageBox MB_OK|MB_ICONINFORMATION \
    "Your servers, settings and stored credentials were kept.$\r$\n$\r$\n\
     To remove them as well, delete:$\r$\n$\r$\n    %APPDATA%\vds-admin$\r$\n$\r$\n\
     and remove the 'vds-admin' entries from Windows Credential Manager."
SectionEnd
