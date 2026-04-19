//! Windows AI / bloat service scanner — detects Copilot, Recall, Xbox GameBar,
//! Widgets, and other known memory-heavy system services in the running process list.

#![allow(dead_code)]

use crate::state::app_state::{AppState, BloatEntry};

// ---------------------------------------------------------------------------
// Known bloat definitions (hardcoded, not user-editable in v1)
// ---------------------------------------------------------------------------

/// Static descriptor for a known Windows bloat/AI service category.
pub struct BloatDefinition {
    /// Human-readable display name shown in the UI.
    pub display_name: &'static str,
    /// Executable names that belong to this category (case-insensitive match).
    pub exe_names: &'static [&'static str],
    /// Tooltip description shown in the UI.
    pub description: &'static str,
}

const KNOWN_BLOAT: &[BloatDefinition] = &[
    BloatDefinition {
        display_name: "Copilot",
        exe_names: &["Copilot.exe", "Microsoft.Windows.Copilot.exe"],
        description: "Windows AI assistant. Runs in background consuming 400-800MB.",
    },
    BloatDefinition {
        display_name: "Recall (AI Screenshot)",
        exe_names: &["AIXHost.exe", "ScreenshotService.exe"],
        description: "Continuously screenshots your screen for AI indexing.",
    },
    BloatDefinition {
        display_name: "Xbox Game Bar",
        exe_names: &["GameBar.exe", "GameBarPresenceWriter.exe", "GameBarFTServer.exe"],
        description: "Xbox overlay and capture tools. Often unused.",
    },
    BloatDefinition {
        display_name: "Widgets",
        exe_names: &["Widgets.exe", "WidgetService.exe"],
        description: "Windows news/weather widget panel.",
    },
    BloatDefinition {
        display_name: "Windows AI Services",
        exe_names: &["WinMLDashboard.exe", "WindowsAI.exe"],
        description: "Machine learning framework services running in background.",
    },
];

// ---------------------------------------------------------------------------
// scan_bloat
// ---------------------------------------------------------------------------

/// Scan the current process map against the known bloat registry.
///
/// For each `BloatDefinition`, checks `AppState.process_map` for any matching
/// exe name (case-insensitive). Returns a full list of `BloatEntry` values
/// including entries for services that are **not** currently running (so the UI
/// can show "Not running" for those). The caller should store the result in
/// `AppState.bloat_list`.
pub fn scan_bloat(state: &AppState) -> Vec<BloatEntry> {
    let mut results = Vec::with_capacity(KNOWN_BLOAT.len());

    for def in KNOWN_BLOAT {
        let mut total_ram_mb: f32 = 0.0;
        let mut matching_pids: Vec<u32> = Vec::new();

        // Collect all matching PIDs and sum their RAM usage
        for (pid, entry) in &state.process_map {
            let entry_name_lower = entry.name.to_lowercase();
            let is_match = def
                .exe_names
                .iter()
                .any(|exe| exe.to_lowercase() == entry_name_lower);

            if is_match {
                total_ram_mb += entry.ram_mb;
                matching_pids.push(*pid);
            }
        }

        // is_suspended == true only if ALL matching PIDs are in suspended_set
        // (and there is at least one matching PID)
        let is_suspended = !matching_pids.is_empty()
            && matching_pids
                .iter()
                .all(|pid| state.suspended_set.contains(pid));

        results.push(BloatEntry {
            name: def.display_name.to_string(),
            exe_names: def.exe_names.iter().map(|s| s.to_string()).collect(),
            ram_mb: total_ram_mb,
            is_suspended,
        });
    }

    results
}

// ---------------------------------------------------------------------------
// get_bloat_pids
// ---------------------------------------------------------------------------

/// Return all PIDs currently running that match a named bloat definition.
///
/// `definition_name` is matched case-insensitively against
/// `BloatDefinition.display_name`. Returns an empty `Vec` if the name is
/// unknown or no matching processes are running.
///
/// Used by `trim_bloat` and `suspend_bloat` IPC commands.
pub fn get_bloat_pids(definition_name: &str, state: &AppState) -> Vec<u32> {
    let name_lower = definition_name.to_lowercase();

    let def = match KNOWN_BLOAT
        .iter()
        .find(|d| d.display_name.to_lowercase() == name_lower)
    {
        Some(d) => d,
        None => {
            log::warn!("bloat_scan: unknown bloat definition name \"{definition_name}\"");
            return Vec::new();
        }
    };

    let mut pids = Vec::new();
    for (pid, entry) in &state.process_map {
        let entry_name_lower = entry.name.to_lowercase();
        let is_match = def
            .exe_names
            .iter()
            .any(|exe| exe.to_lowercase() == entry_name_lower);
        if is_match {
            pids.push(*pid);
        }
    }
    pids
}

// ---------------------------------------------------------------------------
// known_bloat_names — convenience for suspend_all_bloat
// ---------------------------------------------------------------------------

/// Returns the display names of all known bloat definitions.
/// Used by `suspend_all_bloat` to iterate over every category.
pub fn known_bloat_names() -> impl Iterator<Item = &'static str> {
    KNOWN_BLOAT.iter().map(|d| d.display_name)
}
