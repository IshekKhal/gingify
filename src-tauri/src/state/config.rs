//! User configuration — read from and written to `%APPDATA%\Gingify\config.json`.

#![allow(dead_code)]

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config struct
// ---------------------------------------------------------------------------

/// Persistent user preferences stored as JSON in AppData.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    /// Whether the auto-trim loop is active.
    pub auto_trim_enabled: bool,
    /// RAM pressure percentage that triggers auto-trim (0–100).
    pub auto_trim_threshold_pct: u8,
    /// Minimum idle time before a process is eligible for trimming (seconds).
    pub idle_threshold_secs: u64,
    /// Name of the currently active profile ("Work" / "Gaming" / "Focus" / "Custom").
    pub active_profile: String,
    /// Whether hard suspend (NtSuspendProcess) is allowed at all.
    pub hard_suspend_enabled: bool,
    /// Exe names the user has excluded from all auto-gingify actions.
    pub excluded_processes: Vec<String>,
    /// Whether Gingify registers itself in HKCU Run at startup.
    pub start_on_login: bool,
    /// Whether toast notifications are allowed.
    pub notifications_enabled: bool,
    /// UI theme: "system" / "dark" / "light".
    pub theme: String,
    /// Maximum number of trim events kept in the history ring buffer.
    pub trim_history_limit: usize,
    /// Whether the first-launch onboarding screen has been completed.
    /// Set to `true` after the user clicks [Get Started].
    #[serde(default)]
    pub first_launch_complete: bool,
    /// Saved idle threshold (seconds) from before entering Focus mode.
    /// Restored when the user leaves Focus. Not shown in the UI directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_focus_idle_threshold: Option<u64>,
    /// Saved hard_suspend_enabled value from before entering Focus mode.
    /// Restored when the user leaves Focus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_focus_hard_suspend: Option<bool>,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            auto_trim_enabled: true,
            auto_trim_threshold_pct: 80,
            idle_threshold_secs: 600,
            active_profile: "Work".to_string(),
            hard_suspend_enabled: false,
            excluded_processes: vec!["code.exe".to_string(), "chrome.exe".to_string()],
            start_on_login: true,
            notifications_enabled: true,
            theme: "system".to_string(),
            trim_history_limit: 50,
            first_launch_complete: false,
            pre_focus_idle_threshold: None,
            pre_focus_hard_suspend: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the path to `%APPDATA%\Gingify\config.json`.
fn config_path() -> Result<PathBuf> {
    let appdata = std::env::var("APPDATA")
        .context("APPDATA environment variable is not set")?;
    Ok(PathBuf::from(appdata).join("Gingify").join("config.json"))
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

impl UserConfig {
    /// Load config from disk.
    ///
    /// - If the file is missing, returns `Default::default()`.
    /// - If the file is corrupt / unparseable, logs a warning and returns `Default::default()`.
    pub fn load() -> Result<Self> {
        let path = config_path()?;

        if !path.exists() {
            log::info!("Config file not found at {:?} — using defaults", path);
            return Ok(Self::default());
        }

        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file at {:?}", path))?;

        match serde_json::from_str::<Self>(&raw) {
            Ok(cfg) => {
                log::info!("Config loaded from {:?}", path);
                Ok(cfg)
            }
            Err(e) => {
                log::warn!(
                    "Config file at {:?} is corrupt ({}) — resetting to defaults",
                    path,
                    e
                );
                Ok(Self::default())
            }
        }
    }

    /// Persist config to disk, creating `%APPDATA%\Gingify\` if needed.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;

        // Ensure directory exists
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create config directory {:?}", dir))?;
        }

        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialise config to JSON")?;

        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write config file at {:?}", path))?;

        log::info!("Config saved to {:?}", path);
        Ok(())
    }
}
