; Hearthdrive installer hooks (Tauri NSIS).
;
; The background service, nexus-service.exe, runs from the install folder
; whether or not the app's window is open, so the installer stops it before
; replacing its files and starts the new one afterwards. Updates spread
; between devices rely on this: the service starts this installer
; (`/P /UPDATE`), and the installer brings the service back.

; Where NEXUS Cloud (the app's earlier name) is installed, if it is.
Var EarlierInstall
Var NewNameFolder

!macro NEXUS_STOP_SERVICE
  ; Only this Windows account's processes can be stopped without
  ; administrator rights, and only those are meant.
  nsExec::Exec 'taskkill /F /IM nexus-service.exe'
  Pop $0
  Sleep 800
!macroend

; The app was called NEXUS Cloud before. Windows knows an install by the
; app's name, so this installer would otherwise put Hearthdrive next to it.
; Instead it updates that install in its folder, which keeps the program's
; paths the same (its firewall permission, the startup entry and the
; virtual drive's registration point there), renames its shortcuts, and
; drops the old name from Installed apps. The data is not touched.
!macro HEARTHDRIVE_ADOPT_EARLIER_INSTALL
  ReadRegStr $EarlierInstall HKCU "Software\nexuscloud\NEXUS Cloud" ""
  ${If} $EarlierInstall == ""
    StrCpy $EarlierInstall "$LOCALAPPDATA\NEXUS Cloud"
  ${EndIf}
  ${If} ${FileExists} "$EarlierInstall\nexus-desktop.exe"
    StrCpy $NewNameFolder $INSTDIR
    StrCpy $INSTDIR $EarlierInstall
    SetOutPath $INSTDIR
    ; The installer has already made (and worked in) the new name's default
    ; folder. It is still empty; RMDir leaves a folder that is not.
    ${If} $NewNameFolder != $INSTDIR
      RMDir "$NewNameFolder"
    ${EndIf}
    !insertmacro HEARTHDRIVE_RENAME_SHORTCUT "$SMPROGRAMS"
    !insertmacro HEARTHDRIVE_RENAME_SHORTCUT "$DESKTOP"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\NEXUS Cloud"
    DeleteRegKey HKCU "Software\nexuscloud\NEXUS Cloud"
  ${EndIf}
!macroend

!macro HEARTHDRIVE_RENAME_SHORTCUT DIR
  ${If} ${FileExists} "${DIR}\NEXUS Cloud.lnk"
    ${If} ${FileExists} "${DIR}\${PRODUCTNAME}.lnk"
      Delete "${DIR}\NEXUS Cloud.lnk"
    ${Else}
      Rename "${DIR}\NEXUS Cloud.lnk" "${DIR}\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro NEXUS_STOP_SERVICE
  !insertmacro HEARTHDRIVE_ADOPT_EARLIER_INSTALL
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Start the service as the signed-in person (never elevated). It has no
  ; window. A failure here is harmless: the app starts it when opened.
  nsis_tauri_utils::RunAsUser "$INSTDIR\nexus-service.exe" ""
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro NEXUS_STOP_SERVICE
!macroend
