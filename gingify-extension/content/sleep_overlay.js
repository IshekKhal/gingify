// RAM estimation content script — injected into every page at document_end.
// Approximates actual tab RSS from three signals, since Chrome does not
// expose per-tab process memory to extensions:
//   - JS heap (underreports by ~3× vs renderer RSS)
//   - Transferred/decoded resource payload (HTML, JS, images, fonts, ...)
//   - DOM complexity (~1 KB per node covers layout + style state)
// Result is always a rough approximation; the UI prefixes values with ~.
(function () {
    if (location.protocol === 'chrome-extension:' || location.protocol === 'chrome:') return;

    function estimateTabMb() {
        let heap = 0;
        try { heap = performance.memory ? performance.memory.usedJSHeapSize : 0; } catch (_) {}

        let resourceBytes = 0;
        try {
            for (const e of performance.getEntriesByType('resource')) {
                resourceBytes += e.transferSize || e.decodedBodySize || e.encodedBodySize || 0;
            }
        } catch (_) {}

        let domNodes = 0;
        try { domNodes = document.getElementsByTagName('*').length; } catch (_) {}

        const bytes = heap * 3 + resourceBytes + domNodes * 1024;
        return Math.max(1, Math.round(bytes / (1024 * 1024)));
    }

    function report() {
        try {
            chrome.runtime.sendMessage({ type: 'REPORT_RAM', heapMb: estimateTabMb() });
        } catch (_) {
            // Extension context may be invalidated during reload — ignore
        }
    }

    if (document.readyState === 'complete') {
        report();
    } else {
        window.addEventListener('load', report, { once: true });
    }
    // Re-estimate periodically so long-lived pages (Gmail, Docs) show fresh numbers.
    setInterval(report, 30000);
})();
