// Tells brief's background script what this page is about: its description and, for pages you
// read (articles, docs, papers), the start of the text. The background script decides whether to
// keep it (blocked sites, paused, too short a visit).
(() => {
  const MAX = 1500;
  const meta = name =>
    document.querySelector(`meta[name="${name}"], meta[property="${name}"]`)?.content?.trim() || '';
  const clean = s => (s || '').replace(/\s+/g, ' ').trim();

  function excerpt() {
    const root = document.querySelector('article, main, [role=main]') || document.body;
    if (!root) return '';
    const text = clean(root.innerText);
    const articleLike = document.querySelector('article') || meta('og:type') === 'article' || text.length > 2500;
    return articleLike ? text.slice(0, MAX) : '';
  }

  let sent = '';
  function send() {
    if (location.href === sent) return;
    sent = location.href;
    try {
      chrome.runtime.sendMessage({
        type: 'page',
        url: location.href,
        title: document.title,
        description: (meta('description') || meta('og:description')).slice(0, 300),
        excerpt: excerpt(),
      });
    } catch (e) {} // extension reloaded under us; the next page load reconnects
  }

  // Wait a moment so single-page apps and slow pages have their real text.
  setTimeout(send, 1500);
  // Single-page apps change the URL without a page load.
  setInterval(() => { if (location.href !== sent) setTimeout(send, 1500); }, 3000);
})();
