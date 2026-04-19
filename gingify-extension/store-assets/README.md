# Chrome Web Store Assets Needed

## Required before submission:

### Screenshots (1–5 required)
- Size: 1280×800 or 640×400 pixels
- Format: PNG or JPEG
- Suggested screenshots:
  1. Popup open showing tab list with RAM estimates and Sleep buttons
  2. Options page showing per-domain rules
  3. Sleep.html page showing a tab sleeping
  4. Popup showing several sleeping tabs with badge count visible
  5. Before/after RAM usage comparison

### Promotional Images (optional but recommended)
- Small: 440×280 PNG
- Large: 1400×560 PNG
- Marquee: 1400×560 PNG

### Store Listing Copy

**Name:** Gingify — Tab Memory Manager

**Short Description (max 132 chars):**
Sleep idle tabs to free RAM. Manual control, per-domain rules, real-time RAM dashboard.

**Full Description (up to 16,000 chars):**
[See store-assets/full_description.txt]

The full description must include:
- "Zero telemetry. No data ever leaves your browser." — in its own paragraph, not buried
- "GPL-3.0 open source — you can read every line of code on GitHub."
- These two points appear near the top, before the feature list

**Category:** Productivity

**Language:** English (United States)

---

## Manual Steps Before Submission

1. Create a Google developer account at chrome.google.com/webstore/devconsole ($5 one-time fee)
2. Confirm 4 icon sizes exist in `icons/`: icon16.png, icon32.png, icon48.png, icon128.png
3. Take 2–5 screenshots per the specs above
4. Package the extension:
   ```
   # Windows (PowerShell):
   Compress-Archive -Path gingify-extension\* -DestinationPath gingify-extension.zip

   # Mac/Linux:
   cd gingify-extension && zip -r ../gingify-extension.zip . -x "*.DS_Store"
   ```
   Verify: when unzipped, `manifest.json` is at the root (not inside a subfolder)
5. Upload the ZIP and fill in the listing
6. Set privacy policy URL: GitHub repo README privacy section, or a hosted privacy page
7. Submit for review — expect 1–3 business days for new extensions

---

## Known Limitations (document in listing if asked)

- RAM estimates are approximations — reported as "~X MB" using `performance.memory.usedJSHeapSize`
- Tab wake requires a full page reload (Chrome MV3 cannot resume a frozen renderer)
- Auto-sleep minimum interval is 1 minute (Chrome alarm API constraint)
- Sleep-on-minimize has a 30-second delay to avoid sleeping tabs on accidental minimizes
- Extension does not communicate with the Gingify desktop app — they are independent products
