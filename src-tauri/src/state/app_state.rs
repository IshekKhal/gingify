//! Global shared application state — all live data accessed by both the
//! background monitor thread and Tauri IPC command handlers.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::core::suspender::SuspenderContext;
use crate::state::config::UserConfig;

// ---------------------------------------------------------------------------
// Type alias — AppState wrapped for thread-safe sharing
// ---------------------------------------------------------------------------

/// Thread-safe shared state passed between the monitor thread and IPC handlers.
pub type SharedState = Arc<Mutex<AppState>>;

// ---------------------------------------------------------------------------
// Top-level application state
// ---------------------------------------------------------------------------

/// Central application state holding all live runtime data.
#[derive(Debug)]
pub struct AppState {
    /// Live map of PID → per-process data, updated every 5 s by monitor.rs.
    pub process_map: HashMap<u32, ProcessEntry>,
    /// Set of PIDs currently hard-suspended via NtSuspendProcess.
    pub suspended_set: HashSet<u32>,
    /// The active operational profile (Work / Gaming / Focus / Custom).
    pub active_profile: Profile,
    /// Current system RAM statistics.
    pub ram_stats: RamStats,
    /// Known Windows AI / bloat services detected this cycle.
    pub bloat_list: Vec<BloatEntry>,
    /// Result of the most recent trim operation (None until first trim runs).
    pub last_trim_result: Option<TrimResult>,
    /// User configuration loaded from disk.
    pub config: UserConfig,
    /// Ring buffer of trim events (capped at config.trim_history_limit).
    pub trim_history: Vec<TrimEvent>,
    /// Instant of the last auto-trim; used to enforce the 60-second cooldown.
    pub last_auto_trim_at: Option<Instant>,
    /// Resolved NtSuspendProcess / NtResumeProcess function pointers.
    /// `None` only if ntdll resolution fails (should never happen on any
    /// supported Windows version).
    pub suspender_ctx: Option<SuspenderContext>,
    // FIX: session-only exclusion for Focus Mode (not persisted to disk)
    /// Process name excluded for the duration of the current Focus Mode session.
    /// Cleared when leaving Focus Mode. NOT saved to config.json.
    pub session_excluded: Option<String>,
}

impl AppState {
    /// Create a default-initialised `AppState`.
    pub fn new(config: UserConfig) -> Self {
        let active_profile = Profile::from_name(&config.active_profile);
        Self {
            process_map: HashMap::new(),
            suspended_set: HashSet::new(),
            active_profile,
            ram_stats: RamStats::default(),
            bloat_list: Vec::new(),
            last_trim_result: None,
            trim_history: Vec::new(),
            last_auto_trim_at: None,
            config,
            suspender_ctx: None, // populated in lib.rs after load_suspender()
            session_excluded: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Process entry
// ---------------------------------------------------------------------------

/// Data for a single running process, tracked by the monitor and profiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEntry {
    /// Windows Process ID.
    pub pid: u32,
    /// Executable name (e.g. "chrome.exe").
    pub name: String,
    /// Current working set size in megabytes.
    pub ram_mb: f32,
    /// CPU usage as a percentage (0.0–100.0) over the last poll interval.
    pub cpu_usage_pct: f32,
    /// Seconds since this process last had measurable CPU activity (>0.5%).
    pub idle_seconds: u64,
    /// Whether this PID is currently in `AppState.suspended_set`.
    pub is_suspended: bool,
    /// System / protected process — Gingify must never touch these.
    pub is_protected: bool,
    /// User has excluded this process from all auto-gingify actions.
    pub is_excluded: bool,
    /// Whether this process owns at least one visible window (Apps vs Background detection).
    pub has_window: bool,
    /// Base64-encoded PNG data URL of the process icon (extracted from the exe on first poll).
    /// Omitted from JSON when not available to keep payload small.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_data_url: Option<String>,
    /// Timestamp of the last observed CPU activity — skipped during serialisation.
    #[serde(skip)]
    pub last_active_at: Option<Instant>,
    /// Previous kernel-mode FILETIME for CPU delta calculation — skipped during serialisation.
    #[serde(skip)]
    pub prev_kernel_time: u64,
    /// Previous user-mode FILETIME for CPU delta calculation — skipped during serialisation.
    #[serde(skip)]
    pub prev_user_time: u64,
}

// ---------------------------------------------------------------------------
// RAM statistics
// ---------------------------------------------------------------------------

/// Snapshot of system-wide RAM metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RamStats {
    /// Total physical RAM in the system (MB).
    pub total_mb: u64,
    /// Currently used RAM (MB) = total - available.
    pub used_mb: u64,
    /// Available (free + standby) RAM as reported by GlobalMemoryStatusEx (MB).
    pub available_mb: u64,
    /// Memory load as a percentage (0.0–100.0), from GlobalMemoryStatusEx.
    pub pressure_pct: f32,
    /// Categorical pressure level derived from `pressure_pct`.
    pub pressure_level: PressureLevel,
}

impl Default for RamStats {
    fn default() -> Self {
        Self {
            total_mb: 0,
            used_mb: 0,
            available_mb: 0,
            pressure_pct: 0.0,
            pressure_level: PressureLevel::Low,
        }
    }
}

// ---------------------------------------------------------------------------
// Pressure level
// ---------------------------------------------------------------------------

/// Categorical RAM pressure derived from the raw percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PressureLevel {
    /// < 70 % used — system is comfortable.
    Low,
    /// 70–84 % used — approaching threshold, amber tray icon.
    Medium,
    /// 85–94 % used — auto-trim may fire, red tray icon.
    High,
    /// ≥ 95 % used — critical, immediate action warranted.
    Critical,
}

impl PressureLevel {
    /// Convert a raw pressure percentage (0.0–100.0) to the appropriate variant.
    pub fn from_pct(pct: f32) -> Self {
        match pct {
            p if p >= 95.0 => PressureLevel::Critical,
            p if p >= 85.0 => PressureLevel::High,
            p if p >= 70.0 => PressureLevel::Medium,
            _ => PressureLevel::Low,
        }
    }
}

// ---------------------------------------------------------------------------
// Trim types
// ---------------------------------------------------------------------------

/// Per-process breakdown entry inside a TrimResult.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessTrimRecord {
    /// Executable name (e.g. "chrome.exe").
    pub name: String,
    /// Bytes freed from this process's working set.
    pub freed_bytes: u64,
}

/// Summary result from a trim operation (soft or batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrimResult {
    /// Bytes reclaimed from the standby / available pool.
    pub freed_bytes: u64,
    /// Number of processes whose working sets were trimmed.
    pub processes_trimmed: u32,
    /// Wall-clock time when the trim completed.
    pub timestamp: SystemTime,
    // FIX: track unique PIDs trimmed (for accurate "apps snoozed today" stat)
    /// PIDs that were actually trimmed in this operation (deduplicated).
    pub unique_pids: Vec<u32>,
    // FIX: per-process breakdown for expandable history rows
    /// Per-process freed-bytes breakdown.
    pub per_process: Vec<ProcessTrimRecord>,
}

/// Cause of a trim event recorded in the history log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrimTrigger {
    /// Triggered automatically by the auto-gingify threshold.
    Auto,
    /// Triggered explicitly by the user ("Free RAM Now").
    Manual,
    /// Triggered when Gaming Mode was activated.
    GamingMode,
}

/// A single entry in the trim history ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrimEvent {
    /// The outcome of the trim.
    pub result: TrimResult,
    /// What caused this trim to run.
    pub trigger: TrimTrigger,
}

// ---------------------------------------------------------------------------
// Bloat entry
// ---------------------------------------------------------------------------

/// A detected Windows AI / bloat service with its current RAM cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BloatEntry {
    /// Human-readable display name (e.g. "Copilot").
    pub name: String,
    /// Executable names that belong to this bloat category.
    pub exe_names: Vec<String>,
    /// Current RAM usage across all matching processes (MB).
    pub ram_mb: f32,
    /// Whether all matching processes are currently hard-suspended.
    pub is_suspended: bool,
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// User-defined profile settings persisted in config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomProfile {
    /// Display name for the custom profile.
    pub name: String,
    /// Minimum idle time (seconds) before a process is eligible for trimming.
    pub idle_threshold_secs: u64,
    /// Whether hard suspend (NtSuspendProcess) is enabled.
    pub use_hard_suspend: bool,
    /// Exe names excluded from all auto-gingify actions in this profile.
    pub excluded_apps: Vec<String>,
}

impl Default for CustomProfile {
    fn default() -> Self {
        Self {
            name: "Custom".to_string(),
            idle_threshold_secs: 600,
            use_hard_suspend: false,
            excluded_apps: Vec::new(),
        }
    }
}

/// Operational mode that controls trim aggressiveness and targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Profile {
    /// 15-minute idle threshold, soft trim only, no bloat auto-suspend.
    Work,
    /// 5-minute idle threshold, soft + hard suspend, auto-suspend GameBar/Widgets.
    Gaming,
    /// 10-minute idle threshold, soft trim only, protects active app.
    Focus,
    /// User-defined profile with custom settings.
    Custom(CustomProfile),
}

impl Profile {
    /// Parse a profile name from the config string (case-insensitive, defaults to Work).
    /// For "custom", returns a default `CustomProfile` — callers that need the full
    /// custom config must load it separately from `UserConfig`.
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "gaming" => Profile::Gaming,
            "focus" => Profile::Focus,
            "custom" => Profile::Custom(CustomProfile::default()),
            _ => Profile::Work,
        }
    }

    /// Idle threshold in seconds for this profile.
    pub fn idle_threshold_secs(&self) -> u64 {
        match self {
            Profile::Work => 900,          // 15 min
            Profile::Gaming => 300,         // 5 min
            Profile::Focus => 600,          // 10 min
            Profile::Custom(cp) => cp.idle_threshold_secs,
        }
    }

    /// Whether hard suspend (NtSuspendProcess) is enabled for this profile.
    pub fn hard_suspend_enabled(&self) -> bool {
        match self {
            Profile::Gaming => true,
            Profile::Custom(cp) => cp.use_hard_suspend,
            _ => false,
        }
    }

    /// Whether bloat services (GameBar, Widgets) should be auto-suspended.
    pub fn auto_suspend_bloat(&self) -> bool {
        matches!(self, Profile::Gaming)
    }
}
