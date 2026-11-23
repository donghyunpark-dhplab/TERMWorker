; TermWorker NSIS installer hooks (bundle.windows.nsis.installerHooks).
;
; The installer is per-machine (C:\Program Files\TermWorker). It also:
;  1. removes a per-user install left by 0.3.0 (%LOCALAPPDATA%\TermWorker), so the
;     PC does not end up with two copies; settings in %APPDATA%\com.dhplab.termworker stay;
;  2. adds the install folder to the system PATH, and takes it out again on uninstall.
;
; NSIS strings are limited to 1024 characters here, and the system PATH can be longer,
; so PATH is edited by PowerShell straight in the registry (keeping REG_EXPAND_SZ and the
; unexpanded %...% entries), never through an NSIS string.

Var TwOldDir
Var TwOldRet

; Adds (MODE = add) or removes (MODE = remove) $INSTDIR in the machine PATH.
!macro TW_UpdatePath MODE
  System::Call 'Kernel32::SetEnvironmentVariable(t "TW_DIR", t "$INSTDIR")'
  System::Call 'Kernel32::SetEnvironmentVariable(t "TW_PATH_MODE", t "${MODE}")'
  nsExec::ExecToLog `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$d=$$env:TW_DIR.TrimEnd('\'); $$k=[Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Control\Session Manager\Environment', $$true); $$p=[string]$$k.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); $$a=@($$p.Split(';') | Where-Object { $$_ -and $$_.TrimEnd('\') -ne $$d }); if ($$env:TW_PATH_MODE -eq 'add') { $$a += $$d }; $$n=$$a -join ';'; if ($$n -ne $$p) { $$k.SetValue('Path', $$n, [Microsoft.Win32.RegistryValueKind]::ExpandString) }; $$k.Close()"`
  Pop $0
  ${If} $0 != 0
    DetailPrint "Could not update the system PATH ($0)."
  ${EndIf}
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; A per-user install (0.3.0) has its uninstall entry and location under HKCU.
  ReadRegStr $TwOldRet HKCU "${UNINSTKEY}" "UninstallString"
  ReadRegStr $TwOldDir HKCU "${MANUPRODUCTKEY}" ""
  ${If} $TwOldRet != ""
  ${AndIf} $TwOldDir != ""
  ${AndIf} $TwOldDir != $INSTDIR
  ${AndIf} ${FileExists} "$TwOldDir\uninstall.exe"
    ; Ask before closing a running copy, as the installer does for its own folder.
    !insertmacro CheckIfAppIsRunning "$TwOldDir\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
    DetailPrint "Removing the previous per-user installation in $TwOldDir"
    ; _?= runs the uninstaller in place, so ExecWait waits for it; app data is kept.
    ExecWait '"$TwOldDir\uninstall.exe" /S _?=$TwOldDir' $TwOldRet
    ${If} $TwOldRet = 0
      Delete "$TwOldDir\uninstall.exe"
      RMDir "$TwOldDir"
      ; A silent uninstall keeps the old install location under HKCU; drop it.
      DeleteRegValue HKCU "${MANUPRODUCTKEY}" ""
      DeleteRegKey /ifempty HKCU "${MANUPRODUCTKEY}"
      DeleteRegKey /ifempty HKCU "${MANUKEY}"
    ${Else}
      DetailPrint "The previous installation could not be removed ($TwOldRet)."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro TW_UpdatePath add
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro TW_UpdatePath remove
!macroend
