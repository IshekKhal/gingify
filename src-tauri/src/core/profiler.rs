//! Per-process idle-time and RAM tracking — consumes monitor data each cycle
//! to maintain `last_active_at` timestamps and expose idle process lists.

#![allow(dead_code)]

use std::time::Instant;

use crate::state::app_state::{AppState, ProcessEntry, SharedState};

// ---------------------------------------------------------------------------
// Idle time update — called at end of each monitor poll cycle
// ---------------------------------------------------------------------------

/// Update `idle_seconds` and `last_active_at` for every entry in `process_map`.
///
/// Rule:
/// - If `cpu_usage_pct > 0.5` → reset `last_active_at` to now, `idle_seconds = 0`
/// - Otherwise             → compute `idle_seconds` from `last_active_at.elapsed()`
pub fn update_idle_times(state: &SharedState) {
    let mut st = state.lock();
    let now = Instant::now();

    for entry in st.process_map.values_mut() {
        if entry.cpu_usage_pct > 0.5 {
            // Process is active — reset idle clock
            entry.last_active_at = Some(now);
            entry.idle_seconds = 0;
        } else {
            // Process is idle — accumulate elapsed seconds
            let elapsed = entry
                .last_active_at
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            entry.idle_seconds = elapsed;
        }
    }
}

// ---------------------------------------------------------------------------
// Idle process query — callable from trimmer and profile logic
// ---------------------------------------------------------------------------

/// Return a cloned list of processes that are:
/// - idle for at least `min_idle_secs` seconds
/// - **not** protected (`is_protected == false`)
/// - **not** user-excluded (`is_excluded == false`)
/// - **not** currently hard-suspended (`is_suspended == false`)
///
/// Sorted by `ram_mb` descending (highest RAM consumers first).
pub fn get_idle_processes(state: &AppState, min_idle_secs: u64) -> Vec<ProcessEntry> {
    let mut result: Vec<ProcessEntry> = state
        .process_map
        .values()
        .filter(|e| {
            e.idle_seconds >= min_idle_secs
                && !e.is_protected
                && !e.is_excluded
                && !e.is_suspended
        })
        .cloned()
        .collect();

    result.sort_by(|a, b| {
        b.ram_mb
            .partial_cmp(&a.ram_mb)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    result
}
