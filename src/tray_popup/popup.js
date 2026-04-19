const { invoke } = window.__TAURI__.core;
const { listen, emit } = window.__TAURI__.event;

// ── Disable browser context menu and reload shortcuts ─────────────────────
// The WebView's native context menu (Back / Refresh / Save as / Print) must
// never appear: Refresh navigates the WebView away from the Tauri content,
// leaving a blank black window.

document.addEventListener('contextmenu', e => e.preventDefault());
document.addEventListener('keydown', e => {
  if (e.key === 'F5' || (e.ctrlKey && (e.key === 'r' || e.key === 'R'))) {
    e.preventDefault();
  }
});

// ── DOM references ────────────────────────────────────────────────────────

const ramStatusDot   = document.getElementById('ram-status-dot');
const ramStatusText  = document.getElementById('ram-status-text');
const ramBarFill     = document.getElementById('ram-bar-fill');
const ramUsedText    = document.getElementById('ram-used-text');
const ramTotalText   = document.getElementById('ram-total-text');

const btnFreeNow     = document.getElementById('btn-free-now');
const btnGamingMode  = document.getElementById('btn-gaming-mode');
const trimBanner     = document.getElementById('trim-result-banner');

const processList    = document.getElementById('process-list');
const seeAllLink     = document.getElementById('link-see-all');

const profileSelect  = document.getElementById('profile-select');
const lastFreedText  = document.getElementById('last-freed-text');
const btnClose       = document.getElementById('btn-close');

// ── State ─────────────────────────────────────────────────────────────────

let currentProfile    = 'Work';
let hardSuspendEnabled = false;
let trimBannerTimer   = null;
let _ipcBusy          = false;   // true while an IPC call is in flight → block blur-hide
let _isAdmin          = false;   // cached result of is_admin()
let _lastFocusTime    = 0;       // timestamp of last focus event (blur race guard)

// ── Initialise on load ────────────────────────────────────────────────────

// is_admin() with 3s timeout — if the IPC hangs (e.g. Win7 edge case), we
// fall back to non-admin mode rather than blocking popup init forever.
async function checkAdminWithTimeout() {
  try {
    return await Promise.race([
      invoke('is_admin'),
      new Promise((_, rej) => setTimeout(() => rej(new Error('is_admin timeout')), 3000)),
    ]);
  } catch (e) {
    console.error('[gingify] is_admin failed:', e);
    return false;
  }
}

window.addEventListener('DOMContentLoaded', async () => {
  // Load admin status first so Gaming Mode guard is ready before user can click
  _isAdmin = await checkAdminWithTimeout();
  await Promise.all([
    loadRamStats(),
    loadProcessList(),
    loadCurrentProfile(),
    loadTrimHistory(),
    loadConfig(),
  ]);
});

// ── Refresh data whenever popup is brought into focus ─────────────────────
// DOMContentLoaded fires once at window creation. On every subsequent open
// (tray left-click → show + focus), we refresh so data is never stale.

window.addEventListener('focus', async () => {
  _lastFocusTime = Date.now();
  document.body.classList.add('popup-visible');
  await Promise.all([
    loadRamStats(),
    loadProcessList(),
    loadCurrentProfile(),
    loadTrimHistory(),
    loadConfig(),
  ]);
});

// ── Close on focus loss ───────────────────────────────────────────────────

window.addEventListener('blur', () => {
  document.body.classList.remove('popup-visible');
  // Don't auto-close while an IPC call is in progress (e.g. Gaming Mode toggle
  // may briefly shift focus to a UAC dialog or another window).
  if (_ipcBusy) return;
  // Guard against Windows focus-steal race: tray click causes blur to fire
  // immediately after show+focus, before user can interact with the popup.
  if (Date.now() - _lastFocusTime < 300) return;
  // Snapshot the time of this blur so the timer can tell if focus came back
  // before it fires (e.g. tray left-click: blur → Rust set_focus → focus → timer).
  const blurAt = Date.now();
  setTimeout(() => {
    if (_lastFocusTime > blurAt) return; // focus returned since this blur — stay open
    if (_ipcBusy) return;
    invoke('hide_window', { label: 'popup' }).catch(e => console.error(e));
  }, 150);
});

// ── Close button ──────────────────────────────────────────────────────────

btnClose.addEventListener('click', () => {
  invoke('hide_window', { label: 'popup' }).catch(e => console.error(e));
});

// ── Real-time event listeners ─────────────────────────────────────────────

listen('gingify://ram-update', (event) => {
  updateRamBar(event.payload);
  // Synchronise processes and history auto-updates with the backend 5s monitor heartbeat
  loadProcessList().catch(() => {});
  loadTrimHistory().catch(() => {});
});

listen('gingify://trim-fired', (event) => {
  showTrimResult(event.payload);
  loadProcessList().catch(() => {});
  loadTrimHistory().catch(() => {});
});

// Theme: reapply whenever config changes (e.g. user switches theme in Settings)
listen('gingify://config-changed', (event) => {
  if (event.payload?.theme) applyTheme(event.payload.theme);
});

// ── Load functions ────────────────────────────────────────────────────────

async function loadConfig() {
  try {
    const cfg = await invoke('get_config');
    hardSuspendEnabled = cfg.hard_suspend_enabled ?? false;
    // Apply persisted theme on every popup open
    applyTheme(cfg.theme ?? 'system');
  } catch (e) {
    console.warn('loadConfig failed:', e);
  }
}

async function loadRamStats() {
  try {
    let stats = null;
    for (let attempt = 0; attempt < 6; attempt++) {
      stats = await invoke('get_ram_stats');
      if (stats.total_mb > 0) break;
      await new Promise(r => setTimeout(r, 1000));
    }
    updateRamBar(stats);
  } catch (e) {
    ramStatusText.textContent = 'RAM unavailable';
    console.warn('loadRamStats failed:', e);
  }
}

async function loadProcessList() {
  try {
    // Retry loop — process_map is empty until the monitor completes its first
    // poll cycle (can take up to ~5 s on startup).  Without retrying the popup
    // shows "No processes found" until the next 5-second gingify://ram-update.
    let processes = [];
    for (let attempt = 0; attempt < 4; attempt++) {
      processes = await invoke('get_process_list');
      if (processes && processes.length > 0) break;
      if (attempt < 3) await new Promise(r => setTimeout(r, 1500));
    }
    renderProcessList(processes || []);
  } catch (e) {
    processList.innerHTML = '<p class="process-loading">Process list unavailable</p>';
    console.warn('loadProcessList failed:', e);
  }
}

async function loadCurrentProfile() {
  try {
    const profile = await invoke('get_current_profile');
    currentProfile = profile;
    profileSelect.value = profile;
    updateGamingModeButton(profile);
  } catch (e) {
    console.warn('loadCurrentProfile failed:', e);
  }
}

async function loadTrimHistory() {
  try {
    const history = await invoke('get_trim_history');
    if (!history || history.length === 0) {
      lastFreedText.classList.add('hidden');
      return;
    }
    const latest = history[history.length - 1]; // most recent
    const gbFreed = (latest.result.freed_bytes / 1_073_741_824).toFixed(1);
    const minsAgo = minutesAgo(latest.result.timestamp);
    lastFreedText.textContent = `Last freed: ${gbFreed} GB · ${minsAgo} min ago`;
    lastFreedText.classList.remove('hidden');
  } catch (e) {
    lastFreedText.classList.add('hidden');
    console.warn('loadTrimHistory failed:', e);
  }
}

// ── RAM bar rendering ─────────────────────────────────────────────────────

function updateRamBar(stats) {
  // Flow 15.5 — RAM Stats Cannot Be Read
  if (!stats) {
    ramBarFill.style.width = '0%';
    ramBarFill.style.background = 'var(--text-secondary)';
    ramStatusDot.className = 'status-dot';
    ramStatusText.textContent = 'Unable to read RAM stats';
    ramUsedText.textContent  = '—';
    ramTotalText.textContent = '';
    return;
  }

  const pct   = stats.pressure_pct ?? 0;
  const level = stats.pressure_level ?? 'Low';

  // Fill width
  ramBarFill.style.width = `${Math.min(pct, 100)}%`;

  // Fill color and dot color based on level
  let color, dotClass, levelLabel;
  if (level === 'Critical') {
    color = 'var(--red)';
    dotClass = 'dot-red';
    levelLabel = 'Critical';
  } else if (level === 'High' || level === 'Medium') {
    color = 'var(--amber)';
    dotClass = 'dot-amber';
    levelLabel = level === 'High' ? 'High' : 'Moderate';
  } else {
    color = 'var(--green)';
    dotClass = 'dot-green';
    levelLabel = 'Good';
  }

  ramBarFill.style.background = color;

  ramStatusDot.className = `status-dot ${dotClass}`;
  ramStatusText.textContent = `● ${levelLabel} — ${Math.round(pct)}% used`;
  ramStatusText.style.color = color;

  // Detail row
  const usedMb  = stats.used_mb  ?? 0;
  const totalMb = stats.total_mb ?? 0;
  ramUsedText.textContent  = `${(usedMb  / 1024).toFixed(1)} GB used`;
  ramTotalText.textContent = `of ${(totalMb / 1024).toFixed(1)} GB`;
}

// ── Process list rendering ────────────────────────────────────────────────

function renderProcessList(processes) {
  if (!processes || processes.length === 0) {
    processList.innerHTML = '<p class="process-loading">No processes found</p>';
    return;
  }

  // First group by name — skip system/protected processes
  const groups = new Map();
  for (const proc of processes) {
    if (proc.is_protected) continue;
    if (!groups.has(proc.name)) {
      groups.set(proc.name, {
        name: proc.name,
        pids: [proc.pid],
        ram_mb: proc.ram_mb,
        idle_seconds: proc.idle_seconds ?? 0,
        is_suspended: proc.is_suspended,
        is_protected: proc.is_protected,
        is_excluded: proc.is_excluded ?? false,
        icon_data_url: proc.icon_data_url ?? null,
      });
    } else {
      const g = groups.get(proc.name);
      g.pids.push(proc.pid);
      g.ram_mb += proc.ram_mb;
      // If any is not suspended, the group is active
      g.is_suspended = g.is_suspended && proc.is_suspended;
      // Take the min idle_seconds (most recently active instance)
      g.idle_seconds = Math.min(g.idle_seconds, proc.idle_seconds ?? 0);
    }
  }

  // Sort by RAM descending, take top 5
  const sorted = Array.from(groups.values()).sort((a, b) => b.ram_mb - a.ram_mb).slice(0, 5);

  processList.innerHTML = '';

  for (const proc of sorted) {
    processList.appendChild(buildProcessRow(proc));
  }
}

function buildProcessRow(proc) {
  const row = document.createElement('div');
  row.className = 'process-row' + (proc.is_suspended ? ' is-suspended' : '');
  row.dataset.pids = proc.pids.join(',');

  // Icon: real app icon when available, colored dot as fallback
  if (proc.icon_data_url) {
    const imgEl = document.createElement('img');
    imgEl.className = 'proc-icon';
    imgEl.src = proc.icon_data_url;
    imgEl.alt = '';
    row.appendChild(imgEl);
  } else {
    const dotEl = document.createElement('div');
    dotEl.className = 'proc-dot';
    dotEl.style.backgroundColor = nameToColor(proc.name);
    row.appendChild(dotEl);
  }

  // Name (truncate at 18 chars)
  const nameEl = document.createElement('span');
  nameEl.className = 'proc-name';
  const nameText = proc.name.length > 18 ? proc.name.slice(0, 17) + '…' : proc.name;
  nameEl.textContent = nameText + (proc.pids.length > 1 ? ` (${proc.pids.length})` : '');
  nameEl.title = proc.name;
  row.appendChild(nameEl);

  // RAM
  const ramEl = document.createElement('span');
  ramEl.className = 'proc-ram';
  ramEl.textContent = `${Math.round(proc.ram_mb)} MB`;
  row.appendChild(ramEl);

  // Idle time (shown when idle > 1 min)
  if ((proc.idle_seconds ?? 0) > 60) {
    const idleEl = document.createElement('span');
    idleEl.className = 'proc-idle';
    const mins = Math.floor(proc.idle_seconds / 60);
    idleEl.textContent = `(idle ${mins}m)`;
    row.appendChild(idleEl);
  }

  // Status or action buttons
  if (proc.is_excluded) {
    const statusEl = document.createElement('span');
    statusEl.className = 'proc-status';
    statusEl.textContent = 'Excluded';
    row.appendChild(statusEl);
  } else if (proc.is_suspended) {
    const statusEl = document.createElement('span');
    statusEl.className = 'proc-status';
    statusEl.textContent = 'Sleeping';
    row.appendChild(statusEl);

    const wakeBtn = document.createElement('button');
    wakeBtn.className = 'proc-btn';
    wakeBtn.textContent = '▶ Wake';
    wakeBtn.addEventListener('click', () => wakeGroup(proc.pids));
    row.appendChild(wakeBtn);
  } else {
    const trimBtn = document.createElement('button');
    trimBtn.className = 'proc-btn';
    trimBtn.textContent = 'Snooze';
    trimBtn.addEventListener('click', function() {
      this.textContent = '...';
      this.disabled = true;
      trimSingleProcess(proc.pids, this);
    });
    row.appendChild(trimBtn);

    if (hardSuspendEnabled && !proc.is_protected) {
      const suspBtn = document.createElement('button');
      suspBtn.className = 'proc-btn';
      suspBtn.textContent = '⏸';
      suspBtn.title = 'Hard suspend';
      suspBtn.addEventListener('click', function() {
        // Show inline confirmation before suspending
        const existing = row.querySelector('.proc-confirm');
        if (existing) { existing.remove(); return; }
        const confirmEl = document.createElement('span');
        confirmEl.className = 'proc-confirm';
        const shortName = proc.name.replace(/\.exe$/i, '');
        confirmEl.innerHTML = `Freeze ${shortName}? `;
        const yesBtn = document.createElement('button');
        yesBtn.className = 'proc-btn proc-confirm-yes';
        yesBtn.textContent = 'Yes';
        const noBtn = document.createElement('button');
        noBtn.className = 'proc-btn proc-confirm-no';
        noBtn.textContent = 'No';
        confirmEl.appendChild(yesBtn);
        confirmEl.appendChild(noBtn);
        row.appendChild(confirmEl);
        yesBtn.addEventListener('click', () => {
          confirmEl.remove();
          suspBtn.textContent = '...';
          suspBtn.disabled = true;
          suspendSingleProcess(proc.pids, suspBtn);
        });
        noBtn.addEventListener('click', () => confirmEl.remove());
      });
      row.appendChild(suspBtn);
    }
  }

  return row;
}

// ── Process actions ───────────────────────────────────────────────────────

async function trimSingleProcess(pids, btnEl) {
  try {
    await Promise.all(pids.map(pid => invoke('trim_process', { pid })));
    await loadProcessList();
    await loadRamStats();
  } catch (e) {
    if (btnEl) { btnEl.textContent = 'Snooze'; btnEl.disabled = false; }
    // Flow 15.1 — Access Denied on Process
    const row = btnEl ? btnEl.closest('.process-row') : null;
    if (row && String(e).startsWith('Access denied')) {
      showInlineError(row, "Can't snooze — this app is running as admin",
        'To trim admin apps, you\'d need to run Gingify as admin. We don\'t recommend this for normal use.');
    }
    console.warn('trim_process failed:', e);
  }
}

async function suspendSingleProcess(pids, btnEl) {
  try {
    await Promise.all(pids.map(pid => invoke('suspend_process', { pid })));
    await loadProcessList();
  } catch (e) {
    if (btnEl) { btnEl.textContent = '⏸'; btnEl.disabled = false; }
    const row = btnEl ? btnEl.closest('.process-row') : null;
    if (row && String(e).startsWith('Access denied')) {
      showInlineError(row, "Can't suspend — this app is running as admin",
        'To suspend admin apps, you\'d need to run Gingify as admin. We don\'t recommend this for normal use.');
    }
    console.warn('suspend_process failed:', e);
  }
}

async function wakeGroup(pids) {
  try {
    await Promise.all(pids.map(pid => invoke('resume_process', { pid })));
    await loadProcessList();
  } catch (e) {
    console.warn('resume_process failed:', e);
  }
}

// ── Free RAM Now ──────────────────────────────────────────────────────────

btnFreeNow.addEventListener('click', async () => {
  btnFreeNow.textContent = 'Freeing…';
  btnFreeNow.disabled = true;

  try {
    const result = await invoke('trim_all', { triggerStr: 'Manual' });
    showTrimResult(result);
    await loadRamStats();
    await loadProcessList();
    await loadTrimHistory();
  } catch (e) {
    showBanner('Trim failed', true);
    console.warn('trim_all failed:', e);
  } finally {
    btnFreeNow.textContent = 'Free RAM Now';
    btnFreeNow.disabled = false;
  }
});

function showTrimResult(result) {
  if (!result) return;
  const gb = (result.freed_bytes / 1_073_741_824).toFixed(1);
  const count = result.processes_trimmed ?? 0;
  const msg = count === 0
    ? 'Nothing freed — RAM already optimal'
    : `Freed ${gb} GB — ${count} apps snoozed`;
  showBanner(msg, count === 0);
}

function showBanner(msg, isAmber = false) {
  trimBanner.textContent = msg;
  trimBanner.className = 'trim-result-banner' + (isAmber ? ' banner-amber' : '');
  trimBanner.classList.remove('hidden');

  if (trimBannerTimer) clearTimeout(trimBannerTimer);
  trimBannerTimer = setTimeout(() => {
    trimBanner.classList.add('hidden');
    trimBannerTimer = null;
  }, 3000);
}

// ── Gaming Mode ───────────────────────────────────────────────────────────

btnGamingMode.addEventListener('click', async () => {
  const targetProfile = currentProfile === 'Gaming' ? 'Work' : 'Gaming';

  // Gaming Mode always uses hard suspend — always requires admin.
  if (targetProfile === 'Gaming' && !_isAdmin) {
    showInlineError(
      btnGamingMode.parentElement,
      'Gaming Mode + hard suspend requires admin',
      'Right-click Gingify in the tray and choose "Run as administrator", then try again.',
      true,
    );
    return;
  }

  _ipcBusy = true;
  try {
    await invoke('set_profile', { profile: targetProfile });
    currentProfile = targetProfile;
    profileSelect.value = targetProfile;
    updateGamingModeButton(targetProfile);
    // Refresh data so process list and stats reflect profile change immediately
    await Promise.all([loadRamStats(), loadProcessList(), loadTrimHistory()]);
  } catch (e) {
    console.warn('set_profile failed:', e);
  } finally {
    _ipcBusy = false;
  }
});

function updateGamingModeButton(profile) {
  if (profile === 'Gaming') {
    btnGamingMode.textContent = 'Exit Gaming Mode';
    btnGamingMode.classList.add('btn-active');
  } else {
    btnGamingMode.textContent = 'Gaming Mode';
    btnGamingMode.classList.remove('btn-active');
  }
}

// ── Profile dropdown ──────────────────────────────────────────────────────

profileSelect.addEventListener('change', async () => {
  const selected = profileSelect.value;

  // Same admin guard as the Gaming Mode button — Gaming always uses hard suspend.
  if (selected === 'Gaming' && !_isAdmin) {
    profileSelect.value = currentProfile; // revert dropdown visually
    showInlineError(
      profileSelect.parentElement,
      'Gaming Mode + hard suspend requires admin',
      'Right-click Gingify in the tray and choose "Run as administrator", then try again.',
      true,
    );
    return;
  }

  _ipcBusy = true;
  try {
    await invoke('set_profile', { profile: selected });
    currentProfile = selected;
    updateGamingModeButton(selected);
    await Promise.all([loadRamStats(), loadProcessList(), loadTrimHistory()]);
  } catch (e) {
    console.warn('set_profile failed:', e);
  } finally {
    _ipcBusy = false;
  }
});

// ── See All link ──────────────────────────────────────────────────────────

seeAllLink.addEventListener('click', (e) => {
  e.preventDefault();
  // _ipcBusy guards the blur handler from auto-hiding the popup during the
  // transition. Rust's open_detail_window_tab now hides the popup and shows
  // the detail window atomically, so we don't call hide_window here.
  _ipcBusy = true;
  invoke('open_detail_window_tab', { tab: 'processes' })
    .then(() => { _ipcBusy = false; })
    .catch(err => {
      _ipcBusy = false;
      console.warn('open_detail_window_tab failed:', err);
    });
});

// ── Utilities ─────────────────────────────────────────────────────────────

/**
 * Apply a theme class to <body> based on the theme string from config.
 * 'system' = no class; media query handles it via shared.css.
 */
function applyTheme(theme) {
  document.body.classList.remove('theme-light', 'theme-dark');
  if (theme === 'light') document.body.classList.add('theme-light');
  if (theme === 'dark')  document.body.classList.add('theme-dark');
}

/**
 * Append an inline error element to containerEl.
 * Flow 15 (UIUX_DESKTOP.md) — no icon, no animation.
 *
 * @param {Element} containerEl  - Parent element to append error into.
 * @param {string}  message      - Short user-facing error text.
 * @param {string}  [tooltip]    - Optional [?] tooltip text (access-denied hint).
 * @param {boolean} [isTemporary=true] - Auto-removes after 4 s when true.
 */
function showInlineError(containerEl, message, tooltip = '', isTemporary = true) {
  // Remove any existing inline error first
  const existing = containerEl.querySelector('.inline-error');
  if (existing) existing.remove();

  const errEl = document.createElement('div');
  errEl.className = 'inline-error';
  errEl.textContent = message;

  if (tooltip) {
    const hint = document.createElement('span');
    hint.style.cssText = 'margin-left:6px;cursor:help;font-weight:700;color:var(--red)';
    hint.textContent = '[?]';
    hint.title = tooltip;
    errEl.appendChild(hint);
  }

  containerEl.appendChild(errEl);
  if (isTemporary) setTimeout(() => errEl.remove(), 4000);
}

/**
 * Generate a consistent, visually distinct color from a process name.
 * Uses a simple hash → HSL with fixed saturation/lightness.
 */
function nameToColor(name) {
  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = (hash * 31 + name.charCodeAt(i)) & 0xffffffff;
  }
  const hue = Math.abs(hash) % 360;
  return `hsl(${hue}, 55%, 45%)`;
}

/**
 * Compute how many minutes ago a given SystemTime (seconds since UNIX epoch) was.
 * The Rust `SystemTime` serialises as `{ secs_since_epoch: N, nanos_since_epoch: N }`.
 */
function minutesAgo(timestamp) {
  if (!timestamp || !timestamp.secs_since_epoch) return '?';
  const nowSecs = Date.now() / 1000;
  const diffSecs = nowSecs - timestamp.secs_since_epoch;
  return Math.max(0, Math.round(diffSecs / 60));
}
