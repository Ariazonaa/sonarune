; Installer hooks, wired in through bundle.windows.nsis.installerHooks in
; tauri.conf.json and inserted into Tauri's own installer template.

!macro NSIS_HOOK_POSTINSTALL
  ; Remember the installer language in every install mode. MUI stores it only
  ; when the progress page is left, which a silent install (/S, winget's
  ; default) never shows; the passive in-app update would then ask for the
  ; language again -- after the app has quit, and Cancel there leaves it
  ; neither updated nor running.
  WriteRegStr "${MUI_LANGDLL_REGISTRY_ROOT}" "${MUI_LANGDLL_REGISTRY_KEY}" "${MUI_LANGDLL_REGISTRY_VALUENAME}" $LANGUAGE

  ; Up to 0.5.1 the app installed as "jellysic": its own program folder, its
  ; own uninstall entry, Start-menu shortcut and registry key, all named after
  ; the old product name. The new name gets a new one of each, so an update
  ; from 0.5.x would otherwise leave the old copy installed next to the new
  ; one. This takes it out with its own uninstaller.
  ;
  ; The user's data is not touched here: the uninstaller runs silently, which
  ; leaves "delete app data" unticked, and the app moves its data directories
  ; over itself on first start (src-tauri/src/legacy.rs).
  ReadRegStr $R0 HKCU "Software\jellysic\jellysic" ""
  ${If} $R0 != ""
  ${AndIf} ${FileExists} "$R0\jellysic.exe"
    ; Keep the shortcuts the old install had. An update (/UPDATE) creates none
    ; of its own, and the old ones go below.
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

    ${If} $R0 != $INSTDIR
      ; /S runs it silently (and kills a still running jellysic.exe); _?= keeps
      ; it in place instead of copying itself to TEMP, so ExecWait really waits
      ; and gets its exit code. That also means it cannot delete itself. It
      ; aborts (exit code 2) when it cannot end a running jellysic.exe; then
      ; everything stays for the next install to try again.
      ${If} ${FileExists} "$R0\uninstall.exe"
        StrCpy $R1 1
        ExecWait '"$R0\uninstall.exe" /S _?=$R0' $R1
        ${If} $R1 = 0
          Delete "$R0\uninstall.exe"
        ${EndIf}
      ${EndIf}
      RMDir "$R0"
    ${Else}
      ; Installed into the old folder itself. Our uninstall.exe has just
      ; replaced the old one, so running "it" would remove this install: do by
      ; hand what the old uninstaller would have done. Otherwise its Apps &
      ; Features entry stays and starts our uninstaller, and its shortcuts keep
      ; launching 0.5.
      Delete "$R0\jellysic.exe"
      ${IfNot} ${FileExists} "$R0\jellysic.exe"
        !insertmacro IsShortcutTarget "$SMPROGRAMS\jellysic.lnk" "$R0\jellysic.exe"
        Pop $R1
        ${If} $R1 = 1
          !insertmacro UnpinShortcut "$SMPROGRAMS\jellysic.lnk"
          Delete "$SMPROGRAMS\jellysic.lnk"
        ${EndIf}
        !insertmacro IsShortcutTarget "$DESKTOP\jellysic.lnk" "$R0\jellysic.exe"
        Pop $R1
        ${If} $R1 = 1
          Delete "$DESKTOP\jellysic.lnk"
        ${EndIf}
        DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\jellysic"
      ${EndIf}
    ${EndIf}
  ${EndIf}
  ; Holds the old install location and installer language; the uninstaller
  ; keeps it unless app data is deleted. Dropped only once the old program is
  ; really gone, so a removal that failed is tried again by the next install.
  ${If} $R0 != ""
  ${AndIfNot} ${FileExists} "$R0\jellysic.exe"
    DeleteRegKey HKCU "Software\jellysic\jellysic"
    DeleteRegKey /ifempty HKCU "Software\jellysic"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; "Delete app data" takes the two data folders, further down in the
  ; template. The access token, the password and the ListenBrainz token are
  ; kept in the Windows Credential Manager instead, so the app removes its own
  ; entries there, and nothing else (src-tauri/src/credential_purge.rs). That
  ; has to happen here, while the program is still in place.
  ;
  ; Never in an update. The box is read when the confirm page is left, which
  ; /P and /S skip, and the in-app update (/P /UPDATE) installs over the old
  ; copy without running its uninstaller at all; $UpdateMode is tested anyway,
  ; as the template does before it deletes the data folders.
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; Close a running copy first, as the template does right after this hook,
    ; so it cannot write a credential back. Cancel in its prompt ends the
    ; uninstall before anything is removed.
    !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
    ; Only the exe of this very build knows the switch. An older one (an
    ; upgrade that could not replace it, a copy put there by hand) would start
    ; as the app and keep the uninstaller waiting until it is closed. Tauri
    ; gives the exe the same four-part version as the installer.
    StrCpy $R0 1
    ClearErrors
    ${GetFileVersion} "$INSTDIR\${MAINBINARYNAME}.exe" $R1
    ${IfNot} ${Errors}
    ${AndIf} $R1 == "${VERSIONWITHBUILD}"
      ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --purge-credentials' $R0
      ${IfThen} ${Errors} ${|} StrCpy $R0 1 ${|}
    ${EndIf}
    ; Best effort: the uninstall goes on, with a line in the details list.
    ${If} $R0 <> 0
      ${If} $LANGUAGE = ${LANG_GERMAN}
        DetailPrint "Die gespeicherten Zugangsdaten konnten nicht alle entfernt werden."
      ${Else}
        DetailPrint "Not all saved credentials could be removed."
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend
