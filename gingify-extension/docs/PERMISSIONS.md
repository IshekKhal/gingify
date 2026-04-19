# Gingify Extension — Permissions Justification

This document explains why each permission declared in `manifest.json` is required.
For internal reference — not uploaded to Chrome Web Store but useful for review preparation.

---

## `tabs`

**Why needed:** Reading the full tab list is the core function of the extension. The popup displays every open tab with its title, favicon, URL, and RAM estimate. The `tabs` permission is also required to navigate a tab to `sleep.html` (sleep) and back to its original URL (wake). Without it, none of the sleep/wake mechanics work.

---

## `storage`

**Why needed:** Persisting user settings and per-domain rules across browser sessions. Two storage areas are used:
- `chrome.storage.sync` — stores settings (auto-sleep threshold, toggles) and per-domain rules. Syncs across the user's Chrome devices.
- `chrome.storage.session` — stores the sleeping tab state (original URL, favicon, RAM estimate) for the current browser session. Automatically cleared when Chrome closes, ensuring no stale tab data persists.

---

## `alarms`

**Why needed:** Running the auto-sleep check on a recurring schedule. Chrome's MV3 service workers go idle when not in use; `chrome.alarms` is the only way to reliably wake the service worker on a timer. The alarm fires every 60 seconds to evaluate which inactive tabs exceed the user's auto-sleep threshold.

---

## `notifications`

**Why needed:** Alerting the user when tabs were auto-slept (not manually slept). If 3 tabs were put to sleep automatically while the user was away, a notification tells them what happened and how many tabs were affected. This is only shown when the user has notifications enabled in options (default: on).

---

## `contextMenus`

**Why needed:** Right-click context menu integration. Adds a "Sleep this tab — Gingify" item to the browser's right-click menu, and a "Never auto-sleep [domain]" option. This lets power users sleep tabs without opening the popup and add never-sleep rules with one click from any page.

---

## `system.memory`

**Why needed:** Displaying system-level RAM pressure in the popup. The RAM pressure bar (green/amber/red) in the popup header shows how much of the system's total RAM is in use. This requires reading `chrome.system.memory.getInfo()` to get total and available system RAM. Without this, the RAM pressure bar and badge color logic cannot function.

---

## `host_permissions: <all_urls>`

**Why needed:** Injecting the content script (`content/sleep_overlay.js`) into every tab. The content script reads `performance.memory.usedJSHeapSize` from each tab's renderer process and reports it to the service worker — this is the only way to get a per-tab RAM estimate in MV3 (the `chrome.processes` API was removed). The content script writes nothing to the page and makes no network requests.
