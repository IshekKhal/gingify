//! Hard process suspender — dynamically loads NtSuspendProcess / NtResumeProcess
//! from ntdll.dll to freeze and thaw processes on demand (Gaming Mode).
//!
//! `NtSuspendProcess` is an undocumented but stable ntdll export used by
//! Process Hacker, x64dbg, and similar legitimate tools. It is not exported
//! from the `windows` crate's public surface, so we load it at runtime via
//! `LoadLibraryW` + `GetProcAddress`.

#![allow(dead_code)]

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SUSPEND_RESUME,
};
use windows::Win32::UI::Accessibility::{
    SetWinEventHook, HWINEVENTHOOK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, GetWindowThreadProcessId, MSG,
    EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT,
};

use tauri::Emitter;

use crate::state::app_state::SharedState;

// ---------------------------------------------------------------------------
// FFI function type definitions
// ---------------------------------------------------------------------------

/// Signature of the undocumented `NtSuspendProcess` ntdll export.
#[allow(non_snake_case)]
type NtSuspendProcessFn = unsafe extern "system" fn(ProcessHandle: HANDLE) -> i32;

/// Signature of the undocumented `NtResumeProcess` ntdll export.
#[allow(non_snake_case)]
type NtResumeProcessFn = unsafe extern "system" fn(ProcessHandle: HANDLE) -> i32;

// ---------------------------------------------------------------------------
// SuspendError
// ---------------------------------------------------------------------------

/// Errors that can arise during a suspend / resume operation.
#[derive(Debug, thiserror::Error)]
pub enum SuspendError {
    #[error("NtSuspendProcess not available on this system")]
    NotAvailable,
    #[error("Access denied for PID {0}")]
    AccessDenied(u32),
    #[error("Suspend failed for PID {0}: NTSTATUS {1:#x}")]
    SuspendFailed(u32, i32),
    #[error("Resume failed for PID {0}: NTSTATUS {1:#x}")]
    ResumeFailed(u32, i32),
}

// ---------------------------------------------------------------------------
// SuspenderContext — loaded once at startup and stored in AppState
// ---------------------------------------------------------------------------

/// Holds the dynamically resolved function pointers for `NtSuspendProcess`
/// and `NtResumeProcess`. Loaded once during app init; shared via
/// `AppState.suspender_ctx: Option<SuspenderContext>`.
///
/// # Safety
///
/// The function pointers are valid for the lifetime of the process because
/// ntdll.dll is always resident and is never unloaded. The `Send` + `Sync`
/// implementations below are safe for the same reason.
pub struct SuspenderContext {
    nt_suspend: NtSuspendProcessFn,
    nt_resume: NtResumeProcessFn,
}

// Manual Debug impl — fn pointers don't derive Debug, but we can print their addresses.
impl std::fmt::Debug for SuspenderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuspenderContext")
            .field("nt_suspend", &(self.nt_suspend as usize))
            .field("nt_resume", &(self.nt_resume as usize))
            .finish()
    }
}

// SAFETY: ntdll function pointers are stable for the process lifetime.
unsafe impl Send for SuspenderContext {}
unsafe impl Sync for SuspenderContext {}

// ---------------------------------------------------------------------------
// load_suspender — called once during app init
// ---------------------------------------------------------------------------

/// Dynamically resolve `NtSuspendProcess` and `NtResumeProcess` from
/// `ntdll.dll`.
///
/// Returns `Err(SuspendError::NotAvailable)` if either symbol cannot be
/// resolved (should never happen on any supported Windows version).
pub fn load_suspender() -> Result<SuspenderContext, SuspendError> {
    unsafe {
        // ntdll.dll is always loaded into every Windows process. Calling
        // LoadLibraryW here just increments its reference count and gives us
        // a module handle — it does not actually load anything new.
        let ntdll_wide: Vec<u16> = "ntdll.dll\0"
            .encode_utf16()
            .collect();

        let module = LoadLibraryW(windows::core::PCWSTR(ntdll_wide.as_ptr()))
            .map_err(|_| SuspendError::NotAvailable)?;

        // Resolve NtSuspendProcess
        let suspend_proc = GetProcAddress(module, windows::core::s!("NtSuspendProcess"))
            .ok_or(SuspendError::NotAvailable)?;

        // Resolve NtResumeProcess
        let resume_proc = GetProcAddress(module, windows::core::s!("NtResumeProcess"))
            .ok_or(SuspendError::NotAvailable)?;

        let nt_suspend: NtSuspendProcessFn = std::mem::transmute(suspend_proc);
        let nt_resume: NtResumeProcessFn = std::mem::transmute(resume_proc);

        Ok(SuspenderContext {
            nt_suspend,
            nt_resume,
        })
    }
}

// ---------------------------------------------------------------------------
// hard_suspend
// ---------------------------------------------------------------------------

/// Hard-suspend a process using `NtSuspendProcess`.
///
/// On success, the PID is added to `AppState.suspended_set` and
/// `AppState.process_map[pid].is_suspended` is set to `true`.
pub fn hard_suspend(
    ctx: &SuspenderContext,
    pid: u32,
    state: SharedState,
) -> Result<(), SuspendError> {
    unsafe {
        // Open with PROCESS_SUSPEND_RESUME rights
        let handle = OpenProcess(PROCESS_SUSPEND_RESUME, false, pid)
            .map_err(|_| SuspendError::AccessDenied(pid))?;

        // Call NtSuspendProcess — NTSTATUS 0 = success, negative = error
        let ntstatus = (ctx.nt_suspend)(handle);
        let _ = CloseHandle(handle);

        if ntstatus < 0 {
            return Err(SuspendError::SuspendFailed(pid, ntstatus));
        }
    }

    // Update shared state
    {
        let mut st = state.lock();
        st.suspended_set.insert(pid);
        if let Some(entry) = st.process_map.get_mut(&pid) {
            entry.is_suspended = true;
        }
        let pids = st.suspended_set.clone();
        drop(st);
        // Persist outside the lock
        if let Err(e) = save_suspended_set(&pids) {
            log::warn!("suspender: failed to save suspended_set: {e}");
        }
    }

    log::info!("suspender: hard-suspended PID {pid}");
    Ok(())
}

// ---------------------------------------------------------------------------
// hard_resume
// ---------------------------------------------------------------------------

/// Resume a previously hard-suspended process.
///
/// Returns `Ok(())` immediately if the PID is not in `suspended_set` (idempotent).
/// If the process has already died, cleans up state and returns `Ok(())`.
pub fn hard_resume(
    ctx: &SuspenderContext,
    pid: u32,
    state: SharedState,
) -> Result<(), SuspendError> {
    // Check if this PID is actually suspended — if not, nothing to do
    {
        let st = state.lock();
        if !st.suspended_set.contains(&pid) {
            return Ok(());
        }
    }

    // Check if the process is still alive
    let process_alive = unsafe {
        OpenProcess(PROCESS_QUERY_INFORMATION, false, pid).is_ok()
    };

    if !process_alive {
        // Process already dead — clean up the bookkeeping and return
        let mut st = state.lock();
        st.suspended_set.remove(&pid);
        if let Some(entry) = st.process_map.get_mut(&pid) {
            entry.is_suspended = false;
        }
        let pids = st.suspended_set.clone();
        drop(st);
        let _ = save_suspended_set(&pids);
        log::info!("suspender: PID {pid} was dead on resume — cleaned up");
        return Ok(());
    }

    // Resume with NtResumeProcess
    unsafe {
        let handle = OpenProcess(PROCESS_SUSPEND_RESUME, false, pid)
            .map_err(|_| SuspendError::AccessDenied(pid))?;

        let ntstatus = (ctx.nt_resume)(handle);
        let _ = CloseHandle(handle);

        if ntstatus < 0 {
            return Err(SuspendError::ResumeFailed(pid, ntstatus));
        }
    }

    // Update shared state
    {
        let mut st = state.lock();
        st.suspended_set.remove(&pid);
        if let Some(entry) = st.process_map.get_mut(&pid) {
            entry.is_suspended = false;
        }
        let pids = st.suspended_set.clone();
        drop(st);
        if let Err(e) = save_suspended_set(&pids) {
            log::warn!("suspender: failed to save suspended_set: {e}");
        }
    }

    log::info!("suspender: hard-resumed PID {pid}");
    Ok(())
}

// ---------------------------------------------------------------------------
// resume_all_suspended — called on Gaming Mode exit and app quit
// ---------------------------------------------------------------------------

/// Resume every PID currently in `AppState.suspended_set`.
///
/// Logs failures but always continues — never aborts on a single error.
/// Used on Gaming Mode exit, app quit, and startup cleanup.
pub fn resume_all_suspended(ctx: &SuspenderContext, state: SharedState) {
    // Clone the set first — avoid holding the lock during resume calls
    let pids: Vec<u32> = {
        let st = state.lock();
        st.suspended_set.iter().copied().collect()
    };

    for pid in pids {
        if let Err(e) = hard_resume(ctx, pid, state.clone()) {
            log::warn!("suspender: resume_all — failed to resume PID {pid}: {e}");
        }
    }

    log::info!("suspender: resume_all_suspended complete");
}

// ---------------------------------------------------------------------------
// Foreground change hook — auto-resume when user clicks a suspended app
// ---------------------------------------------------------------------------

/// Spawn a dedicated thread that installs a `WinEventHook` listening for
/// `EVENT_SYSTEM_FOREGROUND` changes.  When the user brings a suspended
/// process's window to the foreground, Gingify automatically resumes it and
/// emits the `gingify://process-resumed` event to the frontend.
///
/// The hook thread runs a Win32 message pump for the lifetime of the app.
/// All callback logic is wrapped in `std::panic::catch_unwind` so a panic
/// cannot bring down the whole application.
pub fn start_foreground_hook(state: SharedState, app_handle: tauri::AppHandle) {
    std::thread::spawn(move || {
        // Clone SuspenderContext out of AppState so the callback owns it
        // We borrow a cloned Arc for the callback closure below.
        let state_clone = state.clone();
        let handle_clone = app_handle.clone();

        // We need to pass state into the WinEvent callback.  Because
        // SetWinEventHook requires a plain extern fn (no closures), we use a
        // thread-local to communicate.
        //
        // SAFETY: the hook is only ever invoked on this thread (WINEVENT_OUTOFCONTEXT
        // means the callback runs in-process on the thread that pumps messages).
        HOOK_STATE.with(|cell| {
            // Store the shared state and app handle in thread-locals so the
            // static callback can access them.
            *cell.state.borrow_mut() = Some(state_clone);
            *cell.app_handle.borrow_mut() = Some(handle_clone);
        });

        unsafe {
            // Register the foreground-change hook (0, 0 = all processes/threads)
            let _hook: HWINEVENTHOOK = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_hook_callback),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );

            // Message pump — required for WinEvent hooks on this thread
            let mut msg = MSG::default();
            loop {
                let result = GetMessageW(&mut msg, None, 0, 0);
                // GetMessage returns 0 for WM_QUIT, -1 for error
                if result.0 == 0 || result.0 == -1 {
                    break;
                }
                // We intentionally skip TranslateMessage / DispatchMessage —
                // WinEvent hooks are delivered directly to the callback.
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Thread-local storage for the WinEvent callback
// ---------------------------------------------------------------------------

use std::cell::RefCell;

struct HookThreadLocals {
    state: RefCell<Option<SharedState>>,
    app_handle: RefCell<Option<tauri::AppHandle>>,
}

// SAFETY: HOOK_STATE is only accessed on the hook thread.
unsafe impl Sync for HookThreadLocals {}

thread_local! {
    static HOOK_STATE: HookThreadLocals = const { HookThreadLocals {
        state: RefCell::new(None),
        app_handle: RefCell::new(None),
    } };
}

// ---------------------------------------------------------------------------
// WinEvent callback — must be a plain extern "system" fn
// ---------------------------------------------------------------------------

unsafe extern "system" fn foreground_hook_callback(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: windows::Win32::Foundation::HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _event_time: u32,
) {
    // Wrap all logic in catch_unwind so a panic cannot kill the hook thread
    let _ = std::panic::catch_unwind(|| {
        // Get the PID of the window that just became foreground
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        if pid == 0 {
            return;
        }

        HOOK_STATE.with(|cell| {
            let maybe_state = cell.state.borrow();
            let maybe_handle = cell.app_handle.borrow();

            let (Some(state), Some(app_handle)) =
                (maybe_state.as_ref(), maybe_handle.as_ref())
            else {
                return;
            };

            // Check if the newly focused PID is in the suspended set
            let is_suspended = {
                let st = state.lock();
                st.suspended_set.contains(&pid)
            };

            if !is_suspended {
                return;
            }

            // We need the SuspenderContext to resume — load a fresh context on
            // the callback thread (ntdll is always resident, this is cheap).
            match load_suspender() {
                Ok(ctx) => {
                    match hard_resume(&ctx, pid, state.clone()) {
                        Ok(()) => {
                            log::info!("suspender: auto-resumed PID {pid} (foreground change)");
                            // Emit Tauri event so the frontend can update
                            if let Err(e) = app_handle.emit("gingify://process-resumed", pid) {
                                log::warn!("suspender: failed to emit process-resumed event: {e}");
                            }
                        }
                        Err(e) => {
                            log::warn!("suspender: auto-resume failed for PID {pid}: {e}");
                        }
                    }
                }
                Err(e) => {
                    log::error!("suspender: foreground hook — load_suspender failed: {e}");
                }
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Persistence — suspended_pids.json in %APPDATA%\Gingify\
// ---------------------------------------------------------------------------

/// Returns the path to `%APPDATA%\Gingify\suspended_pids.json`.
fn suspended_pids_path() -> Result<PathBuf> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| anyhow::anyhow!("APPDATA environment variable is not set"))?;
    Ok(PathBuf::from(appdata)
        .join("Gingify")
        .join("suspended_pids.json"))
}

/// Persist the current `suspended_set` to disk.
///
/// Called whenever `suspended_set` changes so that the set survives a
/// force-kill of Gingify (startup cleanup will resume orphaned PIDs).
pub fn save_suspended_set(pids: &HashSet<u32>) -> Result<()> {
    let path = suspended_pids_path()?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let list: Vec<u32> = pids.iter().copied().collect();
    let json = serde_json::to_string_pretty(&list)?;
    std::fs::write(&path, json)?;

    log::debug!(
        "suspender: suspended_pids saved to {:?} ({} PIDs)",
        path,
        pids.len()
    );
    Ok(())
}

/// Load previously persisted PIDs from `suspended_pids.json`.
///
/// Returns an empty `HashSet` on any error (missing file = fresh start,
/// corrupt file = safe reset).
pub fn load_suspended_set() -> HashSet<u32> {
    let path = match suspended_pids_path() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("suspender: cannot resolve suspended_pids path: {e}");
            return HashSet::new();
        }
    };

    if !path.exists() {
        return HashSet::new();
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("suspender: failed to read suspended_pids file: {e}");
            return HashSet::new();
        }
    };

    match serde_json::from_str::<Vec<u32>>(&raw) {
        Ok(list) => {
            log::info!(
                "suspender: loaded {} orphaned suspended PIDs from {:?}",
                list.len(),
                path
            );
            list.into_iter().collect()
        }
        Err(e) => {
            log::warn!("suspender: suspended_pids file corrupt ({e}) — resetting");
            HashSet::new()
        }
    }
}
