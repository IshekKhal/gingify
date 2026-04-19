//! Update checker — fetches the latest Gingify version from GitHub Releases
//! and notifies the user via a toast if a newer version is available.
//!
//! Call `schedule_update_check(app_handle)` once from `lib.rs` after the
//! Tauri app is built. It spawns a Tokio task that waits 10 seconds and
//! then performs a single check per app session.

#![allow(dead_code)]

use tauri::AppHandle;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Configuration constants
// ---------------------------------------------------------------------------

const GITHUB_REPO: &str = "IshekKhal/gingify";

/// The current application version, read from Cargo.toml at compile time.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Information about an available update.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// The new version string (without leading 'v'), e.g. "1.2.0".
    pub version: String,
    /// Direct download URL from the GitHub release (browser_download_url or html_url).
    pub download_url: String,
}

/// Errors that can occur during the version check.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("Network request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Failed to parse remote version '{0}': {1}")]
    VersionParse(String, semver::Error),
    #[error("Failed to parse current version '{0}': {1}")]
    CurrentVersionParse(String, semver::Error),
    #[error("GitHub API response missing expected field: {0}")]
    MissingField(String),
}

// ---------------------------------------------------------------------------
// Version check
// ---------------------------------------------------------------------------

/// Query the GitHub Releases API for the latest release and compare it with
/// the compiled-in version.
///
/// Returns:
/// - `Ok(Some(info))` — a newer version is available
/// - `Ok(None)`       — already on the latest version
/// - `Err(_)`         — network / parse error (caller logs, does not surface to user)
pub async fn check_for_update() -> Result<Option<UpdateInfo>, UpdateError> {
    let url = format!(
        "https://api.github.com/repos/{GITHUB_REPO}/releases/latest"
    );

    let user_agent = format!("Gingify/{CURRENT_VERSION}");

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", &user_agent)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    // Extract tag_name (e.g. "v1.2.0")
    let tag_name = response
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| UpdateError::MissingField("tag_name".to_string()))?;

    // Extract download URL — prefer html_url as a fallback
    let download_url = response
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://github.com")
        .to_string();

    // Strip leading 'v' and parse as semver
    let remote_str = tag_name.trim_start_matches('v');
    let remote_ver = semver::Version::parse(remote_str)
        .map_err(|e| UpdateError::VersionParse(remote_str.to_string(), e))?;

    let current_ver = semver::Version::parse(CURRENT_VERSION)
        .map_err(|e| UpdateError::CurrentVersionParse(CURRENT_VERSION.to_string(), e))?;

    log::debug!(
        "updater: remote={} current={}", remote_ver, current_ver
    );

    if remote_ver > current_ver {
        Ok(Some(UpdateInfo {
            version: remote_str.to_string(),
            download_url,
        }))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Scheduled background check
// ---------------------------------------------------------------------------

/// Spawn a Tokio task that waits 10 seconds after startup and performs a
/// single version check for this app session.
///
/// If an update is found, calls `notifications::notify_update_available`.
pub fn schedule_update_check(app_handle: AppHandle) {
    // Spawn a background OS thread with its own Tokio runtime.
    // We cannot use tokio::spawn here because setup() runs before Tauri
    // starts the async runtime — calling tokio::spawn without a reactor panics.
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                log::error!("updater: failed to create Tokio runtime: {e}");
                return;
            }
        };
        rt.block_on(async move {
        // Wait 10 seconds so the app settles before hitting the network
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        let _ = &app_handle; // suppress unused warning until we use it below

        match check_for_update().await {
            Ok(Some(info)) => {
                log::info!(
                    "updater: new version {} available — {}",
                    info.version, info.download_url
                );
                crate::core::notifications::notify_update_available(
                    &info.version,
                    &info.download_url,
                );
            }
            Ok(None) => {
                log::info!("updater: already on the latest version ({CURRENT_VERSION})");
            }
            Err(e) => {
                log::warn!("updater: version check failed — {e}");
            }
        }
    }); // end rt.block_on
    }); // end std::thread::spawn
}
