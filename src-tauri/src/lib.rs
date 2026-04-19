//! Gingify desktop application — Tauri v2 entry point.
//!
//! Initialises logging, loads user config, constructs shared state, sets up
//! the system tray and all IPC command handlers, then runs the Tauri event loop.
//!
//! # Log files
//! All log output (including from release builds) goes to:
//!   `%APPDATA%\Gingify\gingify.log`   — rolling (>5 MB rotates to .old)
//!   `%APPDATA%\Gingify\crash.log`      — panic traces only

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod core;
mod state;

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{
    AppHandle, Emitter, Listener, Manager,
    menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use state::app_state::{AppState, Profile, SharedState};
use state::config::UserConfig;

use commands::config_commands::{add_exclusion, get_config, remove_exclusion, trigger_welcome_notification, update_config, verify_suspend_capable};
use commands::data_commands::{get_bloat_list, get_process_list, get_ram_stats, get_trim_history};
use commands::profile_commands::{get_current_profile, set_profile};
use commands::trim_commands::{
    resume_bloat, resume_process, suspend_all_bloat, suspend_bloat, suspend_process, trim_all, trim_bloat, trim_process,
};

use core::suspender;
use core::trimmer;
use core::profiles;
use core::updater;

// ---------------------------------------------------------------------------
// Logging bootstrap — must be called before any log::* call
// ---------------------------------------------------------------------------

/// Initialise a file-based logger that works in **release builds** (where
/// `windows_subsystem = "windows"` kills stderr and swallows all env_logger
/// output).
///
/// Log file: `%APPDATA%\Gingify\gingify.log`
/// Rotation : if the file exceeds 5 MB it is renamed to `gingify.log.old`
///            and a fresh log is started.
///
/// Returns the log directory path so the panic hook can use it.
fn init_logging() -> std::path::PathBuf {
    let log_dir = std::env::var("APPDATA")
        .map(|d| std::path::PathBuf::from(d).join("Gingify"))
        .unwrap_or_else(|_| std::env::temp_dir().join("Gingify"));
    let _ = std::fs::create_dir_all(&log_dir);

    let log_path = log_dir.join("gingify.log");

    // Simple rotation: rename if > 5 MB so the file never grows unbounded
    if let Ok(meta) = std::fs::metadata(&log_path) {
        if meta.len() > 5 * 1024 * 1024 {
            let _ = std::fs::rename(&log_path, log_dir.join("gingify.log.old"));
        }
    }

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("Gingify: cannot open log file");

    simplelog::WriteLogger::init(
        simplelog::LevelFilter::Info,
        simplelog::Config::default(),
        log_file,
    )
    .expect("Gingify: cannot init logger");

    log_dir
}

/// Install a panic hook that writes the panic message to
/// `%APPDATA%\Gingify\crash.log` **and** to the normal log file.
///
/// This fires even for startup panics that occur before Tauri initialises,
/// ensuring every unhandled panic leaves a trace on disk.
fn install_panic_hook(log_dir: std::path::PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();

        // Write through the normal logger (may already be shut down on some
        // panic paths, so we also write directly below).
        log::error!("PANIC: {msg}");

        // Write directly to crash.log — survives even if the logger is dead.
        let crash_path = log_dir.join("crash.log");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("[{ts}] PANIC: {msg}\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_path)
            .map(|mut f| {
                use std::io::Write;
                let _ = f.write_all(line.as_bytes());
            });
    }));
}

// ---------------------------------------------------------------------------
// Safe quit helper — Task 2 (PROMPT_DESKTOP_10)
// ---------------------------------------------------------------------------

/// Perform a clean shutdown sequence before exiting:
/// 1. Resume all hard-suspended processes so no apps remain frozen.
/// 2. Clear the persisted suspended-PID set.
/// 3. Flush the latest config to disk.
/// 4. Log the shutdown and exit.
fn safe_quit(app: &AppHandle) {
    log::info!("safe_quit: starting clean shutdown sequence");

    let quit_state = app.state::<SharedState>().inner().clone();

    // Step 1 & 2: resume all + clear persisted set
    match suspender::load_suspender() {
        Ok(ctx) => {
            suspender::resume_all_suspended(&ctx, quit_state.clone());
        }
        Err(e) => log::warn!("safe_quit: could not load suspender for cleanup — {e}"),
    }
    if let Err(e) = suspender::save_suspended_set(&HashSet::new()) {
        log::warn!("safe_quit: failed to clear suspended_set file — {e}");
    }

    // Step 3: flush config to disk
    {
        let config_snapshot = quit_state.lock().config.clone();
        if let Err(e) = config_snapshot.save() {
            log::warn!("safe_quit: failed to save config — {e}");
        }
    }

    log::info!("Gingify shutting down cleanly");
    app.exit(0);
}

// ---------------------------------------------------------------------------
// Application entry point
// ---------------------------------------------------------------------------

pub fn run() {
    // -------------------------------------------------------------------------
    // 1. Initialise logging (file-based — works in release builds too)
    // -------------------------------------------------------------------------
    let log_dir = init_logging();
    install_panic_hook(log_dir.clone());
    log::info!(
        "===== Gingify v{} starting up (log: {}) =====",
        env!("CARGO_PKG_VERSION"),
        log_dir.join("gingify.log").display()
    );

    // -------------------------------------------------------------------------
    // 2. Load user config from disk (falls back to defaults on first run)
    // -------------------------------------------------------------------------
    let config = UserConfig::load().unwrap_or_else(|e| {
        log::error!("Failed to load config: {e} — using defaults");
        UserConfig::default()
    });

    // -------------------------------------------------------------------------
    // 3. Load trim history from disk (empty vec on fresh install or corrupt file)
    // -------------------------------------------------------------------------
    let trim_history = core::trimmer::load_history();

    // -------------------------------------------------------------------------
    // 4a. Load suspended PIDs persisted from a previous run (orphan cleanup)
    // -------------------------------------------------------------------------
    let orphaned_pids = suspender::load_suspended_set();

    // -------------------------------------------------------------------------
    // 4b. Load SuspenderContext (dynamic ntdll function pointers)
    // -------------------------------------------------------------------------
    let suspender_ctx = match suspender::load_suspender() {
        Ok(ctx) => {
            log::info!("Gingify: NtSuspendProcess loaded successfully");
            Some(ctx)
        }
        Err(e) => {
            log::error!("Gingify: failed to load NtSuspendProcess: {e} — hard suspend disabled");
            None
        }
    };

    // -------------------------------------------------------------------------
    // 5. Initialise shared application state
    // -------------------------------------------------------------------------
    let mut initial_state = AppState::new(config);
    initial_state.trim_history = trim_history;
    initial_state.suspended_set = orphaned_pids.clone();
    initial_state.suspender_ctx = suspender_ctx;
    let shared_state: SharedState = Arc::new(Mutex::new(initial_state));

    // -------------------------------------------------------------------------
    // 6. Resume orphaned PIDs from previous session (Gingify was force-killed)
    // -------------------------------------------------------------------------
    if !orphaned_pids.is_empty() {
        let resume_state = shared_state.clone();
        std::thread::spawn(move || {
            match suspender::load_suspender() {
                Ok(ctx) => {
                    log::info!(
                        "Gingify startup: resuming {} orphaned suspended PIDs",
                        orphaned_pids.len()
                    );
                    suspender::resume_all_suspended(&ctx, resume_state);
                }
                Err(e) => {
                    log::error!("Gingify startup: orphan cleanup failed — {e}");
                }
            }
        });
    }

    // -------------------------------------------------------------------------
    // 7. Build and run the Tauri application
    // -------------------------------------------------------------------------
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(shared_state)
        .setup(|app| {
            // -----------------------------------------------------------------
            // WebView2 detection is handled by Tauri automatically
            // -----------------------------------------------------------------

            // -----------------------------------------------------------------
            // 4a. Build the tray context menu — Task 1 (PROMPT_DESKTOP_10)
            //
            // Profile items use CheckMenuItem so we can show/remove the
            // active-profile checkmark without rebuilding the entire menu.
            // -----------------------------------------------------------------
            let free_now = MenuItemBuilder::with_id("free_now", "Free RAM Now").build(app)?;
            let sep1     = PredefinedMenuItem::separator(app)?;

            // Read initial active profile to set the correct initial checkmark
            let initial_profile = {
                let st = app.state::<SharedState>().inner().lock();
                st.active_profile.clone()
            };

            let chk_work = CheckMenuItemBuilder::with_id("profile_work", "Work")
                .checked(initial_profile == Profile::Work)
                .build(app)?;
            let chk_gaming = CheckMenuItemBuilder::with_id("profile_gaming", "Gaming")
                .checked(initial_profile == Profile::Gaming)
                .build(app)?;
            let chk_focus = CheckMenuItemBuilder::with_id("profile_focus", "Focus")
                .checked(matches!(initial_profile, Profile::Focus))
                .build(app)?;

            let sep2        = PredefinedMenuItem::separator(app)?;
            let open_detail = MenuItemBuilder::with_id("open_detail", "Open Detail Window").build(app)?;
            let settings    = MenuItemBuilder::with_id("settings", "Settings").build(app)?;
            let sep3        = PredefinedMenuItem::separator(app)?;
            let check_upd   = MenuItemBuilder::with_id("check_updates", "Check for Updates").build(app)?;
            let sep4        = PredefinedMenuItem::separator(app)?;
            let quit        = MenuItemBuilder::with_id("quit", "Quit Gingify").build(app)?;

            let tray_menu = MenuBuilder::new(app)
                .item(&free_now)
                .item(&sep1)
                .item(&chk_work)
                .item(&chk_gaming)
                .item(&chk_focus)
                .item(&sep2)
                .item(&open_detail)
                .item(&settings)
                .item(&sep3)
                .item(&check_upd)
                .item(&sep4)
                .item(&quit)
                .build()?;

            // Clone CheckMenuItem handles into the event closure so we can
            // update checkmarks without rebuilding the menu.
            let chk_work_c   = chk_work.clone();
            let chk_gaming_c = chk_gaming.clone();
            let chk_focus_c  = chk_focus.clone();

            // -----------------------------------------------------------------
            // 4b. Build the tray icon
            //
            // IMPORTANT: In Tauri v2, TrayIcon::drop() calls remove_tray_by_id()
            // which deletes the OS tray icon. We must call std::mem::forget() on
            // the returned handle so the icon lives for the entire process lifetime.
            // -----------------------------------------------------------------
            // Capture state for the tray click handler so we can emit fresh RAM
            // stats the instant the popup opens (instead of waiting up to 5 s for
            // the next monitor cycle).
            let tray_state = app.state::<SharedState>().inner().clone();

            let tray = TrayIconBuilder::with_id("main")
                .icon({
                    // Decode the bundled 32×32 PNG to raw RGBA so we can hand it
                    // to Tauri's Image::new_owned (Image::from_bytes doesn't exist
                    // in Tauri 2.10.x — only new/new_owned accept raw pixels).
                    let bytes: &[u8] = include_bytes!("../icons/32x32.png");
                    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
                    let mut reader = decoder.read_info()
                        .map_err(|e| format!("tray icon decode (read_info): {e}"))?;
                    let mut buf = vec![0u8; reader.output_buffer_size()];
                    let fi = reader.next_frame(&mut buf)
                        .map_err(|e| format!("tray icon decode (next_frame): {e}"))?;
                    let rgba = buf[..fi.buffer_size()].to_vec();
                    tauri::image::Image::new_owned(rgba, fi.width, fi.height)
                })
                .tooltip("Gingify — RAM Manager")
                .menu(&tray_menu)
                .on_menu_event(move |app, event| {
                    match event.id().as_ref() {
                        // ── Free RAM Now ──────────────────────────────────────
                        "free_now" => {
                            log::info!("Tray: Free RAM Now clicked");
                            let st = app.state::<SharedState>().inner().clone();
                            std::thread::spawn(move || {
                                let result = trimmer::soft_trim_all(
                                    &st,
                                    30,
                                    state::app_state::TrimTrigger::Manual,
                                );
                                log::info!(
                                    "Tray: Free RAM Now — freed {} bytes, {} processes",
                                    result.freed_bytes,
                                    result.processes_trimmed
                                );
                            });
                        }

                        // ── Profile: Work ─────────────────────────────────────
                        "profile_work" => {
                            log::info!("Tray: Profile → Work");
                            let st = app.state::<SharedState>().inner().clone();
                            // Update checkmarks
                            let _ = chk_work_c.set_checked(true);
                            let _ = chk_gaming_c.set_checked(false);
                            let _ = chk_focus_c.set_checked(false);
                            std::thread::spawn(move || {
                                match suspender::load_suspender() {
                                    Ok(ctx) => { let _ = profiles::activate_profile(Profile::Work, st, &ctx); }
                                    Err(e)  => log::warn!("Tray: profile switch failed — {e}"),
                                }
                            });
                        }

                        // ── Profile: Gaming ───────────────────────────────────
                        "profile_gaming" => {
                            log::info!("Tray: Profile → Gaming");
                            let st = app.state::<SharedState>().inner().clone();
                            let _ = chk_work_c.set_checked(false);
                            let _ = chk_gaming_c.set_checked(true);
                            let _ = chk_focus_c.set_checked(false);
                            std::thread::spawn(move || {
                                match suspender::load_suspender() {
                                    Ok(ctx) => {
                                        if let Err(e) = profiles::activate_profile(Profile::Gaming, st, &ctx) {
                                            log::warn!("Tray: Gaming Mode activation failed — {e}");
                                        }
                                    }
                                    Err(e) => log::warn!("Tray: profile switch failed — {e}"),
                                }
                            });
                        }

                        // ── Profile: Focus ────────────────────────────────────
                        "profile_focus" => {
                            log::info!("Tray: Profile → Focus");
                            let st = app.state::<SharedState>().inner().clone();
                            let _ = chk_work_c.set_checked(false);
                            let _ = chk_gaming_c.set_checked(false);
                            let _ = chk_focus_c.set_checked(true);
                            std::thread::spawn(move || {
                                match suspender::load_suspender() {
                                    Ok(ctx) => { let _ = profiles::activate_profile(Profile::Focus, st, &ctx); }
                                    Err(e)  => log::warn!("Tray: profile switch failed — {e}"),
                                }
                            });
                        }

                        // ── Open Detail Window ─────────────────────────────────
                        "open_detail" => {
                            log::info!("Tray: Open Detail Window");
                            show_detail_window(app);
                        }

                        // ── Settings ──────────────────────────────────────────
                        // Shows the detail window and switches it to the Settings tab
                        "settings" => {
                            log::info!("Tray: Settings");
                            show_detail_window(app);
                            // Emit the switch-tab event — the frontend listens for it
                            if let Err(e) = app.emit("gingify://switch-tab", "settings") {
                                log::warn!("Tray: failed to emit switch-tab event — {e}");
                            }
                        }

                        // ── Check for Updates ─────────────────────────────────
                        "check_updates" => {
                            log::info!("Tray: Check for Updates");
                            tokio::spawn(async {
                                match updater::check_for_update().await {
                                    Ok(Some(info)) => {
                                        core::notifications::notify_update_available(
                                            &info.version,
                                            &info.download_url,
                                        );
                                    }
                                    Ok(None) => {
                                        core::notifications::notify_up_to_date();
                                    }
                                    Err(e) => {
                                        log::warn!("Tray: update check failed — {e}");
                                    }
                                }
                            });
                        }

                        // ── Quit Gingify ──────────────────────────────────────
                        "quit" => {
                            log::info!("Tray: Quit Gingify");
                            safe_quit(app);
                        }

                        _ => {}
                    }
                })
                .on_tray_icon_event(move |tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(popup) = app.get_webview_window("popup") {
                            // Position popup above the tray area (bottom-right)
                            if let Ok(Some(monitor)) = popup.current_monitor() {
                                    let screen_size = monitor.size();
                                    let scale       = monitor.scale_factor();
                                    let popup_w     = (380.0 * scale) as i32;
                                    let popup_h     = (480.0 * scale) as i32;
                                    let margin      = (12.0  * scale) as i32;
                                    let taskbar_h   = (48.0  * scale) as i32;
                                    let x = screen_size.width  as i32 - popup_w - margin;
                                    let y = screen_size.height as i32 - popup_h - taskbar_h - margin;
                                    let _ = popup.set_position(tauri::PhysicalPosition::new(x, y));
                                }
                            let _ = popup.show();
                            let _ = popup.set_focus();

                            // Emit fresh RAM stats immediately so popup shows
                            // current data without waiting for the next 5 s monitor tick.
                            let fresh_stats = {
                                let st = tray_state.lock();
                                st.ram_stats.clone()
                            };
                            let _ = app.emit("gingify://ram-update", &fresh_stats);
                        } else {
                            log::warn!("Tray: popup window not found");
                        }
                    }
                })
                .build(app)?;

            // Prevent Drop from calling remove_tray_by_id() — the tray icon must
            // live for the entire process, not just the setup closure scope.
            std::mem::forget(tray);

            // -----------------------------------------------------------------
            // 4c. Start the background monitor loop
            // -----------------------------------------------------------------
            let monitor_state  = app.state::<SharedState>().inner().clone();
            let monitor_handle: AppHandle = app.handle().clone();
            std::thread::spawn(move || {
                core::monitor::start_monitor(monitor_state, monitor_handle);
            });

            // -----------------------------------------------------------------
            // 4d. Listen for gingify://open-detail event emitted by popup.js
            // -----------------------------------------------------------------
            {
                let detail_handle = app.handle().clone();
                app.listen("gingify://open-detail", move |_event| {
                    show_detail_window(&detail_handle);
                });
            }

            // -----------------------------------------------------------------
            // 4f. Wire CloseRequested on all windows → safe_quit — Task 2
            // -----------------------------------------------------------------
            for window_label in ["popup", "detail", "onboarding"] {
                if let Some(win) = app.get_webview_window(window_label) {
                    let win_clone = win.clone();
                    win.on_window_event(move |event| {
                        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                            api.prevent_close();
                            let _ = win_clone.hide();
                        }
                    });
                }
            }

            // -----------------------------------------------------------------
            // 4g. Start the foreground-change hook (auto-resume on app open)
            // -----------------------------------------------------------------
            let hook_state  = app.state::<SharedState>().inner().clone();
            let hook_handle: AppHandle = app.handle().clone();
            suspender::start_foreground_hook(hook_state, hook_handle);

            // -----------------------------------------------------------------
            // 4h. Named-event listener for second-instance activation
            //
            // When the user clicks the app icon while Gingify is already
            // running, main.rs's already_exists branch signals the named event
            // "Local\GingifyActivate".  This background thread wakes up and
            // shows the detail window — more reliable than FindWindowW by title.
            // -----------------------------------------------------------------
            {
                use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
                use windows::Win32::Foundation::CloseHandle;

                let activate_handle = app.handle().clone();
                let event_name: Vec<u16> = "Local\\GingifyActivate\0".encode_utf16().collect();

                std::thread::spawn(move || {
                    let event_h = unsafe {
                        CreateEventW(
                            None,
                            false,  // auto-reset: each SetEvent wakes exactly one waiter
                            false,
                            windows::core::PCWSTR(event_name.as_ptr()),
                        )
                    };
                    match event_h {
                        Ok(h) => {
                            loop {
                                // Block until the second instance signals us
                                let r = unsafe { WaitForSingleObject(h, INFINITE) };
                                if r == windows::Win32::Foundation::WAIT_EVENT(0) {
                                    // WAIT_OBJECT_0 — show tray popup (spec: duplicate instance → popup)
                                    if let Some(win) = activate_handle.get_webview_window("popup") {
                                        let _ = win.show();
                                        let _ = win.set_focus();
                                    } else if let Some(win) = activate_handle.get_webview_window("detail") {
                                        let _ = win.show();
                                        let _ = win.set_focus();
                                    }
                                } else {
                                    // WAIT_FAILED or handle closed — exit the thread
                                    break;
                                }
                            }
                            unsafe { let _ = CloseHandle(h); }
                        }
                        Err(e) => log::warn!("GingifyActivate event create failed: {e}"),
                    }
                });
            }

            // -----------------------------------------------------------------
            // 4i. Schedule a background update check (fires 10 s after startup)
            // -----------------------------------------------------------------
            updater::schedule_update_check(app.handle().clone());

            log::info!("Gingify tray initialised successfully");

            // -----------------------------------------------------------------
            // 4j. First-launch onboarding vs. silent startup
            // -----------------------------------------------------------------
            let first_launch = {
                let st = app.state::<SharedState>().inner().lock();
                !st.config.first_launch_complete
            };

            if first_launch {
                log::info!("First launch detected — showing onboarding window");
                if let Some(win) = app.get_webview_window("onboarding") {
                    let _ = win.show();
                    let _ = win.set_focus();
                } else {
                    log::warn!("Onboarding window not found — check tauri.conf.json");
                }
            } else {
                log::info!("Returning user — silent tray startup");
            }

            Ok(())
        })
        // Register all IPC command handlers
        .invoke_handler(tauri::generate_handler![
            // Trim commands
            trim_all,
            trim_process,
            suspend_process,
            resume_process,
            trim_bloat,
            suspend_bloat,
            suspend_all_bloat,
            resume_bloat,
            // Data commands
            get_process_list,
            get_ram_stats,
            get_bloat_list,
            get_trim_history,
            // Profile commands
            set_profile,
            get_current_profile,
            // Config commands
            get_config,
            update_config,
            add_exclusion,
            remove_exclusion,
            trigger_welcome_notification,
            verify_suspend_capable,
            // App commands
            quit_app,
            check_for_updates_cmd,
            open_url_cmd,
            // Window commands
            hide_window,
            open_detail_window,
            open_detail_window_tab,
            open_onboarding_window,
            is_admin,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Gingify");
}

// ---------------------------------------------------------------------------
// Helper: show and focus the detail window
// ---------------------------------------------------------------------------

/// Show and focus the detail window. Creates it from the configuration in
/// `tauri.conf.json` if it already exists (it will, since it is declared as a
/// static window); otherwise logs a warning.
fn show_detail_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("detail") {
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        log::warn!("detail window not found — check tauri.conf.json");
    }
}

#[tauri::command]
fn open_detail_window(app_handle: AppHandle) {
    show_detail_window(&app_handle);
}

/// Show the detail window and switch it to the named tab.
///
/// Called from popup.js "See all processes →" link.
///
/// ORDER IS CRITICAL on Windows:
/// 1. show+focus the detail window WHILE the popup still owns the foreground
///    lock — SetForegroundWindow only works if the calling process is the
///    current foreground owner.  If we hide the popup first we lose that lock
///    and the detail window appears without input focus ("can't click anything").
/// 2. THEN hide the popup — it's already behind the detail window and the
///    popup's JS blur handler will fire but _ipcBusy is still true at that
///    point so it returns early without starting the hide timer.
#[tauri::command]
fn open_detail_window_tab(app_handle: AppHandle, tab: String) {
    // Step 1: show + focus detail while we still own the foreground lock
    show_detail_window(&app_handle);
    log::info!("open_detail_window_tab: detail window shown");
    // Step 2: switch the detail window to the requested tab
    let _ = app_handle.emit("gingify://switch-tab", tab);
    // Step 3: now hide the popup — detail already has focus
    if let Some(popup) = app_handle.get_webview_window("popup") {
        let _ = popup.hide();
        log::info!("open_detail_window_tab: popup hidden");
    }
}

#[tauri::command]
fn open_onboarding_window(app_handle: AppHandle) {
    if let Some(win) = app_handle.get_webview_window("onboarding") {
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        log::warn!("open_onboarding_window: onboarding window not found");
    }
}

#[tauri::command]
fn hide_window(label: String, app_handle: AppHandle) {
    if let Some(win) = app_handle.get_webview_window(&label) {
        let _ = win.hide();
    }
}

/// Quit the application cleanly — resumes all suspended processes, flushes
/// config, then exits. Called by the About → Quit button in the detail window.
#[tauri::command]
fn quit_app(app_handle: AppHandle) {
    safe_quit(&app_handle);
}

/// Trigger an update check from the frontend (About → Check for Updates).
/// Notifies via toast and emits `gingify://update-available` if an update is found.
#[tauri::command]
async fn check_for_updates_cmd(app_handle: AppHandle) {
    tokio::spawn(async move {
        match updater::check_for_update().await {
            Ok(Some(info)) => {
                core::notifications::notify_update_available(&info.version, &info.download_url);
                let _ = app_handle.emit("gingify://update-available", &info.version);
            }
            Ok(None) => core::notifications::notify_up_to_date(),
            Err(e) => log::warn!("check_for_updates_cmd: {e}"),
        }
    });
}

/// Open a URL in the system default browser using the shell plugin.
/// Used by the frontend for the GitHub link in the About section.
#[tauri::command]
#[allow(deprecated)]
fn open_url_cmd(url: String, app_handle: AppHandle) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    app_handle.shell().open(&url, None).map_err(|e| e.to_string())
}

/// Returns true if the current process is running with administrator privileges.
/// Used by the frontend to guard the Gaming Mode (hard suspend) toggle.
#[tauri::command]
fn is_admin() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut return_length: u32 = 0;
        let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut std::ffi::c_void),
            size,
            &mut return_length,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}
