import { STORAGE_KEYS, DEFAULT_SETTINGS, DEFAULT_RULES } from './storage_keys.js';

// Re-register alarm every time the service worker starts (MV3 workers restart on browser restart)
onServiceWorkerStart();

// ─── Helpers ────────────────────────────────────────────────────────────────

async function getSettings() {
    const data = await chrome.storage.sync.get(STORAGE_KEYS.SETTINGS);
    return data[STORAGE_KEYS.SETTINGS] || DEFAULT_SETTINGS;
}

async function getRules() {
    const data = await chrome.storage.sync.get(STORAGE_KEYS.RULES);
    return data[STORAGE_KEYS.RULES] || DEFAULT_RULES;
}

async function getSleepingTabs() {
    const data = await chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS);
    return data[STORAGE_KEYS.SLEEPING_TABS] || {};
}

async function getTabLastActive() {
    const data = await chrome.storage.session.get(STORAGE_KEYS.TAB_LAST_ACTIVE);
    return data[STORAGE_KEYS.TAB_LAST_ACTIVE] || {};
}

function getDomain(url) {
    try { return new URL(url).hostname; } catch (_) { return ''; }
}

function isInternalUrl(url) {
    if (!url) return true;
    return url.startsWith('chrome://') ||
           url.startsWith('chrome-extension://') ||
           url.startsWith('about:') ||
           url.startsWith('edge://') ||
           url.startsWith('devtools://');
}

function getSleepPageUrl(tabId) {
    return chrome.runtime.getURL('sleep.html') + '?tabId=' + tabId;
}

function isSleepPage(url) {
    return url && url.startsWith(chrome.runtime.getURL('sleep.html'));
}

function getMemoryInfo() {
    return new Promise((resolve) => {
        chrome.system.memory.getInfo((info) => {
            const total_mb = Math.round(info.capacity / (1024 * 1024));
            const available_mb = Math.round(info.availableCapacity / (1024 * 1024));
            const pressure_pct = Math.round((1 - info.availableCapacity / info.capacity) * 100);
            resolve({ total_mb, available_mb, pressure_pct });
        });
    });
}

// ─── On Install ─────────────────────────────────────────────────────────────

chrome.runtime.onInstalled.addListener(async (details) => {
    if (details.reason === 'install') {
        await initializeStorage();
        chrome.tabs.create({ url: chrome.runtime.getURL('welcome.html') });
    }
    // Ensure alarm exists after install or update
    ensureAutoSleepAlarm();
    // Recreate context menus (removeAll prevents duplicate-ID errors on update)
    chrome.contextMenus.removeAll(() => {
        chrome.contextMenus.create({
            id: 'sleep-tab',
            title: 'Snooze this tab',
            contexts: ['action'],
        });
        chrome.contextMenus.create({
            id: 'sleep-page',
            title: 'Snooze this tab',
            contexts: ['page'],
        });
        chrome.contextMenus.create({
            id: 'never-sleep-domain',
            title: 'Never snooze this domain',
            contexts: ['page'],
        });
    });
});

// ─── Context Menu Click Handler ──────────────────────────────────────────────

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
    if (!tab || !tab.id) return;
    if (info.menuItemId === 'sleep-tab' || info.menuItemId === 'sleep-page') {
        await sleepTab(tab.id);
    } else if (info.menuItemId === 'never-sleep-domain') {
        const domain = getDomain(tab.url);
        if (!domain) return;
        // Quota guard — silent no-op if storage is nearly full
        const bytes = await new Promise(r => chrome.storage.sync.getBytesInUse(null, r));
        if (bytes / chrome.storage.sync.QUOTA_BYTES > 0.95) return;
        const rules = await getRules();
        if (rules.some(r => r.domain === domain)) return;
        rules.push({ domain, action: 'never' });
        await chrome.storage.sync.set({ [STORAGE_KEYS.RULES]: rules });
    }
});

async function initializeStorage() {
    const existingSettings = await chrome.storage.sync.get(STORAGE_KEYS.SETTINGS);
    if (!existingSettings[STORAGE_KEYS.SETTINGS]) {
        await chrome.storage.sync.set({ [STORAGE_KEYS.SETTINGS]: DEFAULT_SETTINGS });
    }
    const existingRules = await chrome.storage.sync.get(STORAGE_KEYS.RULES);
    if (!existingRules[STORAGE_KEYS.RULES]) {
        await chrome.storage.sync.set({ [STORAGE_KEYS.RULES]: DEFAULT_RULES });
    }
}

// ─── Service Worker Startup ──────────────────────────────────────────────────

async function onServiceWorkerStart() {
    await ensureAutoSleepAlarm();
    await cleanupOrphanedSleepTabs();
}

async function ensureAutoSleepAlarm() {
    const existingAlarms = await chrome.alarms.getAll();
    const hasAutoSleep = existingAlarms.some(a => a.name === 'auto-sleep-check');
    if (!hasAutoSleep) {
        chrome.alarms.create('auto-sleep-check', { periodInMinutes: 1 });
    }
}

async function cleanupOrphanedSleepTabs() {
    // After a browser restart, chrome.storage.session is cleared.
    // Tabs still showing sleep.html with no entry in sleeping_tabs are orphaned — close them.
    const sleepData = await chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS);
    const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};
    const allTabs = await chrome.tabs.query({});
    for (const tab of allTabs) {
        if (isSleepPage(tab.url) && !(tab.id in sleeping)) {
            try { await chrome.tabs.remove(tab.id); } catch (_) {}
        }
    }
}

// ─── Tab Last-Active Tracking ────────────────────────────────────────────────

chrome.tabs.onActivated.addListener(async (activeInfo) => {
    // Snoozed tabs must stay on sleep.html until the user clicks inside the
    // page or hits Wake in the popup. Auto-waking on activation nullifies the
    // memory savings, so we only touch last-active for non-sleeping tabs.
    const sleepingTabs = await getSleepingTabs();
    if (activeInfo.tabId in sleepingTabs) return;
    await updateTabLastActive(activeInfo.tabId);
});

chrome.tabs.onUpdated.addListener(async (tabId, changeInfo, tab) => {
    if (changeInfo.status !== 'complete') return;

    // Step 2 of the sleep sequence: about:blank has loaded, now navigate to sleep.html.
    // This two-step route evicts the old page from bfcache before sleep.html loads.
    const pendingData = await chrome.storage.session.get('pending_sleep');
    const pending = pendingData['pending_sleep'] || {};
    if (tabId in pending && tab.url === 'about:blank') {
        delete pending[tabId];
        await chrome.storage.session.set({ 'pending_sleep': pending });
        await chrome.tabs.update(tabId, { url: getSleepPageUrl(tabId) });
        return;
    }

    if (!isSleepPage(tab.url)) {
        await updateTabLastActive(tabId);
    }
});

async function updateTabLastActive(tabId) {
    const data = await chrome.storage.session.get(STORAGE_KEYS.TAB_LAST_ACTIVE);
    const map = data[STORAGE_KEYS.TAB_LAST_ACTIVE] || {};
    map[tabId] = Date.now();
    await chrome.storage.session.set({ [STORAGE_KEYS.TAB_LAST_ACTIVE]: map });
}

// ─── Tab Removal Cleanup ─────────────────────────────────────────────────────

chrome.tabs.onRemoved.addListener(async (tabId) => {
    await removeTabFromState(tabId);
});

chrome.tabs.onCreated.addListener(async (tab) => {
    // Stamp newly created tabs so they count from their open time for auto-sleep.
    // Without this, tabs opened in background are never tracked and get snoozed
    // immediately on the first auto-sleep check.
    if (tab.id) await updateTabLastActive(tab.id);
});

async function removeTabFromState(tabId) {
    // Remove from last_active map
    const activeData = await chrome.storage.session.get(STORAGE_KEYS.TAB_LAST_ACTIVE);
    const activeMap = activeData[STORAGE_KEYS.TAB_LAST_ACTIVE] || {};
    if (tabId in activeMap) {
        delete activeMap[tabId];
        await chrome.storage.session.set({ [STORAGE_KEYS.TAB_LAST_ACTIVE]: activeMap });
    }

    // Remove from sleeping_tabs if it was sleeping
    const sleepData = await chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS);
    const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};
    if (tabId in sleeping) {
        delete sleeping[tabId];
        await chrome.storage.session.set({ [STORAGE_KEYS.SLEEPING_TABS]: sleeping });
        await updateBadge();
    }

    // Remove from RAM estimate map
    const ramData = await chrome.storage.session.get(STORAGE_KEYS.TAB_RAM_ESTIMATE);
    const ramMap = ramData[STORAGE_KEYS.TAB_RAM_ESTIMATE] || {};
    if (tabId in ramMap) {
        delete ramMap[tabId];
        await chrome.storage.session.set({ [STORAGE_KEYS.TAB_RAM_ESTIMATE]: ramMap });
    }
}

// ─── Alarm Listener ──────────────────────────────────────────────────────────

chrome.alarms.onAlarm.addListener(async (alarm) => {
    if (alarm.name === 'auto-sleep-check') {
        await runAutoSleepCheck();
    } else if (alarm.name === 'sleep-on-minimize-pending') {
        await handleSleepOnMinimizeAlarm();
    }
});

// ─── Auto-Sleep Check ────────────────────────────────────────────────────────

async function runAutoSleepCheck() {
    const settings = await getSettings();
    if (!settings.auto_sleep_enabled) return;

    const [allTabs, activeData, sleepData, rules, ramEstData] = await Promise.all([
        chrome.tabs.query({}),
        chrome.storage.session.get(STORAGE_KEYS.TAB_LAST_ACTIVE),
        chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS),
        getRules(),
        chrome.storage.session.get(STORAGE_KEYS.TAB_RAM_ESTIMATE),
    ]);

    const lastActiveMap = activeData[STORAGE_KEYS.TAB_LAST_ACTIVE] || {};
    const sleepingTabs = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};
    const ramEstMap = ramEstData[STORAGE_KEYS.TAB_RAM_ESTIMATE] || {};
    // Ensure auto_sleep_after_mins is set; fall back to default if missing
    const autoSleepMins = settings.auto_sleep_after_mins ?? DEFAULT_SETTINGS.auto_sleep_after_mins;
    const thresholdMs = autoSleepMins * 60 * 1000;
    const now = Date.now();

    // Build a set of active tab IDs per window
    const activeTabIds = new Set(allTabs.filter(t => t.active).map(t => t.id));

    let sleptCount = 0;
    let savedMb = 0;

    for (const tab of allTabs) {
        if (!tab.id) continue;

        // Skip incognito tabs
        if (tab.incognito) continue;

        // Skip active tabs
        if (activeTabIds.has(tab.id)) continue;

        // Skip pinned tabs (unless setting allows it)
        if (tab.pinned && !settings.sleep_pinned_tabs) continue;

        // Skip already sleeping tabs
        if (tab.id in sleepingTabs) continue;

        // Skip internal/extension URLs
        if (isInternalUrl(tab.url)) continue;

        // Skip sleep page itself
        if (isSleepPage(tab.url)) continue;

        const domain = getDomain(tab.url);

        // Check domain rules — match exact domain or subdomains
        const rule = rules.find(r => domain === r.domain || domain.endsWith('.' + r.domain));
        if (rule && rule.action === 'never') continue;

        // Determine effective threshold
        const effectiveThresholdMs = (rule && rule.action === 'sleep' && rule.after_mins)
            ? rule.after_mins * 60 * 1000
            : thresholdMs;

        const lastActive = lastActiveMap[tab.id];
        // Skip tabs that have never been tracked — they'll be stamped when first opened
        // or focused, then counted from that point. Fixes: untracked tabs snoozed immediately.
        if (lastActive === undefined) continue;
        if ((now - lastActive) >= effectiveThresholdMs) {
            savedMb += ramEstMap[tab.id] || 0;
            await sleepTab(tab.id);
            sleptCount++;
        }
    }

    if (sleptCount > 0) {
        notifyTabsSlept(sleptCount, savedMb);
    }
}

// ─── Sleep / Wake ────────────────────────────────────────────────────────────

async function sleepTab(tabId) {
    let tab;
    try {
        tab = await chrome.tabs.get(tabId);
    } catch (_) {
        return; // Tab no longer exists
    }

    if (tab.incognito) return;
    if (!tab.url || isSleepPage(tab.url) || isInternalUrl(tab.url)) return;

    // Record sleeping state before navigating
    const sleepData = await chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS);
    const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};

    const ramData = await chrome.storage.session.get(STORAGE_KEYS.TAB_RAM_ESTIMATE);
    const ramMap = ramData[STORAGE_KEYS.TAB_RAM_ESTIMATE] || {};

    sleeping[tabId] = {
        original_url: tab.url,
        title: tab.title || '',
        favicon_url: tab.favIconUrl || '',
        slept_at: Date.now(),
        ram_estimate_mb: ramMap[tabId] || 0,
    };

    await chrome.storage.session.set({ [STORAGE_KEYS.SLEEPING_TABS]: sleeping });

    // Navigate to about:blank first to evict the old page from Chrome's bfcache.
    // bfcache keeps the previous renderer alive in memory after navigation;
    // routing through about:blank breaks the cache chain before we load sleep.html.
    const pendingData = await chrome.storage.session.get('pending_sleep');
    const pending = pendingData['pending_sleep'] || {};
    pending[tabId] = true;
    await chrome.storage.session.set({ 'pending_sleep': pending });

    await chrome.tabs.update(tabId, { url: 'about:blank' });
    // onUpdated will detect the pending state and navigate to sleep.html.

    await updateBadge();
}

async function wakeTab(tabId) {
    const sleepData = await chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS);
    const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};

    const info = sleeping[tabId];
    if (!info) return; // Not a sleeping tab

    // Navigate back to original URL
    try {
        await chrome.tabs.update(tabId, { url: info.original_url });
    } catch (_) {
        // Tab may have been closed; just clean up state
    }

    // Remove from sleeping state
    delete sleeping[tabId];
    await chrome.storage.session.set({ [STORAGE_KEYS.SLEEPING_TABS]: sleeping });

    // Remove stale last_active entry so it gets re-stamped on load
    const activeData = await chrome.storage.session.get(STORAGE_KEYS.TAB_LAST_ACTIVE);
    const activeMap = activeData[STORAGE_KEYS.TAB_LAST_ACTIVE] || {};
    delete activeMap[tabId];
    await chrome.storage.session.set({ [STORAGE_KEYS.TAB_LAST_ACTIVE]: activeMap });

    await updateBadge();
}

async function sleepAllTabs() {
    // Batched: build the full new state in memory, write once, then fire all
    // navigations in parallel. Calling sleepTab() in a loop with await races on
    // chrome.storage reads/writes and serialises the tab transitions.
    const [allTabs, sleepData, ramData, pendingData] = await Promise.all([
        chrome.tabs.query({}),
        chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS),
        chrome.storage.session.get(STORAGE_KEYS.TAB_RAM_ESTIMATE),
        chrome.storage.session.get('pending_sleep'),
    ]);
    const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};
    const ramMap = ramData[STORAGE_KEYS.TAB_RAM_ESTIMATE] || {};
    const pending = pendingData['pending_sleep'] || {};
    const activeTabIds = new Set(allTabs.filter(t => t.active).map(t => t.id));

    const toSleep = [];
    for (const tab of allTabs) {
        if (!tab.id) continue;
        if (tab.incognito) continue;
        if (activeTabIds.has(tab.id)) continue;
        if (tab.pinned) continue;
        if (tab.id in sleeping) continue;
        if (!tab.url || isInternalUrl(tab.url) || isSleepPage(tab.url)) continue;
        sleeping[tab.id] = {
            original_url: tab.url,
            title: tab.title || '',
            favicon_url: tab.favIconUrl || '',
            slept_at: Date.now(),
            ram_estimate_mb: ramMap[tab.id] || 0,
        };
        pending[tab.id] = true;
        toSleep.push(tab.id);
    }
    if (toSleep.length === 0) return;

    await Promise.all([
        chrome.storage.session.set({ [STORAGE_KEYS.SLEEPING_TABS]: sleeping }),
        chrome.storage.session.set({ 'pending_sleep': pending }),
    ]);
    await Promise.all(toSleep.map(id =>
        chrome.tabs.update(id, { url: 'about:blank' }).catch(() => {})
    ));
    await updateBadge();
}

async function wakeAllTabs() {
    const [sleepData, activeData] = await Promise.all([
        chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS),
        chrome.storage.session.get(STORAGE_KEYS.TAB_LAST_ACTIVE),
    ]);
    const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};
    const entries = Object.entries(sleeping);
    if (entries.length === 0) return;

    const activeMap = activeData[STORAGE_KEYS.TAB_LAST_ACTIVE] || {};
    await Promise.all(entries.map(([id, info]) =>
        chrome.tabs.update(parseInt(id, 10), { url: info.original_url }).catch(() => {})
    ));
    for (const [id] of entries) delete activeMap[parseInt(id, 10)];
    await Promise.all([
        chrome.storage.session.set({ [STORAGE_KEYS.SLEEPING_TABS]: {} }),
        chrome.storage.session.set({ [STORAGE_KEYS.TAB_LAST_ACTIVE]: activeMap }),
    ]);
    await updateBadge();
}

// ─── Badge Update ────────────────────────────────────────────────────────────

async function updateBadge() {
    const sleepData = await chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS);
    const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};
    const count = Object.keys(sleeping).length;

    let pressure_pct = 0;
    try {
        const ram = await getMemoryInfo();
        pressure_pct = ram.pressure_pct;
    } catch (_) {}

    // Badge text: count of sleeping tabs
    await chrome.action.setBadgeText({ text: count > 0 ? String(count) : '' });

    // Badge background color based on RAM pressure
    let badgeColor;
    let iconVariant;
    if (pressure_pct >= 85) {
        badgeColor = [198, 40, 40, 255];   // red
        iconVariant = 'red';
    } else if (pressure_pct >= 60) {
        badgeColor = [230, 81, 0, 255];    // amber
        iconVariant = 'amber';
    } else {
        badgeColor = [46, 125, 50, 255];   // green
        iconVariant = 'green';
    }

    await chrome.action.setBadgeBackgroundColor({ color: badgeColor });

    // Update action icon to reflect RAM pressure using brand color variants
    await chrome.action.setIcon({
        path: {
            16: `icons/icon-${iconVariant}.png`,
            32: `icons/icon-${iconVariant}.png`,
            48: `icons/icon-${iconVariant}.png`,
            128: `icons/icon-${iconVariant}.png`,
        },
    });
}

// ─── Full State ──────────────────────────────────────────────────────────────

async function getFullState() {
    const [sleepData, activeData, ramData, settings, rules, ram_stats] = await Promise.all([
        chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS),
        chrome.storage.session.get(STORAGE_KEYS.TAB_LAST_ACTIVE),
        chrome.storage.session.get(STORAGE_KEYS.TAB_RAM_ESTIMATE),
        getSettings(),
        getRules(),
        getMemoryInfo().catch(() => ({ total_mb: 0, available_mb: 0, pressure_pct: 0 })),
    ]);

    return {
        sleeping_tabs: sleepData[STORAGE_KEYS.SLEEPING_TABS] || {},
        tab_last_active: activeData[STORAGE_KEYS.TAB_LAST_ACTIVE] || {},
        tab_ram_estimate: ramData[STORAGE_KEYS.TAB_RAM_ESTIMATE] || {},
        settings,
        rules,
        ram_stats,
    };
}

// ─── Sleep on Minimize ───────────────────────────────────────────────────────

chrome.windows.onFocusChanged.addListener(async (windowId) => {
    if (windowId === chrome.windows.WINDOW_ID_NONE) {
        const settings = await getSettings();
        if (!settings.sleep_on_minimize) return;
        // Use alarm for the delay — setTimeout is unreliable in service workers
        chrome.alarms.create('sleep-on-minimize-pending', { delayInMinutes: 0.5 });
    } else {
        // Chrome regained focus — cancel the pending sleep alarm
        chrome.alarms.clear('sleep-on-minimize-pending');
    }
});

async function handleSleepOnMinimizeAlarm() {
    // Check that Chrome is still not focused before sleeping
    const windows = await chrome.windows.getAll({ populate: false });
    const anyFocused = windows.some(w => w.focused);
    if (anyFocused) return;

    await sleepAllTabs();
}

// ─── Notifications ───────────────────────────────────────────────────────────

async function notifyTabsSlept(count, savedMb) {
    const settings = await getSettings();
    if (!settings.notifications_enabled) return;

    let message = `Snoozed ${count} inactive tab${count !== 1 ? 's' : ''}`;
    if (savedMb > 0) message += ` — saved ~${Math.round(savedMb)} MB`;

    chrome.notifications.create({
        type: 'basic',
        iconUrl: 'icons/icon48.png',
        title: 'Gingify',
        message,
    });
}

// ─── Message Handler ─────────────────────────────────────────────────────────

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    handleMessage(message, sender, sendResponse);
    return true; // keep channel open for async responses
});

async function handleMessage(message, sender, sendResponse) {
    switch (message.type) {
        case 'SLEEP_TAB':
            await sleepTab(message.tabId);
            sendResponse({ ok: true });
            break;
        case 'WAKE_TAB':
            await wakeTab(message.tabId);
            sendResponse({ ok: true });
            break;
        case 'SLEEP_ALL':
            await sleepAllTabs();
            sendResponse({ ok: true });
            break;
        case 'WAKE_ALL':
            await wakeAllTabs();
            sendResponse({ ok: true });
            break;
        case 'GET_STATE': {
            const state = await getFullState();
            sendResponse(state);
            break;
        }
        case 'REPORT_RAM': {
            if (!sender.tab || !sender.tab.id) { sendResponse({ ok: false }); break; }
            const ramData = await chrome.storage.session.get(STORAGE_KEYS.TAB_RAM_ESTIMATE);
            const ramMap = ramData[STORAGE_KEYS.TAB_RAM_ESTIMATE] || {};
            ramMap[sender.tab.id] = message.heapMb;
            await chrome.storage.session.set({ [STORAGE_KEYS.TAB_RAM_ESTIMATE]: ramMap });
            sendResponse({ ok: true });
            break;
        }
        case 'GET_TAB_SLEEP_DATA': {
            const sleepData = await chrome.storage.session.get(STORAGE_KEYS.SLEEPING_TABS);
            const sleeping = sleepData[STORAGE_KEYS.SLEEPING_TABS] || {};
            sendResponse(sleeping[message.tabId] || null);
            break;
        }
        default:
            sendResponse({ ok: false, error: 'Unknown message type' });
    }
}
