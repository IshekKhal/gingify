// Gingify Detail Window — app.js
// Full implementation: tab switching, all 5 tabs, live data, IPC.
// Tauri IPC is available globally via window.__TAURI__ (withGlobalTauri: true).

'use strict';

const { invoke } = window.__TAURI__.core;
const { listen }  = window.__TAURI__.event;

// ── State ──────────────────────────────────────────────────────────────────

let _processes      = [];  // full process list (all pages)
let _showSystem     = false;
let _sortMode       = 'ram';
let _filterText     = '';
let _config         = null;
let _trimBannerTimer = null;
let _ramTotalMb     = 0;   // updated by updateOverviewRam; caps "freed today" display
let _isAdmin        = false; // cached result of is_admin() — used to gate Gaming Mode

// Monotonic peak tracking for overview stats (Bug 6 fix)
let _peakFreedBytes = 0;
let _peakAppsCount  = 0;
let _peakDayLabel   = '';  // YYYY-MM-DD — reset peaks on day rollover

// ── Tab switching ──────────────────────────────────────────────────────────

document.querySelectorAll('[data-tab]').forEach(btn => {
  btn.addEventListener('click', () => switchTab(btn.dataset.tab));
});

function switchTab(name) {
  document.querySelectorAll('.tab-content').forEach(s => s.classList.remove('active'));
  document.querySelectorAll('.tab-btn').forEach(b => {
    b.classList.remove('active');
    b.setAttribute('aria-selected', 'false');
  });

  const section = document.getElementById(`tab-${name}`);
  const btn     = document.getElementById(`tab-btn-${name}`);
  if (section) section.classList.add('active');
  if (btn)     { btn.classList.add('active'); btn.setAttribute('aria-selected', 'true'); }
}

// ── On load — fetch all data ───────────────────────────────────────────────

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
  // Load admin status first so Gaming Mode guard is ready before user can interact
  _isAdmin = await checkAdminWithTimeout();
  // FIX: inject real version from Cargo.toml instead of hardcoded string
  try {
    const v = await window.__TAURI__.app.getVersion();
    const vEl = document.getElementById('about-version');
    if (vEl) vEl.textContent = `Gingify v${v}`;
  } catch (e) { console.error('[gingify] getVersion failed:', e); }
  await Promise.all([
    loadRamStats(),
    loadAllProcesses(),
    loadBloatList(),
    loadHistory(),
    loadConfig(),
    loadCurrentProfile(),
    loadTrimHistory(),
  ]);
  bindSettings();
});

// ── Refresh data whenever detail window is brought into focus ──────────────
// DOMContentLoaded fires once at window creation. Every subsequent time the
// detail window is opened or switched back to, we refresh so data is current.

window.addEventListener('focus', async () => {
  await Promise.all([
    loadRamStats(),
    loadAllProcesses(),
    loadBloatList(),
    loadHistory(),
    loadTrimHistory(),
    loadCurrentProfile(),
    loadConfig(),
  ]);
});

// ── Real-time updates ───────────────────────────────────────────────

listen('gingify://ram-update', event => {
  updateOverviewRam(event.payload);
  loadAllProcesses().catch(() => {});
  loadBloatList().catch(() => {});
  loadTrimHistory().catch(() => {});
});

listen('gingify://update-available', (event) => {
  const badge = document.getElementById('about-update-badge');
  if (badge) {
    badge.textContent = `Update available: v${event.payload}`;
    badge.classList.remove('hidden');
  }
});

// Task 7 (PROMPT_DESKTOP_10) — switch to the named tab when tray 'Settings' fires
listen('gingify://switch-tab', (event) => {
  switchTab(event.payload); // e.g. 'settings'
});

// Task 6 (PROMPT_DESKTOP_10) — reapply theme when config changes from Settings
listen('gingify://config-changed', (event) => {
  if (event.payload?.theme) applyTheme(event.payload.theme);
});

// History updates (handled inside ram-update)

// ─────────────────────────────────────────────────────────────
// OVERVIEW TAB
// ─────────────────────────────────────────────────────────────

async function loadRamStats() {
  try {
    // Retry loop — monitor may not have completed its first poll yet on startup
    let stats = null;
    for (let attempt = 0; attempt < 6; attempt++) {
      stats = await invoke('get_ram_stats');
      if (stats && stats.total_mb > 0) break;
      if (attempt < 5) await new Promise(r => setTimeout(r, 1000));
    }
    updateOverviewRam(stats);
  } catch (e) {
    console.warn('get_ram_stats failed:', e);
  }
}

function updateOverviewRam(stats) {
  // Flow 15.5 — RAM Stats Cannot Be Read
  if (!stats) {
    const fill = document.getElementById('ov-ram-bar-fill');
    if (fill) { fill.style.width = '0%'; fill.style.background = 'var(--text-secondary)'; }
    const statusEl = document.getElementById('ov-ram-status');
    if (statusEl) statusEl.textContent = 'Unable to read RAM stats';
    const detailEl = document.getElementById('ov-ram-detail');
    if (detailEl) detailEl.textContent = '—';
    return;
  }
  const pct   = stats.pressure_pct ?? 0;
  const level = stats.pressure_level ?? 'Low';
  const usedGb  = ((stats.used_mb ?? 0) / 1024).toFixed(1);
  const totalGb = ((stats.total_mb ?? 0) / 1024).toFixed(1);
  if (stats.total_mb) _ramTotalMb = stats.total_mb;

  const fill = document.getElementById('ov-ram-bar-fill');
  fill.style.width = `${Math.min(pct, 100)}%`;

  let color, label;
  if (level === 'Critical') { color = 'var(--red)';   label = 'Critical'; }
  else if (level === 'High')   { color = 'var(--amber)'; label = 'High pressure'; }
  else if (level === 'Medium') { color = 'var(--amber)'; label = 'Medium pressure'; }
  else                         { color = 'var(--green)'; label = 'Low pressure'; }

  fill.style.background = color;

  const statusEl = document.getElementById('ov-ram-status');
  statusEl.textContent = `${Math.round(pct)}% — ${label}`;
  statusEl.style.color = color;

  document.getElementById('ov-ram-detail').textContent = `${usedGb} / ${totalGb} GB`;
}

async function loadCurrentProfile() {
  try {
    const profile = await invoke('get_current_profile');
    setProfileRadio(profile);
  } catch (e) {
    console.warn('get_current_profile failed:', e);
  }
}

function setProfileRadio(profile) {
  const radios = document.querySelectorAll('input[name="ov-profile"]');
  radios.forEach(r => { r.checked = (r.value === profile); });
}

document.getElementById('ov-profile-group').addEventListener('change', async e => {
  const target = e.target;
  if (target.name === 'ov-profile') {
    // Gaming Mode always uses hard suspend — always requires admin.
    if (target.value === 'Gaming' && !_isAdmin) {
      // Revert the radio to the current active profile
      const radios = document.querySelectorAll('input[name="ov-profile"]');
      radios.forEach(r => { r.checked = (r.value !== 'Gaming'); }); // revert visually
      await loadCurrentProfile(); // sync to real backend state
      showInlineError(
        document.getElementById('ov-profile-group'),
        'Gaming Mode + hard suspend requires admin. Restart Gingify as administrator.',
        'Right-click Gingify in the tray and choose "Run as administrator", then try again.',
        true,
      );
      return;
    }
    try {
      await invoke('set_profile', { profile: target.value });
      // Refresh all data so changes from profile activation are visible immediately.
      // loadConfig() re-populates Settings tab with any Focus-mode overrides or restores.
      await Promise.all([loadRamStats(), loadAllProcesses(), loadBloatList(), loadTrimHistory(), loadConfig()]);
    } catch (err) {
      console.warn('set_profile failed:', err);
    }
  }
});

async function loadTrimHistory() {
  try {
    const history = await invoke('get_trim_history');
    computeOverviewStats(history);
    renderHistory(history);
  } catch (e) {
    console.warn('get_trim_history failed:', e);
  }
}

function computeOverviewStats(history) {
  if (!history || history.length === 0) {
    document.getElementById('ov-freed-value').textContent = '0 GB';
    document.getElementById('ov-apps-value').textContent  = '0';
    document.getElementById('ov-auto-value').textContent  = '0';
    return;
  }

  const todayStart = new Date();
  todayStart.setHours(0, 0, 0, 0);
  const todayStartSecs = todayStart.getTime() / 1000;

  let totalFreedBytes = 0;
  let autoCount       = 0;
  // FIX: count unique PIDs across all today's events instead of summing processes_trimmed
  const allPidsToday = new Set();

  for (const ev of history) {
    const ts = ev.result?.timestamp?.secs_since_epoch ?? 0;
    if (ts >= todayStartSecs) {
      totalFreedBytes += ev.result?.freed_bytes ?? 0;
      for (const pid of ev.result?.unique_pids ?? []) allPidsToday.add(pid);
      if (ev.trigger === 'Auto') autoCount++;
    }
  }
  const totalApps = allPidsToday.size;

  // Monotonic peak: reset on day rollover, then keep the high-water mark
  // so eviction from the 50-entry history ring buffer never decreases the display.
  const todayLabel = new Date().toISOString().slice(0, 10);
  if (todayLabel !== _peakDayLabel) {
    _peakFreedBytes = 0;
    _peakAppsCount  = 0;
    _peakDayLabel   = todayLabel;
  }
  _peakFreedBytes = Math.max(_peakFreedBytes, totalFreedBytes);
  _peakAppsCount  = Math.max(_peakAppsCount,  totalApps);

  const capBytes = _ramTotalMb > 0 ? _ramTotalMb * 1024 * 1024 : Infinity;
  const gbFreed  = (Math.min(_peakFreedBytes, capBytes) / 1_073_741_824).toFixed(1);
  document.getElementById('ov-freed-value').textContent = `${gbFreed} GB`;
  document.getElementById('ov-apps-value').textContent  = String(_peakAppsCount);
  document.getElementById('ov-auto-value').textContent  = String(autoCount);
}

// Overview: Free RAM Now
document.getElementById('ov-btn-free-now').addEventListener('click', async () => {
  const btn = document.getElementById('ov-btn-free-now');
  btn.textContent = 'Freeing…';
  btn.disabled = true;
  try {
    const result = await invoke('trim_all', { triggerStr: 'Manual' });
    showOverviewBanner(formatTrimResult(result), false);
    await loadRamStats();
    await loadAllProcesses();
    await loadBloatList();
    await loadTrimHistory();
  } catch (e) {
    showOverviewBanner('Snooze failed', true);
    console.warn('trim_all failed:', e);
  } finally {
    btn.textContent = 'Free RAM Now';
    btn.disabled = false;
  }
});

// Overview: Snooze Idle Only
document.getElementById('ov-btn-trim-idle').addEventListener('click', async () => {
  const btn = document.getElementById('ov-btn-trim-idle');
  btn.textContent = 'Snoozing…';
  btn.disabled = true;
  try {
    // ManualIdle = trim only processes that have been idle >= idle_threshold_secs
    // (unlike plain Manual which uses threshold=0 and trims everything)
    const result = await invoke('trim_all', { triggerStr: 'ManualIdle' });
    showOverviewBanner(formatTrimResult(result), false);
    await loadRamStats();
    await loadAllProcesses();
    await loadBloatList();
    await loadTrimHistory();
  } catch (e) {
    showOverviewBanner('Snooze failed', true);
  } finally {
    btn.textContent = 'Snooze Idle Only';
    btn.disabled = false;
  }
});

function showOverviewBanner(msg, isAmber) {
  const banner = document.getElementById('ov-trim-banner');
  banner.textContent = msg;
  banner.className = 'trim-banner' + (isAmber ? ' banner-amber' : '');
  banner.classList.remove('hidden');
  if (_trimBannerTimer) clearTimeout(_trimBannerTimer);
  _trimBannerTimer = setTimeout(() => {
    banner.classList.add('hidden');
    _trimBannerTimer = null;
  }, 3000);
}

function formatTrimResult(result) {
  if (!result) return 'Done';
  const gb    = (result.freed_bytes / 1_073_741_824).toFixed(1);
  const count = result.processes_trimmed ?? 0;
  return count === 0
    ? 'Nothing to free right now'
    : `Freed ${gb} GB — ${count} apps snoozed`;
}

// Bloat alert in Overview — links to Bloat tab
document.getElementById('ov-bloat-alert-link').addEventListener('click', () => {
  switchTab('bloat');
});

function updateBloatAlert(bloatList) {
  const running = (bloatList || []).filter(b => b.ram_mb > 0);
  const alertEl = document.getElementById('ov-bloat-alert');
  if (running.length === 0) {
    alertEl.classList.add('hidden');
    return;
  }
  const totalMb  = running.reduce((s, b) => s + b.ram_mb, 0);
  const names    = running.map(b => `${b.name} (${Math.round(b.ram_mb)} MB)`).join('  ');
  document.getElementById('ov-bloat-alert-text').textContent =
    `AI Bloat running: ${names}`;
  alertEl.classList.remove('hidden');
}

// ─────────────────────────────────────────────────────────────
// PROCESSES TAB
// ─────────────────────────────────────────────────────────────

async function loadAllProcesses() {
  try {
    // Retry loop — process_map is empty until the monitor completes its first
    // poll cycle (up to ~5 s after startup).  Retrying avoids showing an empty
    // list on initial open.
    let procs = [];
    for (let attempt = 0; attempt < 5; attempt++) {
      // FIX: pass includeSystem flag so backend filters correctly
      procs = await invoke('get_process_list', { includeSystem: _showSystem });
      if (procs && procs.length > 0) break;
      if (attempt < 4) await new Promise(r => setTimeout(r, 1000));
    }
    _processes = procs || [];
    renderProcessTable();
  } catch (e) {
    document.getElementById('proc-list-container').innerHTML =
      '<p class="loading-text">Process list unavailable</p>';
    console.warn('get_process_list failed:', e);
  }
}

// Sort
document.getElementById('proc-sort').addEventListener('change', e => {
  _sortMode = e.target.value;
  renderProcessTable();
});

// Filter
document.getElementById('proc-filter').addEventListener('input', e => {
  _filterText = e.target.value.toLowerCase().trim();
  renderProcessTable();
});

// Snooze all idle
document.getElementById('proc-btn-trim-all-idle').addEventListener('click', async () => {
  const btn = document.getElementById('proc-btn-trim-all-idle');
  btn.textContent = 'Snoozing…';
  btn.disabled = true;
  try {
    await invoke('trim_all', { triggerStr: 'ManualIdle' });
    await loadAllProcesses();
    await loadRamStats();
    await loadBloatList();
    await loadTrimHistory();
  } catch (e) {
    console.warn('trim_all failed:', e);
  } finally {
    btn.textContent = 'Snooze Idle Apps';
    btn.disabled = false;
  }
});

// Show system toggle — re-fetch from backend with updated includeSystem flag
document.getElementById('proc-show-system').addEventListener('change', async e => {
  _showSystem = e.target.checked;
  await loadAllProcesses();
});

const BROWSERS = new Set(['chrome.exe','msedge.exe','firefox.exe','brave.exe','opera.exe','vivaldi.exe','arc.exe']);

function isBrowser(name) {
  return BROWSERS.has(name.toLowerCase());
}

function renderProcessTable() {
  const container = document.getElementById('proc-list-container');

  // Separate into Apps (windowed), Background (no window, >= 30MB), and System.
  // If window-detection isn't available (all has_window=false), fall back to
  // a single unified list so nothing is hidden.
  const hasWindowInfo = _processes.some(p => p.has_window && !p.is_protected);
  let appList, bgList;
  if (hasWindowInfo) {
    appList = _processes.filter(p => !p.is_protected && p.has_window);
    bgList  = _processes.filter(p => !p.is_protected && !p.has_window && p.ram_mb >= 30);
  } else {
    appList = _processes.filter(p => !p.is_protected);
    bgList  = [];
  }
  let systemList = _processes.filter(p => p.is_protected);

  // Apply text filter to all
  if (_filterText) {
    appList    = appList.filter(p => p.name.toLowerCase().includes(_filterText));
    bgList     = bgList.filter(p => p.name.toLowerCase().includes(_filterText));
    systemList = systemList.filter(p => p.name.toLowerCase().includes(_filterText));
  }

  // Group helper
  function groupProcs(procs) {
    const groups = new Map();
    for (const proc of procs) {
      if (!groups.has(proc.name)) {
        groups.set(proc.name, {
          name: proc.name,
          icon_data_url: proc.icon_data_url ?? null,
          pids: [proc.pid],
          ram_mb: proc.ram_mb,
          idle_seconds: proc.idle_seconds,
          is_suspended: proc.is_suspended,
          is_protected: proc.is_protected,
          is_excluded: proc.is_excluded,
          has_window: proc.has_window,
          any_has_window: proc.has_window
        });
      } else {
        const g = groups.get(proc.name);
        g.pids.push(proc.pid);
        g.ram_mb += proc.ram_mb;
        g.idle_seconds = Math.min(g.idle_seconds, proc.idle_seconds);
        g.is_suspended = g.is_suspended && proc.is_suspended;
        g.any_has_window = g.any_has_window || proc.has_window;
      }
    }
    return Array.from(groups.values());
  }

  // Group ALL non-protected first, then split by has_window
  const allGrouped = groupProcs(_processes.filter(p => !p.is_protected));
  let appGrouped    = allGrouped.filter(g => g.any_has_window);
  let bgGrouped     = allGrouped.filter(g => !g.any_has_window && g.ram_mb >= 30);
  let systemGrouped = groupProcs(systemList);

  // Sort by selected mode (system always alphabetical)
  function sortByMode(list, mode) {
    if (mode === 'ram') {
      list.sort((a, b) => b.ram_mb - a.ram_mb);
    } else if (mode === 'name') {
      list.sort((a, b) => a.name.localeCompare(b.name));
    } else if (mode === 'idle') {
      list.sort((a, b) => b.idle_seconds - a.idle_seconds);
    }
  }
  sortByMode(appGrouped, _sortMode);
  sortByMode(bgGrouped, _sortMode);
  systemGrouped.sort((a, b) => a.name.localeCompare(b.name));

  // Hide system if checkbox unchecked
  if (!_showSystem) systemGrouped = [];

  if (appGrouped.length === 0 && bgGrouped.length === 0 && systemGrouped.length === 0) {
    container.innerHTML = '<p class="loading-text">No processes match the filter.</p>';
    return;
  }

  const hardSuspendOn = _config?.hard_suspend_enabled ?? false;

  container.innerHTML = '';

  // Apps section header
  const appsHeader = document.createElement('div');
  appsHeader.className = 'proc-table-header';
  appsHeader.style.cssText = 'opacity:1;margin-top:0;font-size:12px';
  appsHeader.innerHTML = `<span>Apps (${appGrouped.length})</span><span></span><span></span><span></span><span></span>`;
  container.appendChild(appsHeader);

  // Column header row
  const header = document.createElement('div');
  header.className = 'proc-table-header';
  header.innerHTML = `
    <span>Process</span>
    <span>RAM</span>
    <span>Idle Time</span>
    <span>Status</span>
    <span></span>
  `;
  container.appendChild(header);

  // Apps section rows
  for (const proc of appGrouped) {
    container.appendChild(buildProcRow(proc, hardSuspendOn));
  }

  // Background section — only rendered when window categorization is active
  if (bgGrouped.length > 0) {
    const bgHeader = document.createElement('div');
    bgHeader.className = 'proc-table-header';
    bgHeader.style.cssText = 'opacity:0.65;margin-top:8px;font-size:11px';
    bgHeader.innerHTML = `<span>Background Processes (${bgGrouped.length})</span><span></span><span></span><span></span><span></span>`;
    container.appendChild(bgHeader);

    for (const proc of bgGrouped) {
      container.appendChild(buildProcRow(proc, hardSuspendOn));
    }
  }

  // System processes always greyed at bottom
  if (systemGrouped.length > 0) {
    const sep = document.createElement('div');
    sep.className = 'proc-table-header';
    sep.style.cssText = 'opacity:0.45;margin-top:8px;font-size:10px';
    sep.innerHTML = '<span>System processes</span><span></span><span></span><span></span><span></span>';
    container.appendChild(sep);

    for (const proc of systemGrouped) {
      container.appendChild(buildProcRow(proc, hardSuspendOn));
    }
  }
}

function buildProcRow(proc, hardSuspendOn) {
  const row = document.createElement('div');
  row.className = 'proc-row'
    + (proc.is_suspended ? ' is-suspended' : '')
    + (proc.is_protected ? ' is-protected' : '');
  row.dataset.pids = proc.pids.join(',');

  const idleStr = formatIdle(proc.idle_seconds);
  let status = '';
  if (proc.is_suspended) {
    status = 'Suspended';
  } else if (proc.is_protected) {
    status = 'Protected';
  } else if (proc.is_excluded && isBrowser(proc.name)) {
    status = 'Managed by Extension';
  } else if (proc.is_excluded) {
    status = 'Excluded';
  } else {
    status = proc.idle_seconds < 60 ? 'Active' : 'Idle';
  }

  const iconHtml = proc.icon_data_url
    ? `<img class="proc-row-icon" src="${proc.icon_data_url}" alt="" />`
    : `<div class="proc-row-dot" style="background:${nameToColor(proc.name)}"></div>`;

  row.innerHTML = `
    <div class="proc-row-name">
      ${iconHtml}
      <span class="proc-row-name-text" title="${escHtml(proc.name)}">${escHtml(proc.name)}${proc.pids.length > 1 ? ` (${proc.pids.length})` : ''}</span>
    </div>
    <span class="proc-row-ram">${Math.round(proc.ram_mb)} MB</span>
    <span class="proc-row-idle">${idleStr}</span>
    <span class="proc-row-status">${status}</span>
    <div class="proc-row-actions"></div>
  `;

  const actionsEl = row.querySelector('.proc-row-actions');

  if (!proc.is_protected && !proc.is_excluded) {
    if (proc.is_suspended) {
      const resumeBtn = document.createElement('button');
      resumeBtn.className = 'proc-action-btn';
      resumeBtn.textContent = '▶ Resume';
      resumeBtn.addEventListener('click', ev => { ev.stopPropagation(); resumeProc(proc.pids, row); });
      actionsEl.appendChild(resumeBtn);
    } else {
      const trimBtn = document.createElement('button');
      trimBtn.className = 'proc-action-btn';
      trimBtn.textContent = 'Snooze';
      trimBtn.addEventListener('click', ev => {
        ev.stopPropagation();
        trimBtn.textContent = '...'; trimBtn.disabled = true;
        trimProc(proc.pids, row, trimBtn);
      });
      actionsEl.appendChild(trimBtn);

      if (hardSuspendOn) {

        const suspBtn = document.createElement('button');
        suspBtn.className = 'proc-action-btn';
        suspBtn.textContent = '⏸';
        suspBtn.title = 'Hard suspend';
        suspBtn.addEventListener('click', ev => {
          ev.stopPropagation(); 
          suspBtn.textContent = '...'; suspBtn.disabled = true;
          suspendProc(proc.pids, row, suspBtn); 
        });
        actionsEl.appendChild(suspBtn);
      }
    }
  }

  // Click to expand inline details
  let expanded = false;
  row.addEventListener('click', () => {
    expanded = !expanded;
    renderProcExpand(row, proc, expanded);
  });

  return row;
}

function renderProcExpand(rowEl, proc, show) {
  const existingExpand = rowEl.nextSibling;
  if (existingExpand && existingExpand.classList && existingExpand.classList.contains('proc-expand')) {
    existingExpand.remove();
  }
  if (!show) return;

  const expand = document.createElement('div');
  expand.className = 'proc-expand';
  expand.innerHTML = `
    <div class="proc-expand-path">Path: <span style="font-style:italic">${escHtml(proc.name)}</span> &nbsp;·&nbsp; PIDs: ${proc.pids.slice(0, 10).join(', ')}${proc.pids.length > 10 ? '...' : ''}</div>
    <div class="proc-expand-actions">
      ${!proc.is_excluded && !proc.is_protected
        ? `<button class="btn" id="exc-btn-${proc.pids[0]}" style="font-size:11px;padding:3px 8px">Exclude from snoozing</button>` : ''}
    </div>
  `;
  rowEl.insertAdjacentElement('afterend', expand);

  const excBtn = document.getElementById(`exc-btn-${proc.pids[0]}`);
  if (excBtn) {
    excBtn.addEventListener('click', async () => {
      try {
        await invoke('add_exclusion', { name: proc.name });
        excBtn.textContent = 'Excluded ✓';
        excBtn.disabled = true;
        await loadAllProcesses();
        await loadConfig();
        renderExclusionChips();
      } catch (e) {
        console.warn('add_exclusion failed:', e);
      }
    });
  }
}

async function trimProc(pids, rowEl, btnEl) {
  try {
    await Promise.all(pids.map(pid => invoke('trim_process', { pid })));
    await loadAllProcesses();
    await loadRamStats();
  } catch (e) {
    if (btnEl) { btnEl.textContent = 'Snooze'; btnEl.disabled = false; }
    // Flow 15.1 — Access Denied on Process
    if (rowEl && String(e).startsWith('Access denied')) {
      showInlineError(rowEl, "Can't snooze — this app is running as admin",
        'To trim admin apps, you\'d need to run Gingify as admin. We don\'t recommend this for normal use.');
    }
    console.warn('trim_process failed:', e);
  }
}

async function suspendProc(pids, rowEl, btnEl) {
  try {
    await Promise.all(pids.map(pid => invoke('suspend_process', { pid })));
    await loadAllProcesses();
  } catch (e) {
    if (btnEl) { btnEl.textContent = '⏸'; btnEl.disabled = false; }
    if (rowEl && String(e).startsWith('Access denied')) {
      showInlineError(rowEl, "Can't suspend — this app is running as admin",
        'To suspend admin apps, you\'d need to run Gingify as admin. We don\'t recommend this for normal use.');
    }
    console.warn('suspend_process failed:', e);
  }
}

async function resumeProc(pids, rowEl) {
  try {
    await Promise.all(pids.map(pid => invoke('resume_process', { pid })));
    await loadAllProcesses();
  } catch (e) {
    console.warn('resume_process failed:', e);
  }
}

// ─────────────────────────────────────────────────────────────
// BLOAT TAB
// ─────────────────────────────────────────────────────────────

async function loadBloatList() {
  try {
    const list = await invoke('get_bloat_list');
    renderBloatList(list);
    updateBloatAlert(list);
  } catch (e) {
    document.getElementById('bloat-list-container').innerHTML =
      '<p class="loading-text">Bloat list unavailable</p>';
    console.warn('get_bloat_list failed:', e);
  }
}

// Descriptions for known bloat entries
const BLOAT_DESCRIPTIONS = {
  'Copilot':       'Windows Copilot is an AI assistant built into Windows 11 that runs constantly in the background. It uses 400–800 MB even when you never interact with it.',
  'Recall':        'Recall (Windows Recall / AIXHost) captures screenshots of everything you do and processes them locally with AI. It can use 500 MB–1 GB even when paused.',
  'AI Frameworks': 'Windows AI framework services (WinML, WindowsAI) sit idle but hold RAM for any app that might want to run AI workloads.',
  'Widgets':       'The Windows Widgets bar and WidgetService load news, weather and other widgets using web content in the background.',
  'Xbox GameBar':  'Xbox Game Bar is an overlay and performance HUD for gaming. Its background processes run even when you are not gaming.',
};

function renderBloatList(list) {
  const container = document.getElementById('bloat-list-container');
  container.innerHTML = '';

  if (!list || list.length === 0) {
    container.innerHTML = '<p class="loading-text">No bloat entries found.</p>';
    document.getElementById('bloat-total-text').textContent = '';
    return;
  }

  let totalRunningMb = 0;

  for (const entry of list) {
    const notRunning = entry.ram_mb <= 0;
    if (!notRunning) totalRunningMb += entry.ram_mb;

    const el = document.createElement('div');
    el.className = 'bloat-entry' + (notRunning ? ' not-running' : '');
    el.dataset.bloatName = entry.name;

    const exeList = (entry.exe_names || []).join(', ');
    const ramText = notRunning ? '—' : `${Math.round(entry.ram_mb)} MB`;
    const statusText = notRunning ? 'Not running'
                     : entry.is_suspended ? 'Suspended' : '';

    const suspBtnLabel = entry.is_suspended ? '▶ Resume' : 'Suspend';
    const suspBtnId    = `bloat-susp-${encodeId(entry.name)}`;
    const infoBtnId    = `bloat-info-${encodeId(entry.name)}`;
    const infoExpandId = `bloat-info-expand-${encodeId(entry.name)}`;

    el.innerHTML = `
      <div class="bloat-entry-header">
        <span class="bloat-entry-name">${escHtml(entry.name)}</span>
        <span class="bloat-entry-ram">${ramText}</span>
        <div class="bloat-entry-actions">
          <button id="${suspBtnId}" class="btn" style="font-size:11px;padding:3px 8px"
            ${notRunning ? 'disabled' : ''}>${suspBtnLabel}</button>
          <button id="${infoBtnId}" class="btn btn-ghost" style="font-size:11px;padding:3px 8px">Info</button>
        </div>
      </div>
      <div class="bloat-entry-exes">${escHtml(exeList)}${statusText ? ` &nbsp;·&nbsp; <strong>${escHtml(statusText)}</strong>` : ''}</div>
      <div id="${infoExpandId}" class="bloat-entry-info hidden"></div>
    `;

    container.appendChild(el);

    // Suspend / Resume button
    const suspBtn = document.getElementById(suspBtnId);
    if (suspBtn && !notRunning) {
      suspBtn.addEventListener('click', async () => {
        try {
          if (entry.is_suspended) {
            await invoke('resume_bloat', { name: entry.name });
          } else {
            await invoke('suspend_bloat', { name: entry.name });
          }
          await loadBloatList();
        } catch (e) {
          const errMsg = String(e).includes('not available')
            ? 'Cannot suspend — hard suspend unavailable on this system'
            : `Failed: ${e}`;
          console.warn('bloat action failed:', e);
          const entryEl = document.querySelector(`[data-bloat-name="${CSS.escape(entry.name)}"]`);
          if (entryEl) showInlineError(entryEl, errMsg, '', true);
        }
      });
    }

    // Info button — inline expand
    const infoBtn    = document.getElementById(infoBtnId);
    const infoExpand = document.getElementById(infoExpandId);
    if (infoBtn && infoExpand) {
      infoBtn.addEventListener('click', () => {
        if (infoExpand.classList.contains('hidden')) {
          const desc = BLOAT_DESCRIPTIONS[entry.name] || 'No description available.';
          infoExpand.textContent = desc;
          infoExpand.classList.remove('hidden');
          infoBtn.textContent = 'Hide';
        } else {
          infoExpand.classList.add('hidden');
          infoBtn.textContent = 'Info';
        }
      });
    }
  }

  // Total
  document.getElementById('bloat-total-text').textContent =
    `Total bloat RAM: ${Math.round(totalRunningMb)} MB`;
}

// Suspend All Bloat
document.getElementById('bloat-btn-suspend-all').addEventListener('click', async () => {
  const btn = document.getElementById('bloat-btn-suspend-all');
  // FIX: check if any bloat is actually running before calling IPC
  const bloatContainer = document.getElementById('bloat-list-container');
  const runningEntries = bloatContainer.querySelectorAll('.bloat-entry:not(.not-running)');
  if (runningEntries.length === 0) {
    const totalEl = document.getElementById('bloat-total-text');
    const prev = totalEl.textContent;
    totalEl.textContent = 'No bloat services are currently running';
    setTimeout(() => { totalEl.textContent = prev; }, 3000);
    return;
  }
  btn.textContent = 'Suspending…';
  btn.disabled = true;
  try {
    await invoke('suspend_all_bloat');
    await loadBloatList();
  } catch (e) {
    console.warn('suspend_all_bloat failed:', e);
  } finally {
    btn.textContent = 'Suspend All Bloat';
    btn.disabled = false;
  }
});

// ─────────────────────────────────────────────────────────────
// HISTORY TAB
// ─────────────────────────────────────────────────────────────

async function loadHistory() {
  try {
    const history = await invoke('get_trim_history');
    renderHistory(history);
  } catch (e) {
    document.getElementById('history-list-container').innerHTML =
      '<p class="loading-text">History unavailable</p>';
    console.warn('get_trim_history failed:', e);
  }
}

function renderHistory(history) {
  const container = document.getElementById('history-list-container');
  container.innerHTML = '';

  if (!history || history.length === 0) {
    container.innerHTML = '<p class="loading-text">No trim history yet.</p>';
    document.getElementById('history-week-total').textContent = '';
    return;
  }

  // Sort descending by time
  const sorted = [...history].sort(
    (a, b) => (b.result?.timestamp?.secs_since_epoch ?? 0)
             - (a.result?.timestamp?.secs_since_epoch ?? 0)
  );

  // Group by day
  const today     = dayLabel(new Date());
  const yesterday = dayLabel(new Date(Date.now() - 86400000));

  const groups = new Map(); // label → [events]
  for (const ev of sorted) {
    const ts = ev.result?.timestamp?.secs_since_epoch;
    const d  = ts ? new Date(ts * 1000) : null;
    let label;
    if (!d) { label = 'Unknown'; }
    else {
      const dl = dayLabel(d);
      label = dl === today     ? 'Today'
            : dl === yesterday ? 'Yesterday'
            : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
    }
    if (!groups.has(label)) groups.set(label, []);
    groups.get(label).push(ev);
  }

  for (const [label, events] of groups) {
    // Day header
    const hdr = document.createElement('div');
    hdr.className = 'history-day-header';
    hdr.textContent = label;
    container.appendChild(hdr);

    for (const ev of events) {
      const row = buildHistoryRow(ev);
      container.appendChild(row);
    }
  }

  // Week total
  const weekStart = Date.now() - 7 * 86400000;
  const weekStartSecs = weekStart / 1000;
  let weekBytes = 0;
  for (const ev of history) {
    const ts = ev.result?.timestamp?.secs_since_epoch ?? 0;
    if (ts >= weekStartSecs) weekBytes += ev.result?.freed_bytes ?? 0;
  }
  const weekGb = (weekBytes / 1_073_741_824).toFixed(1);
  document.getElementById('history-week-total').textContent =
    `Total freed this week: ${weekGb} GB`;
}

function buildHistoryRow(ev) {
  const ts    = ev.result?.timestamp?.secs_since_epoch;
  const d     = ts ? new Date(ts * 1000) : null;
  const time  = d ? d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }) : '—';
  const gb    = ((ev.result?.freed_bytes ?? 0) / 1_073_741_824).toFixed(1);
  const count = ev.result?.processes_trimmed ?? 0;
  const type  = triggerLabel(ev.trigger);

  const row = document.createElement('div');
  row.className = 'history-row';
  row.innerHTML = `
    <span class="history-time">${time}</span>
    <span class="history-type">${type}</span>
    <span class="history-freed">Freed ${gb} GB</span>
    <span class="history-count">${count} apps</span>
  `;

  // Click to expand per-app breakdown
  let open = false;
  row.addEventListener('click', () => {
    open = !open;
    renderHistoryExpand(row, ev, open);
  });

  return row;
}

function renderHistoryExpand(rowEl, ev, show) {
  const existing = rowEl.nextSibling;
  if (existing && existing.classList && existing.classList.contains('history-expand')) {
    existing.remove();
  }
  if (!show) return;

  const expand = document.createElement('div');
  expand.className = 'history-expand';

  const gb    = ((ev.result?.freed_bytes ?? 0) / 1_073_741_824).toFixed(2);
  const count = ev.result?.processes_trimmed ?? 0;

  // FIX: show per-process breakdown using new per_process field
  const perProc = ev.result?.per_process ?? [];
  let perProcHtml = '';
  if (perProc.length > 0) {
    perProcHtml = perProc
      .sort((a, b) => b.freed_bytes - a.freed_bytes)
      .map(p => {
        const mb = (p.freed_bytes / (1024 * 1024)).toFixed(1);
        return `<div class="history-expand-item"><span>${escHtml(p.name)}</span><span>${mb} MB</span></div>`;
      })
      .join('');
  } else {
    perProcHtml = `<div class="history-expand-item" style="color:var(--text-secondary);font-size:10px">
      <span>Per-process breakdown not available for this entry</span><span></span>
    </div>`;
  }

  expand.innerHTML = `
    <div class="history-expand-item">
      <span>Total freed</span><span>${gb} GB</span>
    </div>
    <div class="history-expand-item">
      <span>Processes trimmed</span><span>${count}</span>
    </div>
    ${perProcHtml}
  `;
  rowEl.insertAdjacentElement('afterend', expand);
}

// ─────────────────────────────────────────────────────────────
// SETTINGS TAB
// ─────────────────────────────────────────────────────────────

async function loadConfig() {
  try {
    _config = await invoke('get_config');
    populateSettings(_config);
  } catch (e) {
    console.warn('get_config failed:', e);
  }
}

function populateSettings(cfg) {
  if (!cfg) return;

  document.getElementById('cfg-auto-trim-enabled').checked = cfg.auto_trim_enabled ?? true;
  document.getElementById('cfg-threshold').value           = String(cfg.auto_trim_threshold_pct ?? 80);
  document.getElementById('cfg-idle-time').value           = String(cfg.idle_threshold_secs ?? 600);
  document.getElementById('cfg-hard-suspend').checked      = cfg.hard_suspend_enabled ?? false;
  document.getElementById('cfg-start-on-login').checked    = cfg.start_on_login ?? true;
  document.getElementById('cfg-notifications').checked     = cfg.notifications_enabled ?? true;
  document.getElementById('cfg-theme').value               = cfg.theme ?? 'system';

  // Apply theme
  applyTheme(cfg.theme ?? 'system');

  renderExclusionChips();
}

function bindSettings() {
  // Auto-trim enabled
  document.getElementById('cfg-auto-trim-enabled').addEventListener('change', async e => {
    await saveConfig({ auto_trim_enabled: e.target.checked });
  });

  // Threshold
  document.getElementById('cfg-threshold').addEventListener('change', async e => {
    await saveConfig({ auto_trim_threshold_pct: parseInt(e.target.value, 10) });
  });

  // Idle time
  document.getElementById('cfg-idle-time').addEventListener('change', async e => {
    await saveConfig({ idle_threshold_secs: parseInt(e.target.value, 10) });
  });

  // Hard suspend — requires admin + NtSuspendProcess availability
  document.getElementById('cfg-hard-suspend').addEventListener('change', async e => {
    if (e.target.checked) {
      // FIX: verify NtSuspendProcess is loadable before enabling
      let capable = false;
      try { capable = await invoke('verify_suspend_capable'); } catch (_) {}
      if (!capable) {
        e.target.checked = false;
        showAdminWarning();
        return;
      }
    }
    await saveConfig({ hard_suspend_enabled: e.target.checked });
  });

  // Start on login
  document.getElementById('cfg-start-on-login').addEventListener('change', async e => {
    await saveConfig({ start_on_login: e.target.checked });
  });

  // Notifications
  document.getElementById('cfg-notifications').addEventListener('change', async e => {
    await saveConfig({ notifications_enabled: e.target.checked });
  });

  // Theme — FIX: emit config-changed so popup window also re-applies the new theme
  document.getElementById('cfg-theme').addEventListener('change', async e => {
    const theme = e.target.value;
    await saveConfig({ theme });
    applyTheme(theme);
    try {
      await window.__TAURI__.event.emit('gingify://config-changed', { theme });
    } catch (_) {}
  });

  // Exclusion: Add button
  document.getElementById('exclusion-add-btn').addEventListener('click', async () => {
    const input  = document.getElementById('exclusion-input');
    const name   = input.value.trim();
    if (!name) return;
    try {
      await invoke('add_exclusion', { name });
      input.value = '';
      await loadConfig();
      renderExclusionChips();
    } catch (e) {
      console.warn('add_exclusion failed:', e);
    }
  });

  // Exclusion: Enter key
  document.getElementById('exclusion-input').addEventListener('keydown', async e => {
    if (e.key === 'Enter') document.getElementById('exclusion-add-btn').click();
  });

  // Check for updates — show "Checking…" feedback and inline result
  const checkUpdatesBtn = document.getElementById('about-check-updates');
  if (checkUpdatesBtn) {
    checkUpdatesBtn.addEventListener('click', async () => {
      const badge = document.getElementById('about-update-badge');
      const originalText = checkUpdatesBtn.textContent;
      checkUpdatesBtn.textContent = 'Checking…';
      checkUpdatesBtn.disabled = true;

      let timer = null;
      let unlistenUpdate = null;

      const cleanup = () => {
        if (timer) { clearTimeout(timer); timer = null; }
        if (unlistenUpdate) { unlistenUpdate(); unlistenUpdate = null; }
        checkUpdatesBtn.textContent = originalText;
        checkUpdatesBtn.disabled = false;
      };

      try {
        await invoke('check_for_updates_cmd');

        // Listen for update-available event using already-imported listen()
        try {
          unlistenUpdate = await listen('gingify://update-available', info => {
            cleanup();
            if (badge) {
              badge.textContent = `v${info.payload} available`;
              badge.classList.remove('hidden');
            }
          });
        } catch (_) { /* listener setup failed — timer handles "up to date" */ }

        // If no update-available event fires within 5s, show "up to date"
        timer = setTimeout(() => {
          cleanup();
          if (badge) {
            badge.textContent = "You're on the latest version";
            badge.classList.remove('hidden');
            setTimeout(() => { badge.textContent = ''; badge.classList.add('hidden'); }, 3000);
          }
        }, 5000);

      } catch (e) {
        cleanup();
        if (badge) {
          badge.textContent = "Couldn't check — verify your internet connection";
          badge.classList.remove('hidden');
          setTimeout(() => { badge.textContent = ''; badge.classList.add('hidden'); }, 4000);
        }
        console.warn('check_for_updates_cmd failed:', e);
      }
    });
  }

  // GitHub link — open in system browser via IPC (shell plugin Rust side)
  const githubLink = document.getElementById('about-github');
  if (githubLink) {
    githubLink.addEventListener('click', async (e) => {
      e.preventDefault();
      try {
        await invoke('open_url_cmd', { url: 'https://github.com/IshekKhal/gingify' });
      } catch (_) {}
    });
  }

  // FIX: Welcome Screen button removed from HTML — no handler needed

  // Quit
  document.getElementById('about-quit').addEventListener('click', () => {
    invoke('quit_app').catch(() => {});
  });
}

function renderExclusionChips() {
  const chipsEl  = document.getElementById('exclusion-chips');
  chipsEl.innerHTML = '';
  const excluded = _config?.excluded_processes ?? [];

  if (excluded.length === 0) {
    chipsEl.innerHTML = '<span style="font-size:11px;color:var(--text-secondary)">None</span>';
    return;
  }

  for (const procName of excluded) {
    const chip = document.createElement('span');
    chip.className = 'exclusion-chip';
    chip.innerHTML = `${escHtml(procName)}<button class="exclusion-chip-remove" title="Remove" data-name="${escHtml(procName)}">×</button>`;
    chip.querySelector('.exclusion-chip-remove').addEventListener('click', async () => {
      try {
        await invoke('remove_exclusion', { name: procName });
        await loadConfig();
        renderExclusionChips();
        // Refresh process table so row is no longer marked excluded
        renderProcessTable();
      } catch (e) {
        console.warn('remove_exclusion failed:', e);
      }
    });
    chipsEl.appendChild(chip);
  }
}

/**
 * Show a brief inline warning under the hard-suspend row when the user
 * tries to enable Gaming Mode without admin privileges.
 */
function showAdminWarning() {
  const toggle = document.getElementById('cfg-hard-suspend');
  const row    = toggle.closest('.settings-row');
  let msg = document.getElementById('hard-suspend-admin-msg');
  if (!msg) {
    msg = document.createElement('div');
    msg.id = 'hard-suspend-admin-msg';
    msg.style.cssText = 'color:var(--red,#e05252);font-size:11px;margin-top:4px;grid-column:1/-1;';
    msg.textContent   = 'Gingify needs to run as administrator to enable Gaming Mode. Restart it with "Run as administrator".';
    row.insertAdjacentElement('afterend', msg);
  }
  msg.style.display = 'block';
  setTimeout(() => { msg.style.display = 'none'; }, 6000);
}

async function saveConfig(partial) {
  try {
    await invoke('update_config', { partialJson: JSON.stringify(partial) });
    // Refresh local config cache
    _config = await invoke('get_config');
  } catch (e) {
    console.warn('update_config failed:', e);
  }
}

function applyTheme(theme) {
  const body = document.body;
  body.classList.remove('theme-dark', 'theme-light');
  if (theme === 'dark')  body.classList.add('theme-dark');
  if (theme === 'light') body.classList.add('theme-light');
}

/**
 * Append an inline error element to containerEl.
 * Flow 15 (UIUX_DESKTOP.md) — no icon, no animation.
 *
 * @param {Element} containerEl  - Parent element to append error into.
 * @param {string}  message      - Short user-facing error text.
 * @param {string}  [tooltip]    - Optional [?] tooltip text on hover.
 * @param {boolean} [isTemporary=true] - Auto-removes after 4 s when true.
 */
function showInlineError(containerEl, message, tooltip = '', isTemporary = true) {
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

// ─────────────────────────────────────────────────────────────
// UTILITIES
// ─────────────────────────────────────────────────────────────

/** Format idle seconds into a human-readable string. */
function formatIdle(secs) {
  if (!secs || secs < 60) return 'Active';
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h} hr ${m} min`;
  return `${m} min`;
}

/** Generate a consistent, per-name HSL color. */
function nameToColor(name) {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) & 0xffffffff;
  return `hsl(${Math.abs(hash) % 360}, 55%, 45%)`;
}

/** Map a TrimTrigger enum value to a display label. */
function triggerLabel(t) {
  if (t === 'Auto')       return 'Auto-snooze';
  if (t === 'GamingMode') return 'Gaming Mode';
  return 'Snooze';
}

/** Return a stable day string (YYYY-MM-DD) for grouping. */
function dayLabel(date) {
  return date.toISOString().slice(0, 10);
}

/** Escape HTML entities. */
function escHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/** Make a string safe for use in an element ID attribute. */
function encodeId(str) {
  return encodeURIComponent(str).replace(/%/g, '_');
}
