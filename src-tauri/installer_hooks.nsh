; installer_hooks.nsh — Gingify NSIS installer hooks
;
; Tauri's default NSIS cleanup deletes %APPDATA%\<BUNDLEID> (i.e.
; %APPDATA%\app.gingify.desktop) which does NOT match the folder our Rust
; code actually writes to (%APPDATA%\Gingify).
;
; These hooks fix that by cleaning the correct folder:
;
;   NSIS_HOOK_PREINSTALL  — runs before files are copied on any install.
;                           Wipes %APPDATA%\Gingify so every fresh install /
;                           reinstall starts with zero stale state and the
;                           onboarding screen appears again.
;
;   NSIS_HOOK_POSTUNINSTALL — runs after files/registry have been removed.
;                             Deletes %APPDATA%\Gingify so nothing is left
;                             behind after an uninstall.
;
; Both hooks are skipped when Tauri runs in update mode ($UpdateMode = 1) so
; that silent background updates preserve the user's config and history.

!macro NSIS_HOOK_PREINSTALL
  ${If} $UpdateMode <> 1
    SetShellVarContext current
    RMDir /r "$APPDATA\Gingify"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    SetShellVarContext current
    RMDir /r "$APPDATA\Gingify"
  ${EndIf}
!macroend
