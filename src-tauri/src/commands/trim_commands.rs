//! IPC commands for trim operations — trim_all, trim_process, suspend_process,
//! resume_process, trim_bloat, suspend_bloat, suspend_all_bloat callable from
//! the frontend via invoke().

#![allow(dead_code)]

use std::time::SystemTime;

use tauri::{State, Emitter};

use crate::state::app_state::{ProcessTrimRecord, SharedState, TrimEvent, TrimResult, TrimTrigger};
use crate::core::trimmer;
use crate::core::suspender;
use crate::core::bloat_scan;

// ---------------------------------------------------------------------------
// trim_all — soft-trim all idle processes
// ---------------------------------------------------------------------------

/// Soft-trim all idle processes according to the current profile threshold.
///
/// `trigger_str` maps to `TrimTrigger`; unrecognised values default to `Manual`.
#[tauri::command]
pub async fn trim_all(
    trigger_str: Option<String>,
    state: State<'_, SharedState>,
    app_handle: tauri::AppHandle,
) -> Result<TrimResult, String> {
    // Clone state Arc so we can move it safely without holding the guard
    let shared = state.inner().clone();

    // Read idle threshold from config (hold lock briefly)
    let idle_threshold = {
        let st = shared.lock();
        st.config.idle_threshold_secs
    };

    // Determine trigger type and effective idle threshold.
    // "ManualIdle" trims only idle processes (respects idle_threshold_secs)
    // but records as a Manual event.  Plain "Manual" uses threshold=0 so
    // it trims everything not protected/excluded (the "Free RAM Now" path).
    let (trigger, effective_threshold) = match trigger_str.as_deref() {
        Some("auto") | Some("Auto")             => (TrimTrigger::Auto,       idle_threshold),
        Some("gaming") | Some("GamingMode")     => (TrimTrigger::GamingMode, idle_threshold),
        Some("ManualIdle") | Some("manual_idle") => (TrimTrigger::Manual,    idle_threshold),
        _                                        => (TrimTrigger::Manual,     0),
    };

    let result = trimmer::soft_trim_all(&shared, effective_threshold, trigger);

    // Emit ram-update to trigger immediate UI refresh
    let stats = {
        let st = shared.lock();
        st.ram_stats.clone()
    };
    let _ = app_handle.emit("gingify://ram-update", &stats);

    Ok(result)
}

// ---------------------------------------------------------------------------
// trim_process — soft-trim a single process by PID
// ---------------------------------------------------------------------------

/// Soft-trim a single process by PID.
///
/// Wraps the freed bytes in a single-process `TrimResult` and appends it to history.
#[tauri::command]
pub async fn trim_process(
    pid: u32,
    state: State<'_, SharedState>,
    app_handle: tauri::AppHandle,
) -> Result<TrimResult, String> {
    let freed = trimmer::soft_trim(pid).map_err(|e| e.to_string())?;

    // Update the process_map to reflect freed RAM immediately
    {
        let mut st = state.lock();
        if let Some(entry) = st.process_map.get_mut(&pid) {
            let freed_mb = freed as f32 / (1024.0 * 1024.0);
            entry.ram_mb = (entry.ram_mb - freed_mb).max(0.0);
        }
    }

    // Return the result to the caller for inline UI feedback.
    // Single-process trims are intentionally NOT added to the history log
    // (only batch trim_all / trim_bloat events appear there) to prevent
    // history spam when the user clicks individual Trim buttons.
    let result = TrimResult {
        freed_bytes: freed,
        processes_trimmed: 1,
        timestamp: SystemTime::now(),
        unique_pids: vec![pid],
        per_process: vec![],
    };

    // Emit ram-update to trigger immediate UI refresh
    let stats = {
        let st = state.lock();
        st.ram_stats.clone()
    };
    let _ = app_handle.emit("gingify://ram-update", &stats);

    Ok(result)
}

// ---------------------------------------------------------------------------
// suspend_process / resume_process
// ---------------------------------------------------------------------------

/// Hard-suspend a process (NtSuspendProcess). Requires Gaming Mode + user consent.
#[tauri::command]
pub async fn suspend_process(
    pid: u32,
    state: State<'_, SharedState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let shared = state.inner().clone();

    // Extract the SuspenderContext from AppState (clone the Arc, not the ctx)
    // We must not hold the lock while calling the Windows API.
    let ctx_ptr: *const suspender::SuspenderContext = {
        let st = shared.lock();
        match st.suspender_ctx.as_ref() {
            Some(ctx) => ctx as *const _,
            None => return Err("SuspenderContext not available — ntdll resolution failed".to_string()),
        }
    };

    // SAFETY: SuspenderContext lives in AppState which is kept alive by the Arc
    // for the process lifetime. We don't hold the lock during the call.
    let ctx = unsafe { &*ctx_ptr };

    suspender::hard_suspend(ctx, pid, shared.clone())
        .map_err(|e| e.to_string())?;

    // Immediately update process_map so UI sees Suspended state without waiting for next poll
    {
        let mut st = shared.lock();
        if let Some(entry) = st.process_map.get_mut(&pid) {
            entry.is_suspended = true;
        }
    }

    // Emit ram-update to trigger immediate UI refresh
    let stats = {
        let st = shared.lock();
        st.ram_stats.clone()
    };
    let _ = app_handle.emit("gingify://ram-update", &stats);

    Ok(())
}

/// Resume a hard-suspended process (NtResumeProcess).
#[tauri::command]
pub async fn resume_process(
    pid: u32,
    state: State<'_, SharedState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let shared = state.inner().clone();

    let ctx_ptr: *const suspender::SuspenderContext = {
        let st = shared.lock();
        match st.suspender_ctx.as_ref() {
            Some(ctx) => ctx as *const _,
            None => return Err("SuspenderContext not available — ntdll resolution failed".to_string()),
        }
    };

    // SAFETY: same as suspend_process — SuspenderContext is held in AppState Arc.
    let ctx = unsafe { &*ctx_ptr };

    suspender::hard_resume(ctx, pid, shared.clone())
        .map_err(|e| e.to_string())?;

    // Immediately update process_map so UI sees Active state without waiting for next poll
    {
        let mut st = shared.lock();
        if let Some(entry) = st.process_map.get_mut(&pid) {
            entry.is_suspended = false;
        }
    }

    // Emit ram-update to trigger immediate UI refresh
    let stats = {
        let st = shared.lock();
        st.ram_stats.clone()
    };
    let _ = app_handle.emit("gingify://ram-update", &stats);

    Ok(())
}

// ---------------------------------------------------------------------------
// trim_bloat — soft-trim all processes in a named bloat category
// ---------------------------------------------------------------------------

/// Soft-trim all currently-running processes matching a named bloat category
/// (e.g. `"Copilot"`, `"Xbox Game Bar"`).
///
/// Returns an aggregated `TrimResult` across all matching PIDs.
#[tauri::command]
pub async fn trim_bloat(
    name: String,
    state: State<'_, SharedState>,
) -> Result<TrimResult, String> {
    let shared = state.inner().clone();

    // Collect PIDs for this bloat category (brief lock, then release)
    let pids: Vec<u32> = {
        let st = shared.lock();
        bloat_scan::get_bloat_pids(&name, &st)
    };

    if pids.is_empty() {
        return Ok(TrimResult {
            freed_bytes: 0,
            processes_trimmed: 0,
            timestamp: SystemTime::now(),
            unique_pids: vec![],
            per_process: vec![],
        });
    }

    let mut total_freed: u64 = 0;
    let mut processes_trimmed: u32 = 0;
    let mut unique_pids: Vec<u32> = Vec::new();
    let mut per_process_bloat: Vec<ProcessTrimRecord> = Vec::new();

    for pid in &pids {
        match trimmer::soft_trim(*pid) {
            Ok(freed) => {
                total_freed += freed;
                processes_trimmed += 1;
                unique_pids.push(*pid);
                if freed > 0 {
                    per_process_bloat.push(ProcessTrimRecord { name: name.clone(), freed_bytes: freed });
                }
                log::debug!("trim_bloat: PID {pid} freed {freed} bytes");
            }
            Err(e) => {
                log::warn!("trim_bloat: PID {pid} failed: {e}");
            }
        }
    }

    let result = TrimResult {
        freed_bytes: total_freed,
        processes_trimmed,
        timestamp: SystemTime::now(),
        unique_pids,
        per_process: per_process_bloat,
    };

    // FIX: only append to history when something was actually freed
    if result.freed_bytes > 0 || result.processes_trimmed > 0 {
        // Append to history
        {
            let mut st = shared.lock();
            let event = TrimEvent {
                result: result.clone(),
                trigger: TrimTrigger::Manual,
            };
            st.trim_history.push(event);
            let limit = st.config.trim_history_limit.max(1);
            if st.trim_history.len() > limit {
                let excess = st.trim_history.len() - limit;
                st.trim_history.drain(..excess);
            }
            st.last_trim_result = Some(result.clone());
        }

        // Persist history
        let history = {
            let st = shared.lock();
            st.trim_history.clone()
        };
        if let Err(e) = trimmer::save_history(&history) {
            log::warn!("trim_bloat: failed to save history: {e}");
        }
    }

    log::info!(
        "trim_bloat: \"{name}\" — trimmed {processes_trimmed} processes, freed {total_freed} bytes"
    );

    Ok(result)
}

// ---------------------------------------------------------------------------
// suspend_bloat — hard-suspend all processes in a named bloat category
// ---------------------------------------------------------------------------

/// Hard-suspend all currently-running processes matching a named bloat category.
///
/// Requires `config.hard_suspend_enabled == true`.
#[tauri::command]
pub async fn suspend_bloat(
    name: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let shared = state.inner().clone();

    // Get SuspenderContext pointer (valid for lifetime of AppState Arc)
    let ctx_ptr: *const suspender::SuspenderContext = {
        let st = shared.lock();
        match st.suspender_ctx.as_ref() {
            Some(ctx) => ctx as *const _,
            None => return Err("SuspenderContext not available — ntdll resolution failed".to_string()),
        }
    };

    // SAFETY: SuspenderContext lives in AppState, kept alive by the Arc.
    let ctx = unsafe { &*ctx_ptr };

    // Collect PIDs for this bloat category
    let pids: Vec<u32> = {
        let st = shared.lock();
        bloat_scan::get_bloat_pids(&name, &st)
    };

    if pids.is_empty() {
        log::info!("suspend_bloat: \"{name}\" — no matching processes running");
        return Ok(());
    }

    let mut errors: Vec<String> = Vec::new();
    for pid in &pids {
        if let Err(e) = suspender::hard_suspend(ctx, *pid, shared.clone()) {
            errors.push(format!("PID {pid}: {e}"));
        }
    }

    if errors.is_empty() {
        log::info!("suspend_bloat: \"{name}\" — suspended {} PIDs", pids.len());
        Ok(())
    } else {
        Err(format!(
            "suspend_bloat \"{name}\": {} of {} PIDs failed — {}",
            errors.len(),
            pids.len(),
            errors.join("; ")
        ))
    }
}

// ---------------------------------------------------------------------------
// suspend_all_bloat — hard-suspend every running bloat category
// ---------------------------------------------------------------------------

/// Hard-suspend all currently-running processes across **all** known bloat categories.
///
/// Iterates every `BloatDefinition` in the registry and calls `suspend_bloat`
/// logic for each. Categories with no running processes are silently skipped.
/// Requires `config.hard_suspend_enabled == true`.
#[tauri::command]
pub async fn suspend_all_bloat(
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let shared = state.inner().clone();

    // Get SuspenderContext pointer (valid for lifetime of AppState Arc)
    let ctx_ptr: *const suspender::SuspenderContext = {
        let st = shared.lock();
        match st.suspender_ctx.as_ref() {
            Some(ctx) => ctx as *const _,
            None => return Err("SuspenderContext not available — ntdll resolution failed".to_string()),
        }
    };

    // SAFETY: SuspenderContext lives in AppState Arc (process lifetime).
    let ctx = unsafe { &*ctx_ptr };

    // Suspend every running bloat category
    let names: Vec<&'static str> = bloat_scan::known_bloat_names().collect();
    let mut total_errors: Vec<String> = Vec::new();

    for name in names {
        let pids: Vec<u32> = {
            let st = shared.lock();
            bloat_scan::get_bloat_pids(name, &st)
        };

        for pid in pids {
            if let Err(e) = suspender::hard_suspend(ctx, pid, shared.clone()) {
                total_errors.push(format!("{name}/PID {pid}: {e}"));
            }
        }
    }

    if total_errors.is_empty() {
        log::info!("suspend_all_bloat: complete");
        Ok(())
    } else {
        log::warn!(
            "suspend_all_bloat: {} error(s): {}",
            total_errors.len(),
            total_errors.join("; ")
        );
        // Return Ok — partial failures are expected (some bloat may be protected)
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// resume_bloat — hard-resume all processes in a named bloat category
// ---------------------------------------------------------------------------

/// Hard-resume all currently-suspended processes matching a named bloat category.
#[tauri::command]
pub async fn resume_bloat(
    name: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let shared = state.inner().clone();

    let ctx_ptr: *const suspender::SuspenderContext = {
        let st = shared.lock();
        match st.suspender_ctx.as_ref() {
            Some(ctx) => ctx as *const _,
            None => return Err("SuspenderContext not available — ntdll resolution failed".to_string()),
        }
    };

    // SAFETY: SuspenderContext lives in AppState Arc (process lifetime).
    let ctx = unsafe { &*ctx_ptr };

    let pids: Vec<u32> = {
        let st = shared.lock();
        bloat_scan::get_bloat_pids(&name, &st)
    };

    for pid in &pids {
        if let Err(e) = suspender::hard_resume(ctx, *pid, shared.clone()) {
            log::warn!("resume_bloat: PID {pid} failed: {e}");
        }
    }

    log::info!("resume_bloat: \"{name}\" — resumed {} PIDs", pids.len());
    Ok(())
}
