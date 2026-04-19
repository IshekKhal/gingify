const params = new URLSearchParams(location.search);
const tabId = parseInt(params.get('tabId'), 10);

// Wake listeners registered unconditionally — replaced on browser-restart recovery below
document.addEventListener('click', wake);

function wake() {
  if (!tabId) return;
  chrome.runtime.sendMessage({ type: 'WAKE_TAB', tabId });
}

// Load sleeping tab info from service worker via targeted message
if (tabId) {
  chrome.runtime.sendMessage({ type: 'GET_TAB_SLEEP_DATA', tabId }, (info) => {
    if (!info) {
      // Browser restarted — session storage was cleared, original URL is gone
      document.getElementById('domain').textContent = 'Session lost';
      document.querySelector('.message').innerHTML =
        'Browser was restarted and this tab\'s state was lost.<br>Click anywhere to close this tab.';
      // Replace wake behavior with close
      document.removeEventListener('click', wake);
      document.removeEventListener('keydown', wake);
      document.addEventListener('click', () => window.close());
      document.addEventListener('keydown', () => window.close());
      return;
    }

    // Show domain
    try {
      const url = new URL(info.original_url);
      document.getElementById('domain').textContent = url.hostname;
    } catch (_) {}

    // Show favicon
    if (info.favicon_url) {
      const img = document.getElementById('favicon');
      img.src = info.favicon_url;
      img.style.display = 'block';
      img.onerror = () => { img.style.display = 'none'; };
    }

    // Show RAM saved estimate
    if (info.ram_estimate_mb > 0) {
      const el = document.getElementById('ram-saved');
      el.textContent = `Saved ~${info.ram_estimate_mb} MB of RAM`;
      el.style.display = 'block';
    }
  });
}
