; NEXUS Cloud installer hooks (Tauri NSIS).
;
; The background service, nexus-service.exe, runs from the install folder
; whether or not the app's window is open, so the installer stops it before
; replacing its files and starts the new one afterwards. Updates spread
; between devices rely on this: the service starts this installer
; (`/P /UPDATE`), and the installer brings the service back.

!macro NEXUS_STOP_SERVICE
  ; Only this Windows account's processes can be stopped without
  ; administrator rights, and only those are meant.
  nsExec::Exec 'taskkill /F /IM nexus-service.exe'
  Pop $0
  Sleep 800
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro NEXUS_STOP_SERVICE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Start the service as the signed-in person (never elevated). It has no
  ; window. A failure here is harmless: the app starts it when opened.
  nsis_tauri_utils::RunAsUser "$INSTDIR\nexus-service.exe" ""
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro NEXUS_STOP_SERVICE
!macroend
