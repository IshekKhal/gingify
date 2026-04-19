//! IPC commands for profile management — set_profile, get_current_profile
//! callable from the frontend via invoke().

#![allow(dead_code)]

use tauri::State;

use crate::state::app_state::{Profile, SharedState};
use crate::core::profiles;
use crate::core::suspender;

/// Switch the active operational profile.
///
/// Accepts `"Work"`, `"Gaming"`, or `"Focus"` (case-insensitive).
/// Calls `profiles::activate_profile` which handles all side-effects:
/// soft-trim on Gaming activation, resume-all on Gaming exit, and config persistence.
#[tauri::command]
pub async fn set_profile(
    profile: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let new_profile = Profile::from_name(&profile);

    let shared = state.inner().clone();

    // We need a SuspenderContext to pass to activate_profile.
    // Extract a raw pointer (valid for AppState Arc lifetime).
    let ctx_ptr: *const suspender::SuspenderContext = {
        let st = shared.lock();
        match st.suspender_ctx.as_ref() {
            Some(ctx) => ctx as *const _,
            None => {
                // No suspender context — still switch the profile, just without
                // hard-suspend side-effects. Fall back to direct state mutation.
                log::warn!(
                    "set_profile: SuspenderContext unavailable — switching to \"{profile}\" without suspend side-effects"
                );
                let mut st2 = shared.lock();
                let name = profile.clone();
                st2.active_profile = new_profile;
                st2.config.active_profile = name;
                let cfg = st2.config.clone();
                drop(st2);
                if let Err(e) = cfg.save() {
                    log::warn!("set_profile: failed to save config: {e}");
                }
                return Ok(());
            }
        }
    };

    // SAFETY: SuspenderContext lives in AppState kept alive by the Arc.
    let ctx = unsafe { &*ctx_ptr };

    // FIX: activate_profile now returns Err when admin required for Gaming Mode
    profiles::activate_profile(new_profile, shared, ctx)?;

    Ok(())
}

/// Return the name of the currently active profile.
#[tauri::command]
pub async fn get_current_profile(state: State<'_, SharedState>) -> Result<String, String> {
    let st = state.lock();
    let name = profiles::profile_to_name(&st.active_profile).to_string();
    Ok(name)
}
