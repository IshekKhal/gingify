/**
 * onboarding.js — First-launch onboarding screen logic.
 *
 * Loads live RAM + process data via IPC, populates the headline, and
 * wires the [Get Started] button to persist config and hide this window.
 */

/* ------------------------------------------------------------------ */
/* Tauri IPC helpers (withGlobalTauri = true in tauri.conf.json)       */
/* ------------------------------------------------------------------ */
const { invoke } = window.__TAURI__.core;

/* ------------------------------------------------------------------ */
/* Headline population                                                  */
/* ------------------------------------------------------------------ */

/**
 * Fill the headline element with live system values.
 * The IPC `get_ram_stats` response uses total_mb / used_mb (not bytes).
 * Retries up to 6x with 1s delay in case the monitor hasn't polled yet.
 */
async function populateHeadline() {
  const headlineEl = document.getElementById('headline');

  try {
    // Retry loop — monitor polls every 5s; on first launch total_mb may be 0
    let ram = null;
    for (let attempt = 0; attempt < 6; attempt++) {
      ram = await invoke('get_ram_stats');
      if (ram.total_mb > 0) break;
      await new Promise(r => setTimeout(r, 1000));
    }

    // total_mb / used_mb come from the Rust RamStats struct
    const totalGb = (ram.total_mb / 1024).toFixed(0);
    const usedGb  = (ram.used_mb  / 1024).toFixed(1);

    // 2. Process count
    const processes = await invoke('get_process_list');
    const processCount = processes.length || '...';

    // Build headline HTML with <strong> around the live numbers
    headlineEl.innerHTML =
      `Your PC has <strong>${totalGb} GB</strong> of RAM.<br />` +
      `Right now, <strong>${usedGb} GB</strong> is being used.<br />` +
      `<strong>${processCount}</strong> apps are running in the background.`;
  } catch (err) {
    // Graceful fallback — show the UI even if IPC fails
    headlineEl.textContent =
      'Gingify will monitor your apps and free RAM automatically.';
    console.error('[onboarding] Failed to load system stats:', err);
  }
}

/* ------------------------------------------------------------------ */
/* Hide the onboarding window with a hard 3s fallback — if the IPC     */
/* silently hangs (a Win7 edge case), we force-close via the WebView   */
/* so the user is never stuck staring at a dead window.                */
/* ------------------------------------------------------------------ */
async function hideOnboardingSafely() {
  try {
    await Promise.race([
      invoke('hide_window', { label: 'onboarding' }),
      new Promise((_, rej) => setTimeout(() => rej(new Error('hide_window timeout')), 3000)),
    ]);
  } catch (e) {
    console.warn('[onboarding] hide_window failed, forcing close:', e);
    window.close();
  }
}

/* ------------------------------------------------------------------ */
/* Get Started — persist config, hide window, fire welcome toast       */
/* ------------------------------------------------------------------ */

document.getElementById('get-started-btn').addEventListener('click', async () => {
  const btn = document.getElementById('get-started-btn');
  btn.disabled = true;
  btn.textContent = 'Starting…';

  try {
    const selectedValue = document.getElementById('threshold-select').value;

    // 1. Save the chosen threshold to config
    await invoke('update_config', {
      partialJson: JSON.stringify({ auto_trim_threshold_pct: parseInt(selectedValue, 10) }),
    });

    // 2. Mark onboarding as complete so it never shows again
    await invoke('update_config', {
      partialJson: JSON.stringify({ first_launch_complete: true }),
    });

    // 3. Fire the welcome toast notification
    await invoke('trigger_welcome_notification');

    // 4. Hide this window
    hideOnboardingSafely();
  } catch (err) {
    console.error('[onboarding] Get Started failed:', err);
    // Re-enable the button so the user can try again
    btn.disabled = false;
    btn.textContent = 'Get Started';
  }
});

/* ------------------------------------------------------------------ */
/* Close button + Escape key                                           */
/* ------------------------------------------------------------------ */

document.getElementById('close-btn').addEventListener('click', () => {
  hideOnboardingSafely();
});

document.addEventListener('keydown', e => {
  if (e.key === 'Escape') hideOnboardingSafely();
});

/* ------------------------------------------------------------------ */
/* Theme                                                                */
/* ------------------------------------------------------------------ */

async function applyTheme() {
  try {
    const cfg = await invoke('get_config');
    const theme = cfg.theme ?? 'system';
    document.body.classList.remove('theme-light', 'theme-dark');
    if (theme === 'light') document.body.classList.add('theme-light');
    if (theme === 'dark')  document.body.classList.add('theme-dark');
  } catch (_) {}
}

/* ------------------------------------------------------------------ */
/* Init                                                                 */
/* ------------------------------------------------------------------ */
applyTheme();
populateHeadline();
