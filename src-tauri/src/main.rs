//! Gingify binary entry point — delegates to lib.rs `run()`.
//!
//! Duplicate-instance prevention lives here (not in lib.rs) so that
//! the Windows HANDLE returned by CreateMutexW stays alive for the
//! entire process lifetime and isn't dropped early.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

fn main() {
    // -------------------------------------------------------------------------
    // Duplicate instance prevention — Task 3 (PROMPT_DESKTOP_10)
    //
    // Attempt to create a named kernel mutex. If it already exists the app is
    // already running; we exit silently so only one instance ever runs.
    //
    // The HANDLE is stored in `_mutex` so it stays alive (and the OS keeps the
    // mutex locked) for the entire process lifetime. If we stored it inside
    // run() it would be dropped immediately, allowing a second instance.
    // -------------------------------------------------------------------------
    let mutex_name: Vec<u16> = "Global\\GingifyRunning\0"
        .encode_utf16()
        .collect();

    // SAFETY: mutex_name is a valid null-terminated UTF-16 string.
    let _mutex_result = unsafe {
        CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr()))
    };

    // CreateMutexW returns Ok(HANDLE) even if the mutex already existed;
    // the ERROR_ALREADY_EXISTS condition is signalled via GetLastError.
    // The windows crate exposes this via windows::Win32::Foundation::GetLastError.
    let already_exists = unsafe {
        windows::Win32::Foundation::GetLastError() == ERROR_ALREADY_EXISTS
    };

    if already_exists {
        // Gingify is already running.  Signal it via a named Windows event so
        // the running instance shows its detail window.  FindWindowW by title is
        // unreliable (multiple "Gingify" windows; some hidden).
        unsafe {
            use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
            use windows::Win32::Foundation::CloseHandle;
            let event_name: Vec<u16> = "Local\\GingifyActivate\0".encode_utf16().collect();
            if let Ok(h) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_name.as_ptr())) {
                let _ = SetEvent(h);
                let _ = CloseHandle(h);
            }
        }
        std::process::exit(0);
    }

    // Keep _mutex_result alive for the whole process (mutex stays locked)
    let _mutex_handle = _mutex_result.ok();

    gingify_lib::run();
}
