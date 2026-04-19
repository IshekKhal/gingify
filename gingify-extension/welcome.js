// Populate live stats
chrome.tabs.query({}, (tabs) => {
  document.getElementById('tab-count').textContent = tabs.length;
  // RAM is estimated — use a rough 150 MB per tab heuristic until real data is available
  document.getElementById('ram-used').textContent = '~' + (tabs.length * 150);
});

// Save settings and close welcome tab
document.getElementById('start-btn').addEventListener('click', async () => {
  const afterMins = parseInt(document.getElementById('sleep-after').value, 10);
  const sleepOnMinimize = document.getElementById('sleep-minimize').checked;
  const sleepPinnedTabs = !document.getElementById('no-pinned').checked;

  const settings = {
    auto_sleep_enabled: afterMins !== 0,
    auto_sleep_after_mins: afterMins || 20,
    sleep_pinned_tabs: sleepPinnedTabs,
    sleep_on_minimize: sleepOnMinimize,
    notifications_enabled: true,
    badge_mode: 'sleep_count',
  };

  await chrome.storage.sync.set({ settings });
  window.close();
});
