; Installer hooks, wired in through bundle.windows.nsis.installerHooks in
; tauri.conf.json and inserted into Tauri's own installer template.

; Up to 0.5.1 the app installed as "jellysic": its own program folder, its own
; uninstall entry, Start-menu shortcut and registry key, all named after the old
; product name. The new name gets a new one of each, so an update from 0.5.x
; would otherwise leave the old copy installed next to the new one. This takes
; it out with its own uninstaller.
;
; The user's data is not touched here: the uninstaller runs silently, which
; leaves "delete app data" unticked, and the app moves its data directories
; over itself on first start (src-tauri/src/legacy.rs).
!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $R0 HKCU "Software\jellysic\jellysic" ""
  ${If} $R0 != ""
  ${AndIf} $R0 != $INSTDIR
  ${AndIf} ${FileExists} "$R0\jellysic.exe"
    ; Keep the shortcuts the old install had. An update (/UPDATE) creates none
    ; of its own, and the old uninstaller below removes the old ones.
    ${If} ${FileExists} "$SMPROGRAMS\jellysic.lnk"
    ${AndIfNot} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}
    ${If} ${FileExists} "$DESKTOP\jellysic.lnk"
    ${AndIfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
      CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}

    ; /S runs it silently (and kills a still running jellysic.exe); _?= keeps
    ; it in place instead of copying itself to TEMP, so ExecWait really waits.
    ; That also means it cannot delete itself.
    ${If} ${FileExists} "$R0\uninstall.exe"
      ExecWait '"$R0\uninstall.exe" /S _?=$R0'
      Delete "$R0\uninstall.exe"
    ${EndIf}
    RMDir "$R0"
  ${EndIf}
  ${If} $R0 != ""
    ; Holds the old install location and installer language. The uninstaller
    ; keeps it unless app data is deleted.
    DeleteRegKey HKCU "Software\jellysic\jellysic"
    DeleteRegKey /ifempty HKCU "Software\jellysic"
  ${EndIf}
!macroend
