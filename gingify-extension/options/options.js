import { STORAGE_KEYS, DEFAULT_SETTINGS } from '../background/storage_keys.js';

// ── Load & Populate ───────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', async () => {
    const [settingsData, rulesData] = await Promise.all([
        chrome.storage.sync.get(STORAGE_KEYS.SETTINGS),
        chrome.storage.sync.get(STORAGE_KEYS.RULES),
    ]);

    const settings = settingsData[STORAGE_KEYS.SETTINGS] || DEFAULT_SETTINGS;
    const rules = rulesData[STORAGE_KEYS.RULES] || [];

    populateSettings(settings);
    renderRules(rules);
    await checkStorageQuota();

    // Show extension version in About section
    const manifest = chrome.runtime.getManifest();
    document.getElementById('version-number').textContent = manifest.version;

    // ── Live-save listeners ──────────────────────────────────────────────────

    document.getElementById('auto-sleep-toggle').addEventListener('change', e => {
        updateSetting('auto_sleep_enabled', e.target.checked);
    });

    document.getElementById('auto-sleep-after').addEventListener('change', e => {
        updateSetting('auto_sleep_after_mins', parseInt(e.target.value, 10));
    });

    document.getElementById('sleep-pinned-toggle').addEventListener('change', e => {
        updateSetting('sleep_pinned_tabs', e.target.checked);
    });

    document.getElementById('sleep-minimize-toggle').addEventListener('change', e => {
        updateSetting('sleep_on_minimize', e.target.checked);
    });

    document.getElementById('notifications-toggle').addEventListener('change', e => {
        updateSetting('notifications_enabled', e.target.checked);
    });

    // ── Add Rule form ────────────────────────────────────────────────────────

    document.getElementById('add-rule-btn').addEventListener('click', () => {
        document.getElementById('add-rule-form').hidden = false;
        document.getElementById('rule-domain').focus();
    });

    document.getElementById('rule-action').addEventListener('change', e => {
        document.getElementById('rule-after').hidden = e.target.value !== 'sleep';
    });

    document.getElementById('save-rule-btn').addEventListener('click', async () => {
        const domain = document.getElementById('rule-domain').value.trim().toLowerCase();
        const action = document.getElementById('rule-action').value;
        const afterRaw = document.getElementById('rule-after').value;
        const after_mins = action === 'sleep' ? (parseInt(afterRaw, 10) || null) : null;

        if (!isValidDomain(domain)) {
            document.getElementById('rule-domain').focus();
            document.getElementById('rule-domain').select();
            return;
        }

        const data = await chrome.storage.sync.get(STORAGE_KEYS.RULES);
        const rules = data[STORAGE_KEYS.RULES] || [];

        // Upsert — update existing rule for domain if present
        const idx = rules.findIndex(r => r.domain === domain);
        const entry = { domain, action, ...(after_mins != null ? { after_mins } : {}) };
        if (idx >= 0) {
            rules[idx] = entry;
        } else {
            rules.push(entry);
        }

        await chrome.storage.sync.set({ [STORAGE_KEYS.RULES]: rules });
        renderRules(rules);
        resetAddRuleForm();
        await checkStorageQuota();
    });
});

// ── Settings Helpers ──────────────────────────────────────────────────────────

function populateSettings(settings) {
    document.getElementById('auto-sleep-toggle').checked = !!settings.auto_sleep_enabled;

    const afterSelect = document.getElementById('auto-sleep-after');
    afterSelect.value = String(settings.auto_sleep_after_mins);
    // Fall back to 20 minutes if no matching option
    if (!afterSelect.value || afterSelect.selectedIndex === -1) afterSelect.value = '20';

    document.getElementById('sleep-pinned-toggle').checked = !!settings.sleep_pinned_tabs;
    document.getElementById('sleep-minimize-toggle').checked = !!settings.sleep_on_minimize;
    document.getElementById('notifications-toggle').checked = !!settings.notifications_enabled;
}

async function updateSetting(key, value) {
    const data = await chrome.storage.sync.get(STORAGE_KEYS.SETTINGS);
    const settings = data[STORAGE_KEYS.SETTINGS] || DEFAULT_SETTINGS;
    settings[key] = value;
    await chrome.storage.sync.set({ [STORAGE_KEYS.SETTINGS]: settings });
}

// ── Rules ─────────────────────────────────────────────────────────────────────

function renderRules(rules) {
    const tbody = document.getElementById('rules-tbody');
    const noRulesMsg = document.getElementById('no-rules-msg');
    tbody.innerHTML = '';

    if (!rules || rules.length === 0) {
        noRulesMsg.hidden = false;
        return;
    }
    noRulesMsg.hidden = true;

    for (const rule of rules) {
        const tr = document.createElement('tr');

        const afterText = (rule.action === 'sleep' && rule.after_mins)
            ? `${rule.after_mins} min`
            : '—';

        const actionLabel = rule.action === 'never' ? 'Never snooze' : 'Snooze after';

        const domainTd = document.createElement('td');
        domainTd.textContent = rule.domain;

        const actionTd = document.createElement('td');
        actionTd.textContent = actionLabel;

        const afterTd = document.createElement('td');
        afterTd.textContent = afterText;

        const removeTd = document.createElement('td');
        const removeBtn = document.createElement('button');
        removeBtn.className = 'btn-remove';
        removeBtn.textContent = '×';
        removeBtn.addEventListener('click', () => removeRule(rule.domain));
        removeTd.appendChild(removeBtn);

        tr.append(domainTd, actionTd, afterTd, removeTd);
        tbody.appendChild(tr);
    }
}

async function removeRule(domain) {
    const data = await chrome.storage.sync.get(STORAGE_KEYS.RULES);
    const rules = (data[STORAGE_KEYS.RULES] || []).filter(r => r.domain !== domain);
    await chrome.storage.sync.set({ [STORAGE_KEYS.RULES]: rules });
    renderRules(rules);
    await checkStorageQuota();
}

function resetAddRuleForm() {
    document.getElementById('rule-domain').value = '';
    document.getElementById('rule-action').value = 'never';
    document.getElementById('rule-after').value = '';
    document.getElementById('rule-after').hidden = true;
    document.getElementById('add-rule-form').hidden = true;
}

// ── Validation ─────────────────────────────────────────────────────────────────

function isValidDomain(str) {
    if (!str) return false;
    if (str.includes(' ')) return false;
    if (str.startsWith('http://') || str.startsWith('https://')) return false;
    if (!str.includes('.')) return false;
    return true;
}

// ── Storage Quota ──────────────────────────────────────────────────────────────

async function checkStorageQuota() {
    const used = await new Promise(resolve =>
        chrome.storage.sync.getBytesInUse(null, resolve)
    );
    const max = chrome.storage.sync.QUOTA_BYTES; // 102400
    const pct = Math.round((used / max) * 100);

    const warning = document.getElementById('storage-warning');
    const fullError = document.getElementById('storage-full-error');
    const addBtn = document.getElementById('add-rule-btn');

    if (pct > 95) {
        warning.hidden = true;
        fullError.hidden = false;
        addBtn.disabled = true;
    } else if (pct > 80) {
        document.getElementById('storage-pct').textContent = pct;
        warning.hidden = false;
        fullError.hidden = true;
        addBtn.disabled = false;
    } else {
        warning.hidden = true;
        fullError.hidden = true;
        addBtn.disabled = false;
    }
}
