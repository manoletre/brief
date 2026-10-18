// brief's browser extension (Dia, Arc, Chrome… any Chromium browser): measures how long each page is actually in front of you (focused window,
// active tab, not idle) and sends those visits to the local brief server once a minute. Nothing
// leaves this Mac. `brief browser` writes config.js (port, key, build) next to this file.
importScripts('config.js');

const API = `http://127.0.0.1:${BRIEF.port}/api/browsing`;
const MIN_SECONDS = 5;        // shorter visits are tab-switching noise
const EXCERPT_SECONDS = 20;   // only keep page text for pages you actually stayed on
const SEGMENT_SECONDS = 300;  // long visits are cut into 5-minute pieces so they arrive in time
const DEFAULT_BLOCKED = [
  'mail.google.com', 'outlook.live.com', 'outlook.office.com', 'accounts.google.com', 'myaccount.google.com',
  'paypal.com', 'venmo.com', 'wise.com', 'revolut.com', 'coinbase.com', 'robinhood.com', 'schwab.com',
  'fidelity.com', 'vanguard.com', 'chase.com', 'wellsfargo.com', 'bankofamerica.com', 'capitalone.com',
  'americanexpress.com', '1password.com', 'bitwarden.com', 'lastpass.com', '*bank*', '*health*',
];

const local = chrome.storage.local, session = chrome.storage.session;
const get = async (area, key, dflt) => (await area.get(key))[key] ?? dflt;
const set = (area, key, value) => area.set({ [key]: value });

async function settings() {
  return { blocked: await get(local, 'blocked', DEFAULT_BLOCKED), pausedUntil: await get(local, 'pausedUntil', 0) };
}

function domainOf(url) {
  try { return new URL(url).hostname.replace(/^www\./, ''); } catch (e) { return ''; }
}

function isBlocked(url, blocked) {
  const u = new URL(url), host = u.hostname.replace(/^www\./, '');
  if (['127.0.0.1', 'localhost'].includes(host) && u.port === String(BRIEF.port)) return true; // brief itself
  return blocked.some(b => {
    b = b.trim().toLowerCase();
    if (!b) return false;
    if (b.includes('*')) return new RegExp('^' + b.split('*').map(s => s.replace(/[.+?^${}()|[\]\\]/g, '\\$&')).join('.*') + '$').test(host);
    return host === b || host.endsWith('.' + b);
  });
}

// Query strings often carry tokens and session ids: keep only the ones that say what you were
// looking at (a search, a video).
function cleanUrl(url) {
  const u = new URL(url), keep = new URLSearchParams();
  const wanted = { 'youtube.com': ['v', 'search_query'], 'google.com': ['q'], 'bing.com': ['q'], 'duckduckgo.com': ['q'],
                   'kagi.com': ['q'], 'github.com': ['q'], 'scholar.google.com': ['q'], 'arxiv.org': ['query'] };
  const host = u.hostname.replace(/^www\./, '');
  for (const [site, params] of Object.entries(wanted)) {
    if (host === site || host.endsWith('.' + site)) params.forEach(p => u.searchParams.has(p) && keep.set(p, u.searchParams.get(p)));
  }
  u.search = keep.toString();
  u.hash = '';
  return u.toString();
}

const tracked = url => /^https?:/.test(url || '');

// Chrome fires events in bursts; run handlers one at a time so read-modify-write of storage
// never loses a visit.
let chain = Promise.resolve();
const serial = fn => (...args) => (chain = chain.then(() => fn(...args)).catch(e => console.warn('brief:', e)));

// ── visits ────────────────────────────────────────────────────────────

async function endCurrent(now = Date.now()) {
  const cur = await get(session, 'current', null);
  await session.remove('current');
  if (!cur) return;
  const seconds = Math.round((now - cur.start) / 1000);
  const { blocked, pausedUntil } = await settings();
  if (seconds < MIN_SECONDS || now < pausedUntil || isBlocked(cur.url, blocked)) return;
  const info = (await get(session, 'pages', {}))[cur.url] || {};
  const visit = { url: cleanUrl(cur.url), title: cur.title || info.title || '', domain: domainOf(cur.url),
                  start: new Date(cur.start).toISOString(), seconds };
  if (info.description) visit.description = info.description;
  if (info.excerpt && seconds >= EXCERPT_SECONDS && !(await get(session, 'sentExcerpts', [])).includes(visit.url)) {
    visit.excerpt = info.excerpt;
    await set(session, 'sentExcerpts', [...(await get(session, 'sentExcerpts', [])), visit.url].slice(-300));
  }
  const queue = await get(local, 'queue', []);
  queue.push(visit);
  await set(local, 'queue', queue.slice(-2000));
  const today = new Date().toDateString(), stats = await get(local, 'stats', {});
  await set(local, 'stats', stats.day === today ? { day: today, seconds: stats.seconds + seconds } : { day: today, seconds });
}

async function startTab(tab) {
  await endCurrent();
  if (!tab || !tracked(tab.url) || tab.incognito) return;
  await set(session, 'current', { url: tab.url, title: tab.title, tabId: tab.id, start: Date.now() });
}

async function activeTab() {
  const win = await chrome.windows.getLastFocused().catch(() => null);
  if (!win || !win.focused) return null;
  const [tab] = await chrome.tabs.query({ active: true, windowId: win.id });
  return tab || null;
}

async function resync() {
  if ((await chrome.idle.queryState(120)) !== 'active') return endCurrent();
  const tab = await activeTab(), cur = await get(session, 'current', null);
  if (tab && cur && cur.tabId === tab.id && cur.url === tab.url) {
    if (tab.title && tab.title !== cur.title) await set(session, 'current', { ...cur, title: tab.title });
    return;
  }
  await startTab(tab);
}

const onResync = serial(resync);
chrome.tabs.onActivated.addListener(() => onResync());
chrome.tabs.onUpdated.addListener((id, change, tab) => { if (tab.active && (change.url || change.status === 'complete' || change.title)) onResync(); });
chrome.windows.onFocusChanged.addListener(() => onResync());
chrome.idle.setDetectionInterval(120);
chrome.idle.onStateChanged.addListener(state => state === 'active' ? onResync() : serial(endCurrent)());

chrome.runtime.onMessage.addListener((msg, sender) => {
  if (msg?.type !== 'page' || !tracked(msg.url) || sender.tab?.incognito) return;
  serial(async () => {
    const pages = await get(session, 'pages', {});
    pages[msg.url] = { title: msg.title, description: msg.description, excerpt: msg.excerpt };
    const keys = Object.keys(pages);
    keys.slice(0, Math.max(0, keys.length - 60)).forEach(k => delete pages[k]);
    await set(session, 'pages', pages);
  })();
});

// ── sending to brief, and updating ourselves ─────────────────────────

async function flush() {
  // Close long visits into a piece so a whole afternoon on one page still arrives.
  const cur = await get(session, 'current', null);
  if (cur && Date.now() - cur.start > SEGMENT_SECONDS * 1000) {
    await endCurrent();
    await set(session, 'current', { ...cur, start: Date.now() });
  }
  await resync();

  const queue = await get(local, 'queue', []);
  let res;
  try {
    res = await fetch(API, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Brief-Key': BRIEF.key },
                             body: JSON.stringify({ build: BRIEF.build, visits: queue.slice(0, 500) }) });
  } catch (e) {
    return set(local, 'status', { ok: false, error: "brief isn't running", at: Date.now() });
  }
  if (!res.ok) return set(local, 'status', { ok: false, error: `brief said ${res.status}${res.status === 401 ? ': run `brief browser` again' : ''}`, at: Date.now() });
  const sent = Math.min(queue.length, 500);
  const rest = (await get(local, 'queue', [])).slice(sent);
  await set(local, 'queue', rest);
  await set(local, 'status', { ok: true, at: Date.now() });

  // `brief chrome` / `make` wrote newer files to this folder: reload to run them, at most once
  // an hour per build so a half-written folder can't cause a reload loop.
  const reply = await res.json().catch(() => ({}));
  if (reply.build && reply.build !== BRIEF.build && reply.installed_build === reply.build) {
    const last = await get(local, 'lastReload', {});
    if (last.build !== reply.build || Date.now() - last.at > 3600e3) {
      await set(local, 'lastReload', { build: reply.build, at: Date.now() });
      await endCurrent();
      chrome.runtime.reload();
    }
  }
}

const onFlush = serial(flush);
chrome.alarms.create('flush', { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(a => a.name === 'flush' && onFlush());
chrome.runtime.onStartup.addListener(() => onResync());
chrome.runtime.onInstalled.addListener(() => onFlush());
// Popup asks us to send right away (after pausing, blocking a site, or to check the connection).
chrome.runtime.onMessage.addListener(msg => { if (msg?.type === 'flush') onFlush(); });
