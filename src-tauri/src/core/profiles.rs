//! Operational profile management — handles Work / Gaming / Focus / Custom
//! mode configs and applies the appropriate trim aggressiveness settings.

#![allow(dead_code)]

use crate::state::app_state::{Profile, SharedState, TrimTrigger};
use crate::core::suspender;
use crate::core::trimmer;
use crate::core::bloat_scan;

use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

// ---------------------------------------------------------------------------
// Elevation check (mirrors is_admin in lib.rs — duplicated to avoid circular dep)
// ---------------------------------------------------------------------------

/// Returns true if the current process is running with administrator privileges.
fn is_process_elevated() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut return_length: u32 = 0;
        let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut std::ffi::c_void),
            size,
            &mut return_length,
        ).is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

// ---------------------------------------------------------------------------
// ProfileConfig — resolved settings the monitor uses
// ---------------------------------------------------------------------------

/// Fully-resolved configuration for the active profile.
/// The monitor and trimmer consume this instead of calling `AppState.active_profile`
/// directly, ensuring a consistent view for an entire poll cycle.
#[derive(Debug, Clone)]
pub struct ProfileConfig {
    /// Minimum idle time (seconds) before a process is eligible for trimming.
    pub idle_threshold_secs: u64,
    /// Whether NtSuspendProcess is permitted.
    pub use_hard_suspend: bool,
    /// Whether all known bloat services are suspended automatically on activation.
    pub suspend_bloat_on_activate: bool,
    /// RAM pressure percentage threshold that triggers auto-trim (0.0–100.0).
    pub auto_trim_threshold_pct: f32,
}

// ---------------------------------------------------------------------------
// resolve_profile
// ---------------------------------------------------------------------------

/// Map a `Profile` variant to its resolved `ProfileConfig`.
///
/// Hardcoded values for Work / Gaming / Focus per ARCHITECTURE_DESKTOP.md §3.6.
/// `Custom` reads values directly from the `CustomProfile` struct.
pub fn resolve_profile(profile: &Profile) -> ProfileConfig {
    match profile {
        Profile::Work => ProfileConfig {
            idle_threshold_secs: 900,    // 15 min
            use_hard_suspend: false,
            suspend_bloat_on_activate: false,
            auto_trim_threshold_pct: 80.0,
        },
        Profile::Gaming => ProfileConfig {
            idle_threshold_secs: 300,    // 5 min
            use_hard_suspend: true,
            suspend_bloat_on_activate: true,
            auto_trim_threshold_pct: 60.0,
        },
        Profile::Focus => ProfileConfig {
            idle_threshold_secs: 600,    // 10 min
            use_hard_suspend: false,
            suspend_bloat_on_activate: false,
            auto_trim_threshold_pct: 75.0,
        },
        Profile::Custom(cp) => ProfileConfig {
            idle_threshold_secs: cp.idle_threshold_secs,
            use_hard_suspend: cp.use_hard_suspend,
            suspend_bloat_on_activate: false,
            auto_trim_threshold_pct: 80.0,
        },
    }
}

// ---------------------------------------------------------------------------
// activate_profile
// ---------------------------------------------------------------------------

/// Switch to a new operational profile, applying all side-effects.
///
/// - Stores `profile` in `AppState.active_profile` and `UserConfig.active_profile`.
/// - Saves config to disk.
/// - If switching **to** `Gaming`:
///   - FIX: checks admin elevation when `hard_suspend_enabled` is true — returns Err if not elevated.
///   - Calls `soft_trim_all` with the Gaming profile's idle threshold.
///   - If `use_hard_suspend == true` and `suspend_bloat_on_activate == true`:
///     calls `suspend_all_bloat` for all running bloat services.
/// - If switching **from** `Gaming` to any other profile:
///   - Calls `suspender::resume_all_suspended`.
///
/// Emits **no** Tauri event here — the monitor loop's next `gingify://ram-update`
/// carries the updated profile data to the frontend.
pub fn activate_profile(
    new_profile: Profile,
    state: SharedState,
    suspender_ctx: &suspender::SuspenderContext,
) -> Result<(), String> {
    // --- 1. Snapshot previous profile before locking for writes ----------------
    let prev_profile = {
        let st = state.lock();
        st.active_profile.clone()
    };

    // Gaming Mode always uses hard suspend — always requires admin
    if new_profile == Profile::Gaming && !is_process_elevated() {
        return Err(
            "Gaming Mode requires administrator privileges (hard suspend is always enabled)".to_string()
        );
    }

    let resolved = resolve_profile(&new_profile);
    let profile_name = profile_to_name(&new_profile).to_owned();

    // --- 1b. Focus Mode: snapshot current settings and apply Focus hardcoded values.
    //         Done before step 3 (save) so the snapshot + overrides are persisted.
    if new_profile == Profile::Focus {
        let mut st = state.lock();
        // Only snapshot once (don't clobber an existing snapshot)
        if st.config.pre_focus_idle_threshold.is_none() {
            st.config.pre_focus_idle_threshold = Some(st.config.idle_threshold_secs);
            st.config.pre_focus_hard_suspend   = Some(st.config.hard_suspend_enabled);
        }
        st.config.idle_threshold_secs  = resolved.idle_threshold_secs; // 600
        st.config.hard_suspend_enabled = false;
    }

    // --- 2. Persist the profile change into AppState + UserConfig --------------
    {
        let mut st = state.lock();
        st.active_profile = new_profile.clone();
        st.config.active_profile = profile_name.clone();
    }

    // --- 3. Save config to disk ------------------------------------------------
    let config_to_save = {
        let st = state.lock();
        st.config.clone()
    };
    if let Err(e) = config_to_save.save() {
        log::warn!("profiles: failed to save config after profile change: {e}");
    }

    // --- 4a. Side-effects when activating or leaving Focus mode ----------------
    if new_profile == Profile::Focus {
        // FIX: record the foreground app as session-excluded for this Focus session
        let fg_name = get_foreground_process_name(&state);
        {
            let mut st = state.lock();
            st.session_excluded = fg_name.clone();
        }
        if let Some(name) = &fg_name {
            log::info!("profiles: Focus Mode — session-excluding foreground app '{name}'");
        }
        // Snooze idle apps immediately on Focus activation
        trimmer::soft_trim_all(&state, resolved.idle_threshold_secs, TrimTrigger::Manual);
    }

    // Restore pre-Focus settings when leaving Focus for any other profile
    if prev_profile == Profile::Focus && new_profile != Profile::Focus {
        let restored_config = {
            let mut st = state.lock();
            st.session_excluded = None;
            if let Some(idle) = st.config.pre_focus_idle_threshold.take() {
                st.config.idle_threshold_secs = idle;
            }
            if let Some(hs) = st.config.pre_focus_hard_suspend.take() {
                st.config.hard_suspend_enabled = hs;
            }
            st.config.clone()
        };
        if let Err(e) = restored_config.save() {
            log::warn!("profiles: failed to save restored config after leaving Focus: {e}");
        }
        log::info!("profiles: exited Focus Mode — session exclusion cleared, pre-Focus settings restored");
    }

    // --- 4. Side-effects when activating Gaming mode ---------------------------
    if new_profile == Profile::Gaming {
        // Soft-trim all idle processes with Gaming idle threshold
        let result = trimmer::soft_trim_all(
            &state,
            resolved.idle_threshold_secs,
            TrimTrigger::GamingMode,
        );
        log::info!(
            "profiles: Gaming Mode activated — trimmed {} processes, freed {} bytes",
            result.processes_trimmed,
            result.freed_bytes
        );

        // Auto-suspend all known bloat if both flags are set
        if resolved.use_hard_suspend && resolved.suspend_bloat_on_activate {
            suspend_all_bloat_with_ctx(&state, suspender_ctx);
        }
    }

    // --- 5. Side-effects when leaving Gaming mode ------------------------------
    if prev_profile == Profile::Gaming && new_profile != Profile::Gaming {
        suspender::resume_all_suspended(suspender_ctx, state.clone());
        log::info!("profiles: exited Gaming Mode — all suspended processes resumed");
    }

    log::info!(
        "profiles: activated profile \"{profile_name}\" (was \"{}\")",
        profile_to_name(&prev_profile)
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// suspend_all_bloat (public — used by suspend_all_bloat IPC command)
// ---------------------------------------------------------------------------

/// Hard-suspend every currently-running process that matches any known bloat definition.
///
/// Requires the `SuspenderContext` to be already loaded. Logs per-PID failures
/// but never aborts — always attempts all categories.
pub fn suspend_all_bloat(
    state: &SharedState,
    ctx: &suspender::SuspenderContext,
) {
    suspend_all_bloat_with_ctx(state, ctx);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn suspend_all_bloat_with_ctx(
    state: &SharedState,
    ctx: &suspender::SuspenderContext,
) {
    let names: Vec<&'static str> = bloat_scan::known_bloat_names().collect();

    for name in names {
        let pids: Vec<u32> = {
            let st = state.lock();
            if bloat_scan::get_bloat_pids(name, &st).is_empty() {
                continue;
            }
            bloat_scan::get_bloat_pids(name, &st)
        };

        for pid in pids {
            if let Err(e) = suspender::hard_suspend(ctx, pid, state.clone()) {
                log::warn!(
                    "profiles: suspend_all_bloat — PID {pid} ({name}) failed: {e}"
                );
            }
        }
    }
}

/// Returns the executable name (e.g. "chrome.exe") of the foreground window's process,
/// or `None` if detection fails.
fn get_foreground_process_name(state: &SharedState) -> Option<String> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 as isize == 0 {
        return None;
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)); }
    if pid == 0 {
        return None;
    }
    // Look up the name in process_map (already populated by monitor)
    let st = state.lock();
    st.process_map.get(&pid).map(|e| e.name.clone())
}

/// Convert a `Profile` variant to its canonical display name string.
pub fn profile_to_name(profile: &Profile) -> &str {
    match profile {
        Profile::Work => "Work",
        Profile::Gaming => "Gaming",
        Profile::Focus => "Focus",
        Profile::Custom(cp) => cp.name.as_str(),
    }
}

