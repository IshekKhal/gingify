//! Background process polling loop — queries all running PIDs every 5 seconds
//! using Windows APIs and writes results into the shared AppState.
//!
//! Called once from `lib.rs`:
//! ```ignore
//! let monitor_state = Arc::clone(&state);
//! let monitor_handle = app_handle.clone();
//! std::thread::spawn(move || core::monitor::start_monitor(monitor_state, monitor_handle));
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri::Emitter;

use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::ProcessStatus::{
    EnumProcesses, GetProcessImageFileNameW, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX2,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL, IsWindowVisible, GetWindowThreadProcessId, GetDesktopWindow, GetWindow, GW_CHILD, GW_HWNDNEXT, GW_OWNER, GetWindowLongW, GetWindowTextLengthW, GWL_EXSTYLE, WS_EX_TOOLWINDOW};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::core::{PCWSTR, PWSTR};

use crate::state::app_state::{PressureLevel, ProcessEntry, RamStats, SharedState, TrimTrigger};

// ---------------------------------------------------------------------------
// Browser process names — excluded from trimming/snoozing (managed by extensions)
// ---------------------------------------------------------------------------

const BROWSER_NAMES: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "arc.exe",
];

fn is_browser(name: &str) -> bool {
    let lower = name.to_lowercase();
    BROWSER_NAMES.iter().any(|b| lower == *b)
}

// ---------------------------------------------------------------------------
// Protected process names — never trim, always mark is_protected = true
// ---------------------------------------------------------------------------

const PROTECTED_NAMES: &[&str] = &[
    "explorer.exe",
    "lsass.exe",
    "winlogon.exe",
    "csrss.exe",
    "svchost.exe",
    "dwm.exe",
    "wininit.exe",
    "services.exe",
    "smss.exe",
    "registry",
    "system",
    "secure system",
    "gingify.exe",
    // Internal language-server / extension processes spawned by IDEs & Electron apps.
    // Their paths are under user AppData, not Windows system dirs, so the path
    // heuristic alone cannot catch them.
    "language_server_windows_x64.exe",
];

fn is_protected(name: &str) -> bool {
    let lower = name.to_lowercase();
    PROTECTED_NAMES.iter().any(|p| lower == *p)
}

/// Returns true for processes whose exe lives under Windows system directories.
/// Catches things like SearchApp.exe (WindowsApps), msedgewebview2.exe, etc.
fn is_system_path(win32_path: &str) -> bool {
    let lower = win32_path.to_lowercase();
    lower.starts_with(r"c:\windows\")
        || lower.starts_with(r"c:\program files\windowsapps\")
        || lower.contains(r"\edgewebview\")
        || lower.contains(r"\edgewebview2\")
}

// ---------------------------------------------------------------------------
// Process icon extraction (cached by exe name)
// ---------------------------------------------------------------------------

static ICON_CACHE: OnceLock<parking_lot::Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

fn icon_cache() -> &'static parking_lot::Mutex<HashMap<String, Option<String>>> {
    ICON_CACHE.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// Returns the cached icon data URL for `name`, extracting it from `win32_path`
/// on first call. Returns `None` if extraction fails or path is unavailable.
fn get_cached_icon(name: &str, win32_path: Option<&str>) -> Option<String> {
    {
        let lock = icon_cache().lock();
        if let Some(cached) = lock.get(name) {
            return cached.clone();
        }
    }
    let url = win32_path.and_then(extract_icon_b64);
    icon_cache().lock().insert(name.to_string(), url.clone());
    url
}

/// Extract the 16×16 icon from `win32_path` and return it as a PNG data URL.
fn extract_icon_b64(win32_path: &str) -> Option<String> {
    const SIZE: i32 = 16;
    let wide: Vec<u16> = win32_path.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        // 1. Get small icon handle via shell
        let mut shfi = SHFILEINFOW::default();
        let ret = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut shfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        );
        if ret == 0 || shfi.hIcon.is_invalid() {
            return None;
        }

        // 2. Create an offscreen 32-bit top-down DIB to render the icon into
        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            let _ = DestroyIcon(shfi.hIcon);
            return None;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIZE,
                biHeight: -SIZE, // negative = top-down scan lines
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: Default::default(),
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbm = match CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) => b,
            Err(_) => {
                let _ = DeleteDC(hdc);
                let _ = DestroyIcon(shfi.hIcon);
                return None;
            }
        };

        // Guard: CreateDIBSection succeeded but bits pointer is still null (shouldn't
        // happen, but is UB to dereference if it does).
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(hbm.0));
            let _ = DeleteDC(hdc);
            let _ = DestroyIcon(shfi.hIcon);
            return None;
        }

        let old_obj = SelectObject(hdc, HGDIOBJ(hbm.0));

        // 3. Draw icon — DrawIconEx writes BGRA with alpha into the 32-bit DIB
        let _ = DrawIconEx(hdc, 0, 0, shfi.hIcon, SIZE, SIZE, 0, None, DI_NORMAL);

        // 4. Read raw BGRA pixels
        let px_count = (SIZE * SIZE) as usize;
        let raw = std::slice::from_raw_parts(bits as *const u8, px_count * 4);

        // 5. Convert BGRA → RGBA; if all alpha bytes are 0 (non-alpha icon), force opaque
        let max_alpha = raw.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0);
        let mut rgba = Vec::with_capacity(px_count * 4);
        for px in raw.chunks_exact(4) {
            rgba.push(px[2]); // R
            rgba.push(px[1]); // G
            rgba.push(px[0]); // B
            rgba.push(if max_alpha == 0 { 255 } else { px[3] }); // A
        }

        // 6. GDI cleanup
        SelectObject(hdc, old_obj);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        let _ = DeleteDC(hdc);
        let _ = DestroyIcon(shfi.hIcon);

        // 7. Encode as PNG
        let mut png_bytes: Vec<u8> = Vec::new();
        {
            let mut enc = png::Encoder::new(
                std::io::Cursor::new(&mut png_bytes),
                SIZE as u32,
                SIZE as u32,
            );
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().ok()?;
            writer.write_image_data(&rgba).ok()?;
        }

        Some(format!("data:image/png;base64,{}", base64_encode(&png_bytes)))
    }
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b0 = c[0] as usize;
        let b1 = c.get(1).copied().unwrap_or(0) as usize;
        let b2 = c.get(2).copied().unwrap_or(0) as usize;
        out.push(T[b0 >> 2] as char);
        out.push(T[((b0 & 3) << 4) | (b1 >> 4)] as char);
        out.push(if c.len() > 1 { T[((b1 & 0xf) << 2) | (b2 >> 6)] as char } else { '=' });
        out.push(if c.len() > 2 { T[b2 & 0x3f] as char } else { '=' });
    }
    out
}

// ---------------------------------------------------------------------------
// FILETIME helpers
// ---------------------------------------------------------------------------

/// Convert a FILETIME (two u32 halves) to a single u64 of 100-ns intervals.
fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

// ---------------------------------------------------------------------------
// CPU usage helpers
// ---------------------------------------------------------------------------

/// Compute CPU usage percentage over one 5-second poll interval.
///
/// `delta_time` is the elapsed wall-clock ticks (100-ns units) since last poll.
/// `delta_cpu`  is the sum of kernel+user CPU ticks consumed by the process.
///
/// Returns a value in [0.0, 100.0] per logical CPU (un-normalised by core count
/// intentionally — this matches what Task Manager shows per-process).
fn compute_cpu_pct(delta_cpu: u64, delta_time: u64) -> f32 {
    if delta_time == 0 {
        return 0.0;
    }
    ((delta_cpu as f64 / delta_time as f64) * 100.0).min(100.0) as f32
}

// ---------------------------------------------------------------------------
// Collect PIDs that own at least one visible window (no-callback safe version)
// ---------------------------------------------------------------------------

fn collect_windowed_pids() -> HashSet<u32> {
    let mut pids = HashSet::new();
    unsafe {
        let desktop = GetDesktopWindow();
        let first = match GetWindow(desktop, GW_CHILD) {
            Ok(h) => h,
            Err(_) => return pids,
        };
        let mut hwnd = first;
        let mut guard = 0u32;
        loop {
            if IsWindowVisible(hwnd).as_bool() {
                // Match Task Manager's "Apps" heuristic: top-level, titled,
                // non-tool taskbar windows only. Filters out overlays, tray
                // hosts, dialogs, and invisible container windows.
                let has_owner = GetWindow(hwnd, GW_OWNER).is_ok();
                let has_title = GetWindowTextLengthW(hwnd) > 0;
                let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                let is_tool = (ex_style & WS_EX_TOOLWINDOW.0) != 0;

                if !has_owner && has_title && !is_tool {
                    let mut pid = 0u32;
                    let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    if pid != 0 {
                        pids.insert(pid);
                    }
                }
            }
            hwnd = match GetWindow(hwnd, GW_HWNDNEXT) {
                Ok(next) => next,
                Err(_) => break,
            };
            // Safety: stop if we've looped more than 10k windows (shouldn't happen)
            guard += 1;
            if guard > 10_000 { break; }
        }
    }
    pids
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Spawn the blocking monitor loop.
/// Must be called from a dedicated OS thread (NOT a Tokio task) because all
/// Windows API calls here are synchronous / blocking.
pub fn start_monitor(state: SharedState, app_handle: AppHandle) {
    // 100-ns ticks per second  ─  used for CPU delta denominator.
    const TICKS_PER_SEC: u64 = 10_000_000;
    const POLL_SECS: u64 = 1;
    const POLL_TICKS: u64 = POLL_SECS * TICKS_PER_SEC;

    log::info!("monitor: poll loop starting (interval = {POLL_SECS}s)");

    loop {
        poll_cycle(&state, &app_handle, POLL_TICKS);
        thread::sleep(Duration::from_secs(POLL_SECS));
    }
}

// ---------------------------------------------------------------------------
// Single poll cycle
// ---------------------------------------------------------------------------

fn poll_cycle(state: &SharedState, app_handle: &AppHandle, poll_ticks: u64) {
    // --- 1. Enumerate all PIDs -----------------------------------------------
    let pids = match enum_processes() {
        Ok(p) => p,
        Err(e) => {
            log::error!("monitor: EnumProcesses failed: {e:?}");
            return;
        }
    };

    let mut seen_pids = HashSet::<u32>::new();

    // --- 1b. Enumerate visible windows to detect windowed (App) vs background processes ----
    let pids_with_windows = collect_windowed_pids();

    // --- 2. Collect PID → exclusion map (take a snapshot, no lock held during API calls) ---
    let excluded_set: HashSet<String> = {
        let st = state.lock();
        st.config
            .excluded_processes
            .iter()
            .map(|s| s.to_lowercase())
            .collect()
    };

    // --- 3. Process each PID -------------------------------------------------
    for pid in &pids {
        let pid = *pid;

        // Skip system idle process (PID 0) and System (PID 4) and anything < 8
        if pid < 8 {
            continue;
        }

        // Open process handle — skip silently on access denied / already dead
        let handle = unsafe {
            match OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
                false,
                pid,
            ) {
                Ok(h) => h,
                Err(_) => continue,
            }
        };

        // --- a. Process name + Win32 path ------------------------------------
        let (name, win32_path) = {
            let mut buf = vec![0u16; 1024];
            let n = unsafe { GetProcessImageFileNameW(handle, &mut buf) } as usize;
            if n == 0 {
                unsafe { let _ = CloseHandle(handle); }
                continue;
            }
            let full = String::from_utf16_lossy(&buf[..n]);
            let name = full.rsplit('\\').next().unwrap_or(&full).to_string();

            // Win32 path (C:\...) needed for system-path detection and icon extraction
            let win32_path = {
                let mut w = vec![0u16; 32768];
                let mut sz = w.len() as u32;
                if unsafe {
                    QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(w.as_mut_ptr()), &mut sz)
                }.is_ok() {
                    Some(String::from_utf16_lossy(&w[..sz as usize]))
                } else {
                    None
                }
            };

            (name, win32_path)
        };

        // --- b. Private Working Set (RAM) — matches Task Manager's "Memory" column
        let ram_mb: f32 = {
            let mut mc = PROCESS_MEMORY_COUNTERS_EX2 {
                cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
                ..Default::default()
            };
            let ok = unsafe {
                GetProcessMemoryInfo(
                    handle,
                    &mut mc as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                    mc.cb,
                )
            };
            if ok.is_err() {
                unsafe { let _ = CloseHandle(handle); }
                continue;
            }
            mc.PrivateWorkingSetSize as f32 / (1024.0 * 1024.0)
        };

        // --- c. CPU usage (kernel + user time delta) -------------------------
        let (kernel_now, user_now): (u64, u64) = {
            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            let ok = unsafe {
                GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
            };
            if ok.is_err() {
                // Non-fatal — just use 0
                (0, 0)
            } else {
                (filetime_to_u64(kernel), filetime_to_u64(user))
            }
        };

        // Close handle — done with Windows API calls for this PID
        unsafe { let _ = CloseHandle(handle); }

        // --- d. Build / update ProcessEntry in state -------------------------
        // Combine name-list protection with path-based heuristic (catches
        // SearchApp.exe, msedgewebview2.exe, language servers, etc.)
        let protected = is_protected(&name)
            || win32_path.as_deref().is_some_and(is_system_path);
        let excluded = excluded_set.contains(&name.to_lowercase()) || is_browser(&name);

        // Icon extraction is cached by name; happens outside the state lock
        // so the ~10 ms SHGetFileInfoW call doesn't block other threads.
        let icon_url = get_cached_icon(&name, win32_path.as_deref());

        {
            let mut st = state.lock();
            let is_suspended = st.suspended_set.contains(&pid);

            let cpu_pct = if let Some(existing) = st.process_map.get(&pid) {
                // Delta calculation
                let delta_kernel = kernel_now.saturating_sub(existing.prev_kernel_time);
                let delta_user   = user_now.saturating_sub(existing.prev_user_time);
                compute_cpu_pct(delta_kernel + delta_user, poll_ticks)
            } else {
                0.0
            };

            let entry = st.process_map.entry(pid).or_insert_with(|| ProcessEntry {
                pid,
                name: name.clone(),
                ram_mb,
                cpu_usage_pct: 0.0,
                idle_seconds: 0,
                is_suspended,
                is_protected: protected,
                is_excluded: excluded,
                has_window: false,
                icon_data_url: icon_url.clone(),
                last_active_at: Some(Instant::now()),
                prev_kernel_time: kernel_now,
                prev_user_time: user_now,
            });

            entry.name = name;
            entry.ram_mb = ram_mb;
            entry.cpu_usage_pct = cpu_pct;
            entry.is_suspended = is_suspended;
            entry.is_protected = protected;
            entry.is_excluded = excluded;
            entry.has_window = pids_with_windows.contains(&pid);
            entry.prev_kernel_time = kernel_now;
            entry.prev_user_time = user_now;
            // Only write the icon once — skip re-assigning on every poll cycle
            if entry.icon_data_url.is_none() {
                entry.icon_data_url = icon_url;
            }
        }

        seen_pids.insert(pid);
    }

    // --- 4. Remove stale PIDs (processes that exited since last poll) ---------
    {
        let mut st = state.lock();
        st.process_map.retain(|pid, _| seen_pids.contains(pid));
    }

    // --- 5. Update idle times via profiler -----------------------------------
    crate::core::profiler::update_idle_times(state);

    // --- 5b. Scan for bloat services and update AppState.bloat_list ----------
    {
        let bloat_list = {
            let st = state.lock();
            crate::core::bloat_scan::scan_bloat(&st)
        };
        let mut st = state.lock();
        st.bloat_list = bloat_list;
    }

    // --- 6. Update system RAM stats ------------------------------------------
    update_ram_stats(state);

    // --- 7. Auto-trim if threshold exceeded (60-second cooldown) -------------
    let should_auto_trim = {
        let st = state.lock();
        let enabled = st.config.auto_trim_enabled;
        let pressure = st.ram_stats.pressure_pct;
        let threshold = st.config.auto_trim_threshold_pct as f32;
        let cooldown_ok = st
            .last_auto_trim_at
            .map(|t| t.elapsed() >= Duration::from_secs(60))
            .unwrap_or(true);
        enabled && pressure >= threshold && cooldown_ok
    };

    if should_auto_trim {
        // Record cooldown timestamp before releasing lock
        {
            let mut st = state.lock();
            st.last_auto_trim_at = Some(Instant::now());
        }

        let idle_threshold = {
            let st = state.lock();
            use crate::state::app_state::Profile;
            match &st.active_profile {
                Profile::Work | Profile::Gaming | Profile::Focus =>
                    crate::core::profiles::resolve_profile(&st.active_profile).idle_threshold_secs,
                _ => st.config.idle_threshold_secs,
            }
        };

        let result =
            crate::core::trimmer::soft_trim_all(state, idle_threshold, TrimTrigger::Auto);

        // Emit gingify://trim-fired with the TrimResult
        if let Err(e) = app_handle.emit("gingify://trim-fired", &result) {
            log::warn!("monitor: failed to emit trim-fired: {e}");
        }

        // Stub notification call — implemented in Prompt 08
        crate::core::notifications::notify_trim_result(&result);
    }

    // --- 8. Notify user when RAM pressure is High or Critical ---------------
    // Replaces the old tray-icon color indicator with toast notifications
    // (rate-limited to once per hour inside notify_high_ram).
    {
        let (should_notify, pct, is_critical) = {
            let st = state.lock();
            let notify = st.config.notifications_enabled;
            let pct = st.ram_stats.pressure_pct;
            let is_critical = matches!(st.ram_stats.pressure_level, PressureLevel::Critical);
            let warn = matches!(
                st.ram_stats.pressure_level,
                PressureLevel::Critical | PressureLevel::High
            );
            (notify && warn, pct, is_critical)
        };
        if should_notify {
            crate::core::notifications::notify_high_ram(pct, is_critical);
        }
    }

    // --- 9. Emit gingify://ram-update to all WebView windows ----------------
    let ram_stats = {
        let st = state.lock();
        st.ram_stats.clone()
    };
    if let Err(e) = app_handle.emit("gingify://ram-update", &ram_stats) {
        log::warn!("monitor: failed to emit ram-update: {e}");
    }
}

// ---------------------------------------------------------------------------
// PID enumeration with dynamic buffer growth
// ---------------------------------------------------------------------------

/// Calls `EnumProcesses` with a buffer that doubles until it's large enough
/// to hold all PIDs (handles systems with many processes).
fn enum_processes() -> windows::core::Result<Vec<u32>> {
    let mut buf: Vec<u32> = vec![0u32; 1024];
    loop {
        let mut bytes_returned: u32 = 0;
        unsafe {
            EnumProcesses(buf.as_mut_ptr(), (buf.len() * 4) as u32, &mut bytes_returned)?;
        }
        let count = (bytes_returned / 4) as usize;
        if count < buf.len() {
            buf.truncate(count);
            return Ok(buf);
        }
        // Buffer was too small — double and retry
        let new_len = buf.len() * 2;
        buf.resize(new_len, 0);
    }
}

// ---------------------------------------------------------------------------
// RAM stats refresh
// ---------------------------------------------------------------------------

/// Query `GlobalMemoryStatusEx` and write results into `AppState.ram_stats`.
pub fn update_ram_stats(state: &SharedState) {
    let mut mem_status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };

    let ok = unsafe { GlobalMemoryStatusEx(&mut mem_status) };
    if ok.is_err() {
        log::warn!("monitor: GlobalMemoryStatusEx failed");
        return;
    }

    const MB: u64 = 1024 * 1024;
    let total_mb = mem_status.ullTotalPhys / MB;
    let available_mb = mem_status.ullAvailPhys / MB;
    let used_mb = total_mb.saturating_sub(available_mb);
    let pressure_pct = mem_status.dwMemoryLoad as f32; // 0–100 from Windows

    let ram_stats = RamStats {
        total_mb,
        used_mb,
        available_mb,
        pressure_pct,
        pressure_level: PressureLevel::from_pct(pressure_pct),
    };

    let mut st = state.lock();
    st.ram_stats = ram_stats;
}
