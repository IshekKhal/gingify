//! Soft working-set trimmer — calls EmptyWorkingSet() on idle processes to
//! return their pages to the standby pool without killing or suspending them.

#![allow(dead_code)]

use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::Result;

use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA,
};
use windows::Win32::Foundation::CloseHandle;

use crate::state::app_state::{ProcessTrimRecord, SharedState, TrimEvent, TrimResult, TrimTrigger};
use crate::core::profiler;

// ---------------------------------------------------------------------------
// TrimError
// ---------------------------------------------------------------------------

/// Errors that can occur during a single-process soft trim.
#[derive(Debug, thiserror::Error)]
pub enum TrimError {
    #[error("Access denied for PID {0}")]
    AccessDenied(u32),
    #[error("Trim failed for PID {0}: {1}")]
    TrimFailed(u32, u32),
    #[error("Process not found: PID {0}")]
    ProcessGone(u32),
}

// ---------------------------------------------------------------------------
// Single-process soft trim
// ---------------------------------------------------------------------------

/// Open a process, empty its working set, and return the bytes freed.
///
/// Returns `Err(TrimError::AccessDenied)` if the handle cannot be obtained,
/// or `Err(TrimError::TrimFailed)` if `EmptyWorkingSet` fails.
pub fn soft_trim(pid: u32) -> Result<u64, TrimError> {
    // --- 1. Open process handle -----------------------------------------------
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
            false,
            pid,
        )
        .map_err(|_| TrimError::AccessDenied(pid))?
    };

    // --- 2. Read working set BEFORE -------------------------------------------
    let working_set_before: usize = {
        let mut mc = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let ok = unsafe { GetProcessMemoryInfo(handle, &mut mc, mc.cb) };
        if ok.is_err() {
            unsafe { let _ = CloseHandle(handle); }
            return Err(TrimError::AccessDenied(pid));
        }
        mc.WorkingSetSize
    };

    // --- 3. Empty the working set ---------------------------------------------
    let trim_ok = unsafe { EmptyWorkingSet(handle) };
    if trim_ok.is_err() {
        let last_error = unsafe { windows::Win32::Foundation::GetLastError().0 };
        unsafe { let _ = CloseHandle(handle); }
        return Err(TrimError::TrimFailed(pid, last_error));
    }

    // --- 4. Read working set AFTER --------------------------------------------
    let working_set_after: usize = {
        let mut mc = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let ok = unsafe { GetProcessMemoryInfo(handle, &mut mc, mc.cb) };
        if ok.is_err() {
            // Non-fatal — we already trimmed; just report 0 as after
            unsafe { let _ = CloseHandle(handle); }
            return Ok(working_set_before as u64);
        }
        mc.WorkingSetSize
    };

    unsafe { let _ = CloseHandle(handle); }

    // --- 5. Return bytes freed ------------------------------------------------
    Ok((working_set_before as u64).saturating_sub(working_set_after as u64))
}

// ---------------------------------------------------------------------------
// Batch soft trim
// ---------------------------------------------------------------------------

/// Trim all processes idle for at least `min_idle_secs` seconds.
///
/// Skips processes in `AppState.config.excluded_processes`.
/// Appends a `TrimEvent` to `AppState.trim_history` (capped at 50) and updates
/// `AppState.last_trim_result`.
pub fn soft_trim_all(
    state: &SharedState,
    min_idle_secs: u64,
    trigger: TrimTrigger,
) -> TrimResult {
    // --- 1. Collect candidates (no lock held during API calls) ----------------
    let (candidates, excluded_names) = {
        let st = state.lock();
        let candidates = profiler::get_idle_processes(&st, min_idle_secs);
        let mut excluded: std::collections::HashSet<String> = st
            .config
            .excluded_processes
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        // FIX: also respect session_excluded (set by Focus Mode to protect foreground app)
        if let Some(ref se) = st.session_excluded {
            excluded.insert(se.to_lowercase());
        }
        (candidates, excluded)
    };

    // --- 2. Trim each candidate -----------------------------------------------
    let mut total_freed: u64 = 0;
    let mut processes_trimmed: u32 = 0;
    // FIX: collect per-process data for unique-PID stats and expandable history rows
    let mut unique_pids: Vec<u32> = Vec::new();
    let mut per_process: Vec<ProcessTrimRecord> = Vec::new();

    for entry in &candidates {
        // Skip user-excluded processes (double-checked here against config snapshot)
        if excluded_names.contains(&entry.name.to_lowercase()) {
            continue;
        }

        match soft_trim(entry.pid) {
            Ok(freed) => {
                total_freed += freed;
                processes_trimmed += 1;
                unique_pids.push(entry.pid);
                if freed > 0 {
                    per_process.push(ProcessTrimRecord {
                        name: entry.name.clone(),
                        freed_bytes: freed,
                    });
                }
                log::debug!(
                    "trimmer: soft_trim PID {} ({}) freed {} bytes",
                    entry.pid,
                    entry.name,
                    freed
                );
            }
            Err(TrimError::AccessDenied(pid)) => {
                log::debug!("trimmer: access denied for PID {pid} — skipping");
            }
            Err(TrimError::TrimFailed(pid, code)) => {
                log::warn!("trimmer: EmptyWorkingSet failed for PID {pid} (error {code}) — skipping");
            }
            Err(TrimError::ProcessGone(pid)) => {
                log::debug!("trimmer: PID {pid} gone before trim — skipping");
            }
        }
    }

    // --- 3. Build TrimResult -------------------------------------------------
    let trim_result = TrimResult {
        freed_bytes: total_freed,
        processes_trimmed,
        timestamp: SystemTime::now(),
        unique_pids,
        per_process,
    };

    // --- 4. Update shared state ----------------------------------------------
    {
        let mut st = state.lock();
        // FIX: skip logging 0-byte/0-process events (stops "Gaming Mode — Freed 0.0 GB" spam)
        if trim_result.freed_bytes > 0 || trim_result.processes_trimmed > 0 {
            let event = TrimEvent {
                result: trim_result.clone(),
                trigger,
            };
            st.trim_history.push(event);
        }

        // Cap history at 50 entries
        let limit = st.config.trim_history_limit.max(1);
        if st.trim_history.len() > limit {
            let excess = st.trim_history.len() - limit;
            st.trim_history.drain(..excess);
        }

        st.last_trim_result = Some(trim_result.clone());
    }

    // --- 5. Persist history to disk ------------------------------------------
    let history = {
        let st = state.lock();
        st.trim_history.clone()
    };
    if let Err(e) = save_history(&history) {
        log::warn!("trimmer: failed to save history: {e}");
    }

    log::info!(
        "trimmer: soft_trim_all complete — freed {} bytes across {} processes (trigger={:?})",
        total_freed,
        processes_trimmed,
        trigger
    );

    trim_result
}

// ---------------------------------------------------------------------------
// History persistence
// ---------------------------------------------------------------------------

/// Returns the path to `%APPDATA%\Gingify\history.json`.
fn history_path() -> Result<PathBuf> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| anyhow::anyhow!("APPDATA environment variable is not set"))?;
    Ok(PathBuf::from(appdata).join("Gingify").join("history.json"))
}

/// Serialize `events` to JSON and write to `%APPDATA%\Gingify\history.json`.
/// Creates the directory if it does not exist.
pub fn save_history(events: &[TrimEvent]) -> Result<()> {
    let path = history_path()?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let json = serde_json::to_string_pretty(events)?;
    std::fs::write(&path, json)?;

    log::debug!("trimmer: history saved to {:?} ({} entries)", path, events.len());
    Ok(())
}

/// Load trim history from `%APPDATA%\Gingify\history.json`.
///
/// Returns an empty `Vec` on any error (missing file = fresh install, corrupt = reset).
pub fn load_history() -> Vec<TrimEvent> {
    let path = match history_path() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("trimmer: cannot resolve history path: {e}");
            return Vec::new();
        }
    };

    if !path.exists() {
        log::info!("trimmer: history file not found at {:?} — starting fresh", path);
        return Vec::new();
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("trimmer: failed to read history file: {e}");
            return Vec::new();
        }
    };

    match serde_json::from_str::<Vec<TrimEvent>>(&raw) {
        Ok(events) => {
            log::info!("trimmer: loaded {} history entries from {:?}", events.len(), path);
            events
        }
        Err(e) => {
            log::warn!("trimmer: history file corrupt ({e}) — resetting");
            Vec::new()
        }
    }
}
