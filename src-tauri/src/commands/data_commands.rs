//! IPC commands for data fetch — get_process_list, get_ram_stats, get_bloat_list,
//! get_trim_history callable from the frontend via invoke().

#![allow(dead_code)]

use tauri::State;

use crate::state::app_state::{BloatEntry, ProcessEntry, RamStats, SharedState, TrimEvent};

/// Return the top 20 processes by RAM usage (sorted descending).
///
/// `include_system`: when `false` (default), protected/system processes are excluded.
/// When `true`, they are included at the bottom of the list (the UI renders them greyed out).
#[tauri::command]
pub async fn get_process_list(
    include_system: Option<bool>,
    state: State<'_, SharedState>,
) -> Result<Vec<ProcessEntry>, String> {
    let show_system = include_system.unwrap_or(false);
    let st = state.lock();
    let mut list: Vec<ProcessEntry> = st
        .process_map
        .values()
        .filter(|p| show_system || !p.is_protected)
        .cloned()
        .collect();
    list.sort_by(|a, b| b.ram_mb.partial_cmp(&a.ram_mb).unwrap_or(std::cmp::Ordering::Equal));
    list.truncate(150);
    Ok(list)
}

/// Return current system RAM statistics.
#[tauri::command]
pub async fn get_ram_stats(state: State<'_, SharedState>) -> Result<RamStats, String> {
    let st = state.lock();
    Ok(st.ram_stats.clone())
}

/// Return the list of detected bloat services and their current RAM usage.
#[tauri::command]
pub async fn get_bloat_list(state: State<'_, SharedState>) -> Result<Vec<BloatEntry>, String> {
    let st = state.lock();
    Ok(st.bloat_list.clone())
}

/// Return the trim event history (up to trim_history_limit entries).
#[tauri::command]
pub async fn get_trim_history(state: State<'_, SharedState>) -> Result<Vec<TrimEvent>, String> {
    let st = state.lock();
    Ok(st.trim_history.clone())
}
