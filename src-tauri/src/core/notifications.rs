//! Windows toast notification dispatcher — sends user-facing alerts for
//! auto-trim events, gaming mode activation, high-RAM warnings, and updates.
//!
//! All functions silently no-op (log at debug level) if the notification
//! system fails to initialise. Never panics.

#![allow(dead_code)]

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use win_toast_notify::WinToastNotify;

use crate::state::app_state::TrimResult;

// ---------------------------------------------------------------------------
// High-RAM rate limiter — at most one notification per hour
// ---------------------------------------------------------------------------

/// Tracks the last time the high-RAM notification was fired so we can enforce
/// a one-per-hour maximum.
static LAST_HIGH_RAM_NOTIFY: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn high_ram_limiter() -> &'static Mutex<Option<Instant>> {
    LAST_HIGH_RAM_NOTIFY.get_or_init(|| Mutex::new(None))
}

// ---------------------------------------------------------------------------
// App identity used by the toast notification system
// ---------------------------------------------------------------------------

/// Application ID registered with Windows for toast attribution.
const APP_ID: &str = "Gingify";

// ---------------------------------------------------------------------------
// Public notification functions
// ---------------------------------------------------------------------------

/// Show a toast after an automatic or manual trim completes.
///
/// Skips the notification silently if no bytes were freed (nothing to report).
/// The `config` check for `notifications_enabled` is the caller's responsibility
/// (see `monitor.rs` and `trim_commands.rs` call sites).
pub fn notify_trim_result(result: &TrimResult) {
    if result.processes_trimmed == 0 {
        log::debug!("notifications: trim produced no results — skipping toast");
        return;
    }

    let freed_gb = result.freed_bytes as f64 / 1_073_741_824.0;
    let body = format!(
        "Freed {freed_gb:.1} GB — {} apps snoozed",
        result.processes_trimmed
    );

    toast(APP_ID, "Gingify", &body);
}

/// Notify the user that Gaming Mode has been activated.
pub fn notify_gaming_mode_on(freed_gb: f64) {
    let body = format!("Gaming Mode on — freed {freed_gb:.1} GB for your game");
    toast(APP_ID, "Gingify", &body);
}

/// Notify the user that Gaming Mode has been deactivated.
pub fn notify_gaming_mode_off() {
    toast(APP_ID, "Gingify", "Gaming Mode off — apps restored");
}

/// Notify the user that a hard-suspended process has been woken up.
///
/// Only calls this for processes that were *hard-suspended* (NtSuspendProcess),
/// not soft-trimmed.
pub fn notify_process_resumed(process_name: &str) {
    let body = format!("[{process_name}] has been woken up");
    toast(APP_ID, "Gingify", &body);
}

/// Notify that RAM pressure is high or critically high.
///
/// `is_critical` selects between a stronger "critically full / slowing down"
/// message and a softer "consider freeing" message.
///
/// Rate-limited to at most once per hour so it doesn't spam the user.
pub fn notify_high_ram(pressure_pct: f32, is_critical: bool) {
    let mut last = high_ram_limiter().lock();

    // Enforce 1-hour cooldown
    if let Some(t) = *last {
        if t.elapsed() < Duration::from_secs(3600) {
            log::debug!("notifications: high-RAM notify rate-limited (last was {:?} ago)", t.elapsed());
            return;
        }
    }

    *last = Some(Instant::now());
    drop(last);

    let body = if is_critical {
        format!(
            "RAM is critically full ({pressure_pct:.0}% used) — your device is likely slowing down. Open Gingify to free RAM now."
        )
    } else {
        format!(
            "RAM at {pressure_pct:.0}% — your device may start slowing down. Consider opening Gingify to free RAM."
        )
    };
    toast(APP_ID, "Gingify — High RAM", &body);
}

/// Notify that a new version of Gingify is available.
///
/// Shows the version number in the body. The human can click the notification
/// to open the download URL — action button support depends on the chosen crate
/// implementation; this uses a simple informational toast for reliability.
pub fn notify_update_available(version: &str, _download_url: &str) {
    let body = format!("v{version} is available — right-click tray to update");
    toast(APP_ID, "Gingify Update", &body);
}

/// Fired from the tray "Check for Updates" item when already on the latest version.
pub fn notify_up_to_date() {
    toast(APP_ID, "Gingify", "You're on the latest version.");
}

/// Fired once after the user completes first-launch onboarding.
///
/// Tells the user where to find Gingify after the onboarding window closes.
pub fn notify_welcome() {
    toast(
        APP_ID,
        "Gingify",
        "Gingify is running. Click the tray icon anytime to see your RAM.",
    );
}

// ---------------------------------------------------------------------------
// Internal helper
// ---------------------------------------------------------------------------

/// Dispatch a single toast notification.
///
/// Silently logs at `debug` level on any failure — never propagates errors.
fn toast(app_id: &str, title: &str, body: &str) {
    let result = WinToastNotify::new()
        .set_app_id(app_id)
        .set_title(title)
        .set_messages(vec![body])
        .show();

    match result {
        Ok(()) => {
            log::debug!("notifications: toast shown — title={title:?} body={body:?}");
        }
        Err(e) => {
            log::debug!(
                "notifications: toast failed (non-fatal) — title={title:?} error={e:?}"
            );
        }
    }
}
