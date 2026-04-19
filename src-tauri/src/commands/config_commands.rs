//! IPC commands for config management — get_config, update_config,
//! add_exclusion, remove_exclusion callable from the frontend via invoke().

#![allow(dead_code)]

use tauri::{AppHandle, Emitter, State};

use crate::core::notifications;
use crate::core::suspender;
use crate::state::app_state::SharedState;
use crate::state::config::UserConfig;

// ---------------------------------------------------------------------------
// Protected process names — these cannot be added to the exclusion list
// (they are already skip-protected at the Rust level, but be defensive)
// ---------------------------------------------------------------------------

const PROTECTED_PROCESSES: &[&str] = &[
    "explorer.exe",
    "lsass.exe",
    "winlogon.exe",
    "csrss.exe",
    "svchost.exe",
    "dwm.exe",
    "wininit.exe",
    "services.exe",
    "smss.exe",
    "registry",
    "system",
    "secure system",
    "gingify.exe",
];

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Return the current user config to the frontend.
#[tauri::command]
pub async fn get_config(state: State<'_, SharedState>) -> Result<UserConfig, String> {
    let st = state.lock();
    Ok(st.config.clone())
}

/// Merge a partial JSON config update into the current config and persist it.
///
/// `partial_json` is a JSON object — only keys that are present will be merged.
/// Validates range constraints on numeric fields before applying.
#[tauri::command]
pub async fn update_config(
    partial_json: String,
    state: State<'_, SharedState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    // Parse the partial JSON
    let partial: serde_json::Value = serde_json::from_str(&partial_json)
        .map_err(|e| format!("Invalid JSON: {e}"))?;

    let obj = partial
        .as_object()
        .ok_or_else(|| "Expected a JSON object".to_string())?;

    // Take a snapshot of the current config to merge into
    let mut config = {
        let st = state.lock();
        st.config.clone()
    };

    // Merge fields one by one with validation
    for (key, value) in obj {
        match key.as_str() {
            "auto_trim_enabled" => {
                config.auto_trim_enabled = value
                    .as_bool()
                    .ok_or_else(|| "Invalid value for auto_trim_enabled: expected bool".to_string())?;
            }
            "auto_trim_threshold_pct" => {
                let v = value
                    .as_u64()
                    .ok_or_else(|| "Invalid value for auto_trim_threshold_pct: expected u8".to_string())?
                    as u8;
                if !(50..=95).contains(&v) {
                    return Err(format!(
                        "Invalid value for auto_trim_threshold_pct: {v} is out of range 50–95"
                    ));
                }
                config.auto_trim_threshold_pct = v;
            }
            "idle_threshold_secs" => {
                let v = value
                    .as_u64()
                    .ok_or_else(|| "Invalid value for idle_threshold_secs: expected u64".to_string())?;
                if !(60..=3600).contains(&v) {
                    return Err(format!(
                        "Invalid value for idle_threshold_secs: {v} is out of range 60–3600"
                    ));
                }
                config.idle_threshold_secs = v;
            }
            "active_profile" => {
                config.active_profile = value
                    .as_str()
                    .ok_or_else(|| "Invalid value for active_profile: expected string".to_string())?
                    .to_string();
            }
            "hard_suspend_enabled" => {
                config.hard_suspend_enabled = value
                    .as_bool()
                    .ok_or_else(|| "Invalid value for hard_suspend_enabled: expected bool".to_string())?;
            }
            "start_on_login" => {
                let enabled = value
                    .as_bool()
                    .ok_or_else(|| "Invalid value for start_on_login: expected bool".to_string())?;
                config.start_on_login = enabled;
                apply_startup_registry(enabled);
            }
            "notifications_enabled" => {
                config.notifications_enabled = value
                    .as_bool()
                    .ok_or_else(|| "Invalid value for notifications_enabled: expected bool".to_string())?;
            }
            "theme" => {
                let t = value
                    .as_str()
                    .ok_or_else(|| "Invalid value for theme: expected string".to_string())?;
                if !["system", "dark", "light"].contains(&t) {
                    return Err("Invalid value for theme: must be system, dark, or light".to_string());
                }
                config.theme = t.to_string();
            }
            "trim_history_limit" => {
                let v = value
                    .as_u64()
                    .ok_or_else(|| "Invalid value for trim_history_limit: expected usize".to_string())?
                    as usize;
                config.trim_history_limit = v;
            }
            "first_launch_complete" => {
                config.first_launch_complete = value
                    .as_bool()
                    .ok_or_else(|| "Invalid value for first_launch_complete: expected bool".to_string())?;
            }
            unknown => {
                log::debug!("config: ignoring unknown field '{unknown}' in partial update");
            }
        }
    }

    // Persist to disk
    config.save().map_err(|e| format!("Failed to save config: {e}"))?;

    // Update shared state
    {
        let mut st = state.lock();
        st.config = config.clone();
    }

    // Notify all windows that config changed (theme reload, etc.)
    if let Err(e) = app_handle.emit("gingify://config-changed", &config) {
        log::warn!("config: failed to emit config-changed event: {e}");
    }

    log::info!("config: updated and saved");
    Ok(())
}

/// Add a process name to the exclusion list and persist the config.
///
/// Normalises to lowercase. Refuses to exclude protected system processes.
#[tauri::command]
pub async fn add_exclusion(
    name: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let normalised = name.to_lowercase();

    // Defensive guard — protected processes cannot be excluded (Rust ignores them anyway)
    if PROTECTED_PROCESSES.contains(&normalised.as_str()) {
        return Err(format!(
            "Cannot exclude protected system process '{normalised}'"
        ));
    }

    let mut config = {
        let st = state.lock();
        st.config.clone()
    };

    if !config.excluded_processes.contains(&normalised) {
        config.excluded_processes.push(normalised.clone());
    }

    config.save().map_err(|e| format!("Failed to save config: {e}"))?;

    {
        let mut st = state.lock();
        st.config = config;
    }

    log::info!("config: added exclusion '{normalised}'");
    Ok(())
}

/// Remove a process name from the exclusion list and persist the config.
#[tauri::command]
pub async fn remove_exclusion(
    name: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let normalised = name.to_lowercase();

    let mut config = {
        let st = state.lock();
        st.config.clone()
    };

    config.excluded_processes.retain(|n| n != &normalised);

    config.save().map_err(|e| format!("Failed to save config: {e}"))?;

    {
        let mut st = state.lock();
        st.config = config;
    }

    log::info!("config: removed exclusion '{normalised}'");
    Ok(())
}

/// Check whether NtSuspendProcess is loadable on this system.
///
/// Returns `true` if hard suspend is available, `false` if resolution fails.
/// Called by the frontend before enabling the hard-suspend toggle.
#[tauri::command]
pub fn verify_suspend_capable() -> bool {
    // FIX: test NtSuspendProcess load; returns false if ntdll resolution fails
    suspender::load_suspender().is_ok()
}

/// Fire the one-time welcome notification after first-launch onboarding completes.
///
/// Called from `onboarding.js` via `invoke('trigger_welcome_notification')`.
#[tauri::command]
pub async fn trigger_welcome_notification() -> Result<(), String> {
    notifications::notify_welcome();
    log::info!("config: welcome notification fired");
    Ok(())
}

// ---------------------------------------------------------------------------
// Startup registry helper — HKCU\Software\Microsoft\Windows\CurrentVersion\Run
// ---------------------------------------------------------------------------

/// Write or remove the "Gingify" run-key so the app auto-starts with Windows.
///
/// Uses `HKCU` (no admin required). Safe to call with `enabled = false` even
/// when the key doesn't exist — the delete is a no-op in that case.
fn apply_startup_registry(enabled: bool) {
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    let run_key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    let value_name: Vec<u16> = "Gingify\0".encode_utf16().collect();

    let mut hkey = windows::Win32::System::Registry::HKEY::default();

    let err = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            windows::core::PCWSTR(run_key.as_ptr()),
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        )
    };

    if err != ERROR_SUCCESS {
        log::warn!("apply_startup_registry: failed to open Run key (error {})", err.0);
        return;
    }

    if enabled {
        let exe_path = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                log::warn!("apply_startup_registry: could not get exe path: {e}");
                unsafe { let _ = RegCloseKey(hkey); }
                return;
            }
        };
        let exe_wide: Vec<u16> = exe_path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let set_err = unsafe {
            RegSetValueExW(
                hkey,
                windows::core::PCWSTR(value_name.as_ptr()),
                Some(0),
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    exe_wide.as_ptr() as *const u8,
                    exe_wide.len() * 2,
                )),
            )
        };
        if set_err != ERROR_SUCCESS {
            log::warn!("apply_startup_registry: RegSetValueExW failed (error {})", set_err.0);
        } else {
            log::info!("apply_startup_registry: set Run key → {:?}", exe_path);
        }
    } else {
        // Ignore ERROR_FILE_NOT_FOUND — key may already be absent
        unsafe {
            let _ = RegDeleteValueW(hkey, windows::core::PCWSTR(value_name.as_ptr()));
        }
        log::info!("apply_startup_registry: removed Run key (or already absent)");
    }

    unsafe { let _ = RegCloseKey(hkey); }
}
