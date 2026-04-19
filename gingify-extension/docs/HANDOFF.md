# Gingify for Chrome — Handoff Note
**Status:** Code complete. Ready for Chrome Web Store submission pending manual assets.
**Date:** 2026-04-16

---

## What Was Built

### Core Extension Files

| File | What it does |
|------|-------------|
| `manifest.json` | MV3 manifest — declares permissions, icons, service worker, popup, content script |
| `background/service_worker.js` | All background logic: tab sleep/wake, auto-sleep timer, badge updates, context menus, sleep-on-minimize, browser restart recovery |
| `background/storage_keys.js` | Shared constants for storage keys and default settings values |
| `content/sleep_overlay.js` | Content script injected into every page; reads `performance.memory` for per-tab RAM estimates and reports to service worker |
| `popup/popup.html` | Extension popup UI structure |
| `popup/popup.css` | Popup styles |
| `popup/popup.js` | Popup logic: renders tab list with RAM estimates, sleep/wake buttons, Sleep All / Wake All, opens options page |
| `options/options.html` | Full settings page structure |
| `options/options.css` | Options page styles |
| `options/options.js` | Options page logic: loads/saves settings, manages per-domain rules table, domain validation, storage quota warnings |
| `sleep.html` | Page shown when a tab is sleeping — displays domain, favicon, RAM saved; click or keypress wakes the tab |
| `welcome.html` | First-install onboarding — shows live tab count, initial settings (auto-sleep threshold, sleep on minimize, protect pinned), saves on "Start Using Gingify" click |
| `icons/` | Extension icons at 4 sizes (16, 32, 48, 128) + 3 badge state icons (green, amber, red) |

---

## Manual Steps Before Chrome Web Store Submission

1. **Create developer account** — chrome.google.com/webstore/devconsole ($5 one-time fee)

2. **Take screenshots** — 2–5 screenshots per `store-assets/README.md` specs:
   - 1280×800 or 640×400 PNG/JPEG
   - Suggested: popup with tab list, options page, sleep.html, popup with sleeping tabs, RAM before/after

3. **Update the Chrome extension link in the desktop app** — once the CWS listing URL is known, replace the placeholder `https://github.com/IshekKhal/gingify` in `src/detail_window/app.js` (the `about-chrome-ext` handler) with the real CWS URL

4. **Package the extension:**
   ```
   # Windows (PowerShell, from repo root):
   Compress-Archive -Path gingify-extension\* -DestinationPath gingify-extension.zip

   # Mac/Linux:
   cd gingify-extension && zip -r ../gingify-extension.zip . -x "*.DS_Store"
   ```
   Verify: `manifest.json` is at the root of the zip (not inside a subfolder)

5. **Upload and fill in listing** — use `store-assets/full_description.txt` for the description, `store-assets/README.md` for asset specs

6. **Privacy policy URL** — set to the GitHub repo README (which documents zero telemetry) or a hosted privacy page

7. **Submit for review** — expect 1–3 business days for new extensions

---

## Known Limitations

- RAM estimates are approximations (`~X MB`) — Chrome MV3 removed `chrome.processes`; we use `performance.memory.usedJSHeapSize` from content scripts instead
- Tab wake requires a full page reload — Chrome cannot resume a frozen renderer; the tab navigates back to its original URL
- Auto-sleep minimum interval is 1 minute — Chrome alarm API enforces this floor
- Sleep-on-minimize has a 30-second delay — avoids sleeping tabs on accidental minimizes
- Incognito tabs are never shown, touched, or affected by any extension logic

---

## How to Load in Development

1. Open `chrome://extensions`
2. Enable **Developer mode** (toggle top-right)
3. Click **Load unpacked** → select the `gingify-extension/` folder
4. Make changes to source files
5. Click the **↺ reload** button on the Gingify card in chrome://extensions
6. Reopen the popup to see changes

No build step. No npm. Edit files directly.

---

## How to Package for Submission

See "Package the extension" under Manual Steps above. The zip must contain `manifest.json` at the root — do not zip the parent folder.

---

## Connection Note: Desktop App ↔ Extension

**These are two independent products. They do NOT communicate with each other in v1.**

- The desktop app (Tauri/Rust/JS) manages Windows process RAM and system-level memory
- The extension (Chrome MV3) manages browser tab memory within Chrome
- They share the Gingify brand and the same GitHub repo but have no runtime connection, no shared storage, no IPC
- Cross-promotion only: the desktop app's About section links to the Chrome extension, and the extension's About page links to the desktop app

---

## Security & Privacy Summary

- Zero network requests — no `fetch()`, no `XMLHttpRequest`, no analytics, no beacons
- No `eval()` — strict Content Security Policy applied: `script-src 'self'; object-src 'self'`
- Tab URLs stored only in `chrome.storage.session` (cleared when Chrome closes)
- Incognito tabs explicitly excluded from all logic
- `host_permissions: <all_urls>` required solely to inject the RAM-reading content script
- GPL-3.0 open source — all code is auditable at https://github.com/IshekKhal/gingify
