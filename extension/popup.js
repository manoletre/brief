const local = chrome.storage.local;
const $ = id => document.getElementById(id);
const get = async (key, dflt) => (await local.get(key))[key] ?? dflt;
// Same list as background.js, for the first time the popup opens.
const DEFAULT_BLOCKED = [
  'mail.google.com', 'outlook.live.com', 'outlook.office.com', 'accounts.google.com', 'myaccount.google.com',
  'paypal.com', 'venmo.com', 'wise.com', 'revolut.com', 'coinbase.com', 'robinhood.com', 'schwab.com',
  'fidelity.com', 'vanguard.com', 'chase.com', 'wellsfargo.com', 'bankofamerica.com', 'capitalone.com',
  'americanexpress.com', '1password.com', 'bitwarden.com', 'lastpass.com', '*bank*', '*health*',
];

async function render() {
  const status = await get('status', null), stats = await get('stats', {}), paused = await get('pausedUntil', 0);
  const blocked = await get('blocked', DEFAULT_BLOCKED);
  const ago = status ? Math.round((Date.now() - status.at) / 60000) : null;
  $('status').innerHTML = !status ? 'Connecting to brief…'
    : status.ok ? `<b class="ok">●</b> Sending to brief · ${ago < 1 ? 'just now' : ago + ' min ago'}`
    : `<b class="bad">●</b> ${status.error.replace(/[<&]/g, '')}`;
  const mins = stats.day === new Date().toDateString() ? Math.round(stats.seconds / 60) : 0;
  $('today').textContent = mins >= 60 ? `${Math.floor(mins / 60)}h ${mins % 60}m` : `${mins} min`;

  const isPaused = Date.now() < paused;
  $('pause').textContent = isPaused
    ? `▶ Resume (paused until ${new Date(paused).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })})`
    : '⏸ Pause for an hour';
  $('pause').onclick = async () => {
    await local.set({ pausedUntil: isPaused ? 0 : Date.now() + 3600e3 });
    render();
  };

  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  let host = '';
  try { host = new URL(tab.url).hostname.replace(/^www\./, ''); } catch (e) {}
  const btn = $('block');
  btn.hidden = !/^https?:/.test(tab?.url || '');
  btn.textContent = blocked.includes(host) ? `✓ Never recording ${host}` : `🚫 Never record ${host}`;
  btn.onclick = async () => {
    if (!blocked.includes(host)) await local.set({ blocked: [...blocked, host] });
    render();
  };

  const ul = $('blocked');
  ul.replaceChildren(...blocked.map(b => {
    const li = document.createElement('li'), x = document.createElement('a');
    li.textContent = b;
    x.textContent = '×';
    x.title = 'Record this site again';
    x.onclick = async () => { await local.set({ blocked: blocked.filter(d => d !== b) }); render(); };
    li.append(x);
    return li;
  }));
}

$('open').onclick = e => { e.preventDefault(); chrome.tabs.create({ url: `http://127.0.0.1:${BRIEF.port}/today` }); };
chrome.runtime.sendMessage({ type: 'flush' }).catch(() => {});
render();
setTimeout(render, 1500);
