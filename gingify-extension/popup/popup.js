// ── Helpers ──────────────────────────────────────────────────────────────────

function getDomain(url) {
    try { return new URL(url).hostname; } catch (_) { return url || ''; }
}

const SLEEP_PAGE_BASE = chrome.runtime.getURL('sleep.html');

async function fetchState() {
    return new Promise((resolve) => {
        chrome.runtime.sendMessage({ type: 'GET_STATE' }, resolve);
    });
}

async function fetchTabs() {
    return chrome.tabs.query({});
}

// ── Data ──────────────────────────────────────────────────────────────────────

function getTabsWithState(state, allTabs) {
    return allTabs.map(tab => ({
        id: tab.id,
        title: tab.title || getDomain(tab.url),
        url: tab.url,
        favIconUrl: tab.favIconUrl || '',
        active: tab.active,
        pinned: tab.pinned,
        windowId: tab.windowId,
        incognito: tab.incognito,
        isSleeping: !!(state.sleeping_tabs && state.sleeping_tabs[tab.id]) ||
                    !!(tab.url && tab.url.startsWith(SLEEP_PAGE_BASE)),
        ramEstimateMb: (state.tab_ram_estimate && state.tab_ram_estimate[tab.id]) || null,
    }));
}

function sortTabs(tabs) {
    return [...tabs].sort((a, b) => {
        // Active tab always first (within its window — handled per-window below)
        if (a.active && !b.active) return -1;
        if (!a.active && b.active) return 1;
        // Sleeping tabs last
        if (a.isSleeping && !b.isSleeping) return 1;
        if (!a.isSleeping && b.isSleeping) return -1;
        // Then by RAM estimate descending
        const ra = a.ramEstimateMb || 0;
        const rb = b.ramEstimateMb || 0;
        return rb - ra;
    });
}

// ── Render ────────────────────────────────────────────────────────────────────

function renderRamBar(ram_stats) {
    const pct = ram_stats ? ram_stats.pressure_pct : 0;
    const fill = document.getElementById('ram-bar-fill');
    fill.style.width = pct + '%';

    if (pct >= 85) {
        fill.style.backgroundColor = '#C62828';
    } else if (pct >= 60) {
        fill.style.backgroundColor = '#E65100';
    } else {
        fill.style.backgroundColor = '#2E7D32';
    }

    const total = ram_stats ? ram_stats.total_mb : 0;
    const avail = ram_stats ? ram_stats.available_mb : 0;
    document.getElementById('ram-status').textContent =
        total > 0
            ? `${pct}% used · ${Math.round(avail / 1024)} GB free of ${Math.round(total / 1024)} GB`
            : '';
}

function renderTabRow(tab) {
    const row = document.createElement('div');
    row.className = 'tab-row' +
        (tab.active ? ' active' : '') +
        (tab.isSleeping ? ' sleeping' : '');

    // Favicon
    const favicon = document.createElement('img');
    favicon.className = 'tab-favicon';
    favicon.alt = '';
    if (tab.favIconUrl) {
        favicon.src = tab.favIconUrl;
        favicon.onerror = () => { favicon.style.visibility = 'hidden'; };
    } else {
        favicon.style.visibility = 'hidden';
    }

    // Title
    const title = document.createElement('span');
    title.className = 'tab-title';
    title.textContent = tab.title;
    title.title = tab.title;

    // RAM estimate
    const ram = document.createElement('span');
    ram.className = 'tab-ram';
    ram.textContent = tab.ramEstimateMb ? `~${tab.ramEstimateMb} MB` : '';

    // Action button
    const btn = document.createElement('button');
    btn.className = 'tab-btn';

    if (tab.isSleeping) {
        btn.textContent = 'Wake';
        btn.addEventListener('click', (e) => {
            e.stopPropagation();
            doWakeTab(tab.id);
        });
    } else if (tab.active) {
        btn.textContent = '●';
        btn.disabled = true;
        btn.title = 'Active tab';
    } else if (tab.pinned) {
        btn.textContent = '—';
        btn.disabled = true;
        btn.title = 'Pinned — will not snooze';
    } else if (tab.incognito) {
        btn.textContent = '—';
        btn.disabled = true;
        btn.title = 'Incognito tab — will not snooze';
    } else {
        btn.textContent = 'Snooze';
        btn.addEventListener('click', (e) => {
            e.stopPropagation();
            doSleepTab(tab.id);
        });
    }

    row.append(favicon, title, ram, btn);
    return row;
}

function renderPopup(state, allTabs) {
    // RAM bar
    renderRamBar(state.ram_stats);

    const tabs = getTabsWithState(state, allTabs);

    // Section label
    const totalRamMb = tabs.reduce((s, t) => s + (t.ramEstimateMb || 0), 0);
    document.getElementById('tab-section-label').textContent =
        `Open Tabs (${tabs.length} tab${tabs.length !== 1 ? 's' : ''}` +
        (totalRamMb > 0 ? ` · ~${totalRamMb} MB` : '') + ')';

    // Tab list
    const list = document.getElementById('tab-list');
    list.innerHTML = '';

    // Group by window
    const windowIds = [...new Set(tabs.map(t => t.windowId))];
    const multiWindow = windowIds.length > 1;

    for (let wi = 0; wi < windowIds.length; wi++) {
        const wid = windowIds[wi];
        const windowTabs = sortTabs(tabs.filter(t => t.windowId === wid));

        if (multiWindow) {
            const header = document.createElement('div');
            header.className = 'window-header';
            header.textContent = `Window ${wi + 1}`;
            list.appendChild(header);
        }

        for (const tab of windowTabs) {
            list.appendChild(renderTabRow(tab));
        }
    }

    // Footer
    const sleepingCount = Object.keys(state.sleeping_tabs || {}).length;
    const savedMb = Object.values(state.sleeping_tabs || {})
        .reduce((s, t) => s + (t.ram_estimate_mb || 0), 0);

    let footerText = sleepingCount > 0
        ? `${sleepingCount} tab${sleepingCount !== 1 ? 's' : ''} snoozed`
        : 'No tabs snoozed';
    if (savedMb > 0) footerText += ` · Saved ~${Math.round(savedMb)} MB`;
    document.getElementById('footer').textContent = footerText;
}

// ── Actions ───────────────────────────────────────────────────────────────────

async function refresh() {
    let state, allTabs;
    try {
        [state, allTabs] = await Promise.all([fetchState(), fetchTabs()]);
    } catch (_) {
        const list = document.getElementById('tab-list');
        list.innerHTML = '';
        const errDiv = document.createElement('div');
        errDiv.className = 'permission-error';
        const msg = document.createElement('p');
        msg.textContent = 'Gingify lost access to your tabs.';
        const btn = document.createElement('button');
        btn.id = 'restore-perms-btn';
        btn.textContent = 'Restore permissions';
        btn.addEventListener('click', () => {
            chrome.tabs.create({ url: 'chrome://extensions/?id=' + chrome.runtime.id });
        });
        errDiv.append(msg, btn);
        list.appendChild(errDiv);
        return;
    }
    renderPopup(state, allTabs);
}

async function doSleepTab(tabId) {
    await new Promise(resolve =>
        chrome.runtime.sendMessage({ type: 'SLEEP_TAB', tabId }, resolve)
    );
    await refresh();
}

async function doWakeTab(tabId) {
    await new Promise(resolve =>
        chrome.runtime.sendMessage({ type: 'WAKE_TAB', tabId }, resolve)
    );
    await refresh();
}

// ── Init ──────────────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', async () => {
    await refresh();

    document.getElementById('sleep-all-btn').addEventListener('click', async () => {
        await new Promise(resolve =>
            chrome.runtime.sendMessage({ type: 'SLEEP_ALL' }, resolve)
        );
        await refresh();
    });

    document.getElementById('wake-all-btn').addEventListener('click', async () => {
        await new Promise(resolve =>
            chrome.runtime.sendMessage({ type: 'WAKE_ALL' }, resolve)
        );
        await refresh();
    });

    document.getElementById('settings-btn').addEventListener('click', () => {
        chrome.runtime.openOptionsPage();
    });

    // Auto-refresh when session state or tabs change. Debounced so bursts of
    // events (Snooze All fires one tab event per tab) collapse into one render.
    let refreshPending = false;
    function scheduleRefresh() {
        if (refreshPending) return;
        refreshPending = true;
        setTimeout(() => { refreshPending = false; refresh(); }, 50);
    }
    chrome.storage.onChanged.addListener((changes, area) => {
        if (area !== 'session') return;
        if (changes.sleeping_tabs || changes.tab_ram_estimate) scheduleRefresh();
    });
    chrome.tabs.onUpdated.addListener((_tabId, changeInfo) => {
        if (changeInfo.status === 'complete' || changeInfo.title || changeInfo.favIconUrl || changeInfo.url) {
            scheduleRefresh();
        }
    });
    chrome.tabs.onRemoved.addListener(scheduleRefresh);
    chrome.tabs.onCreated.addListener(scheduleRefresh);
});
