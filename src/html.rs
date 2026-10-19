//! Server-rendered pages. No JS framework; a little inline JS posts feedback.

use crate::store::{Brief, Feedback, Store};
use chrono::NaiveDate;
use pulldown_cmark::{html::push_html, Event, Options, Parser};
use std::collections::HashMap;
use std::fmt::Write as _;

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Markdown to HTML with raw HTML escaped (content comes from chatbots and the model).
pub fn md(s: &str) -> String {
    let parser = Parser::new_ext(s, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH).map(|ev| match ev {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        other => other,
    });
    let mut out = String::new();
    push_html(&mut out, parser);
    out
}

fn safe_url(u: &str) -> String {
    if u.starts_with("https://") || u.starts_with("http://") { esc(u) } else { "#".into() }
}

/// The website only shows the briefs; everything else (objectives, journal, connections) is in
/// the terminal app.
fn layout(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" type="image/png" href="/icon.png"><link rel="apple-touch-icon" href="/icon.png">
<title>{title}</title><style>{CSS}</style></head>
<body><header><a class="brand" href="/"><img src="/icon.png" alt="">brief</a><nav><a href="/" class="on">Briefs</a></nav></header>
<main>{body}</main><script>{JS}</script></body></html>"#,
        title = esc(title)
    )
}

pub fn index(store: &Store, generating: bool, last_error: Option<&str>) -> String {
    let feedback = store.feedback();
    let mut fb_count: HashMap<NaiveDate, usize> = HashMap::new();
    for f in &feedback {
        *fb_count.entry(f.brief_date).or_default() += 1;
    }
    let today = chrono::Local::now().date_naive();
    let mut b = String::new();

    b.push_str("<div class=\"row\"><h1>Daily briefs</h1>");
    if generating {
        b.push_str("<span class=\"pill\">Writing today's brief… this takes a few minutes</span>");
    } else {
        b.push_str("<button onclick=\"gen(this)\">Generate now</button>");
    }
    b.push_str("</div>");
    if let Some(err) = last_error {
        let _ = write!(b, "<p class=\"err\">Last generation failed: {}</p>", esc(err));
    }
    if store.objectives().trim().is_empty() {
        b.push_str("<p class=\"note\">You haven't set objectives yet. Run <code>brief init</code> in your terminal so the brief knows what you're working towards.</p>");
    }

    let dates = store.brief_dates();
    if dates.is_empty() {
        b.push_str("<p class=\"muted\">No briefs yet. One is written every morning once the server is running, or press <em>Generate now</em>.</p>");
    }
    b.push_str("<ul class=\"menu\">");
    for d in dates {
        let Some(brief) = store.brief(d) else { continue };
        let n_items: usize = brief.sections.iter().map(|s| s.items.len()).sum();
        let fb = fb_count.get(&d).copied().unwrap_or(0);
        let mut tag = if d == today { "<span class=\"pill\">today</span>".to_owned() } else { String::new() };
        if brief.version > 1 {
            let _ = write!(tag, " <span class=\"pill\">v{}</span>", brief.version);
        }
        let _ = write!(
            b,
            "<li><a href=\"/brief/{d}\"><span class=\"date\">{}</span>{tag}<span class=\"head\">{}</span>\
             <span class=\"muted small\">{n_items} items · {} min{}</span></a></li>",
            d.format("%a %b %-d"),
            esc(&brief.headline),
            brief.reading_minutes,
            if fb > 0 { format!(" · {fb} feedback") } else { String::new() },
        );
    }
    b.push_str("</ul>");
    layout("Daily briefs", &b)
}

/// One line of markdown without the wrapping paragraph.
fn md_inline(s: &str) -> String {
    let h = md(s);
    let t = h.trim();
    match t.strip_prefix("<p>").and_then(|x| x.strip_suffix("</p>")) {
        Some(inner) if !inner.contains("<p>") => inner.to_owned(),
        _ => h,
    }
}

/// JSON embedded in a <script type="application/json"> block; `<` escaped so it can't close the tag.
fn json_attr(v: &serde_json::Value) -> String {
    v.to_string().replace('<', "\\u003c")
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "blindspot" => "watch out",
        "next_step" => "today",
        "connection" => "connection",
        "resource" => "hidden gem",
        "time" => "your time",
        "reprioritise" => "reprioritise",
        "gap" => "trying vs building",
        "learn" => "learn",
        other => other,
    }
}

/// The versions of a brief (each rewrite keeps the one before).
pub struct Versions {
    /// Oldest first, the current one last.
    pub all: Vec<u32>,
    pub current: u32,
    /// A brief is being written or rewritten right now.
    pub rewriting: bool,
}

fn provider_name(id: &str) -> &str {
    crate::search::Provider::from_id(id).map_or(id, |p| match p {
        crate::search::Provider::Claude => "the agent's web search",
        p => p.name(),
    })
}

/// One brief, shown as a deck: an intro card, then one card per item (rate it, and it moves on
/// to the next), then a closing card. ← → keys or the buttons move between cards; the bar at the
/// top moves between briefs.
pub fn brief(brief: &Brief, feedback: &[Feedback], older: Option<NaiveDate>, newer: Option<NaiveDate>, versions: &Versions) -> String {
    let date = brief.date.map(|d| d.to_string()).unwrap_or_default();
    let mine: HashMap<&str, &Feedback> = feedback
        .iter()
        .filter(|f| f.brief_date.to_string() == date)
        .map(|f| (f.item_id.as_str(), f))
        .collect();
    let items: Vec<(&str, &crate::store::Item)> = brief.sections.iter().flat_map(|s| s.items.iter().map(move |i| (s.title.as_str(), i))).collect();
    let short = |d: NaiveDate| d.format("%a %b %-d").to_string();

    let mut b = String::new();
    // Between briefs.
    b.push_str("<nav class=\"briefnav\">");
    match older {
        Some(d) => { let _ = write!(b, "<a class=\"older\" href=\"/brief/{d}\" title=\"Previous brief\">← {}</a>", short(d)); }
        None => b.push_str("<span class=\"older off\">← no older brief</span>"),
    }
    let _ = write!(b, "<a class=\"all\" href=\"/\">{}</a>", brief.date.map(|d| d.format("%A, %B %-d").to_string()).unwrap_or_else(|| "All briefs".into()));
    match newer {
        Some(d) => { let _ = write!(b, "<a class=\"newer\" href=\"/brief/{d}\" title=\"Next brief\">{} →</a>", short(d)); }
        None => b.push_str("<span class=\"newer off\">latest</span>"),
    }
    b.push_str("</nav>");

    if brief.version.max(1) != versions.current {
        let _ = write!(b, "<p class=\"note\">You're looking at version {} of this brief. <a href=\"/brief/{date}\">See the latest (v{}) →</a></p>", brief.version.max(1), versions.current);
    }
    let _ = write!(b, "<div class=\"deck\" data-date=\"{date}\"><div class=\"pips\">");
    for i in 0..items.len() + 2 {
        let rated = match i {
            0 => "",
            i if i == items.len() + 1 => mine.get("").map_or("", |f| f.rating.as_str()),
            i => mine.get(items[i - 1].1.id.as_str()).map_or("", |f| f.rating.as_str()),
        };
        let _ = write!(b, "<button class=\"pip {rated}\" data-to=\"{i}\" aria-label=\"Card {}\"></button>", i + 1);
    }
    b.push_str("</div><div class=\"cards\">");

    // Intro card.
    let _ = write!(
        b,
        "<section class=\"card intro\"><p class=\"prompt small\"><span class=\"acc\">›</span> brief {} <span class=\"muted\">· ~{} min · {} items{}</span></p><h1>{}</h1><div class=\"summary\">{}</div>",
        esc(&date),
        brief.reading_minutes,
        items.len(),
        match items.iter().filter(|(_, i)| !i.video_file.is_empty()).count() {
            0 => String::new(),
            n => format!(" · {n} video{}", if n == 1 { "" } else { "s" }),
        },
        esc(&brief.headline),
        md(&brief.summary)
    );
    if let Some(r) = brief.research.as_ref().filter(|r| !r.provider.is_empty()) {
        let _ = write!(
            b,
            "<details class=\"research\"><summary><span class=\"acc\">◆</span> researched with <b>{}</b> · {} searches</summary><ul>",
            esc(provider_name(&r.provider)),
            r.queries.len()
        );
        for q in &r.queries {
            let _ = write!(b, "<li>{}</li>", esc(q));
        }
        b.push_str("</ul></details>");
    }
    if let Some(t) = brief.think_first.as_ref().filter(|t| !t.question.is_empty()) {
        let _ = write!(
            b,
            "<div class=\"think\" data-key=\"think:{date}\"><p><b>Think first:</b> {}</p>\
             <textarea rows=\"2\" placeholder=\"Your guess (saved in this browser)\"></textarea>\
             <button onclick=\"this.nextElementSibling.hidden=false;this.remove()\">Show answer</button>\
             <p class=\"answer\" hidden>{}</p></div>",
            md_inline(&t.question),
            md_inline(&t.answer)
        );
    }
    if !brief.terms.is_empty() {
        b.push_str("<p class=\"terms\">");
        for t in &brief.terms {
            let _ = write!(b, "<span class=\"term\"><b>{}</b> · {}</span>", esc(&t.term), esc(&t.meaning));
        }
        b.push_str("</p>");
    }
    b.push_str("<ol class=\"toc\">");
    for (i, (_, item)) in items.iter().enumerate() {
        let _ = write!(
            b,
            "<li class=\"k-{}\"><a href=\"#{}\" data-to=\"{}\"><span class=\"kind\">{}</span>{}</a></li>",
            esc(&item.kind),
            i + 1,
            i + 1,
            esc(kind_label(&item.kind)),
            esc(&item.title)
        );
    }
    b.push_str("</ol><p><button class=\"primary go\" data-to=\"1\">Start →</button></p></section>");

    for (n, (section, item)) in items.iter().enumerate() {
        let has_vis = !item.visual.is_null();
        let checklist = item.kind == "next_step" && !has_vis;
        let _ = write!(
            b,
            "<article class=\"card item k-{}{}\" data-date=\"{date}\" data-id=\"{}\" data-title=\"{}\">",
            esc(&item.kind),
            if has_vis { " has-vis" } else { "" },
            esc(&item.id),
            esc(&item.title)
        );
        let _ = write!(b, "<div class=\"meta\"><span class=\"count\">{} / {}</span>", n + 1, items.len());
        if !item.kind.is_empty() {
            let _ = write!(b, "<span class=\"kind\">{}</span>", esc(kind_label(&item.kind)));
        }
        if !section.is_empty() && !section.eq_ignore_ascii_case(kind_label(&item.kind)) {
            let _ = write!(b, "<span class=\"muted\">{}</span>", esc(section));
        }
        if !item.project.is_empty() {
            let _ = write!(b, "<span class=\"proj\">{}</span>", esc(&item.project));
        }
        b.push_str("</div>");
        let _ = write!(b, "<h3>{}</h3>", esc(&item.title));
        if !item.takeaway.is_empty() {
            let _ = write!(b, "<p class=\"takeaway\"><span>Takeaway</span>{}</p>", md_inline(&item.takeaway));
        }
        if !item.video_clips.is_empty() {
            let says: Vec<String> = item.video["beats"].as_array().into_iter().flatten().map(|b| b["say"].as_str().unwrap_or_default().to_owned()).collect();
            let clips: Vec<String> = item.video_clips.iter().map(|c| format!("/media/{date}/{c}")).collect();
            let n = clips.len();
            let _ = write!(
                b,
                "<figure class=\"video clips\" data-clips=\"{}\" data-says=\"{}\"><video src=\"{}\" muted playsinline preload=\"auto\"></video>\
                 <figcaption><p class=\"say\">{}</p><div class=\"clipnav\"><button class=\"cprev\" title=\"Previous clip\">‹</button>\
                 <span class=\"cn\">1 / {n}</span><button class=\"cnext\" title=\"Next clip (→)\">›</button>\
                 <button class=\"creplay\" title=\"Play again\">↻</button><span class=\"muted ctitle\">{}</span></div></figcaption></figure>",
                esc(&serde_json::to_string(&clips).unwrap_or_default()),
                esc(&serde_json::to_string(&says).unwrap_or_default()),
                esc(&clips[0]),
                esc(says.first().map(String::as_str).unwrap_or_default()),
                esc(item.video["title"].as_str().unwrap_or_default()),
            );
        } else if !item.video_file.is_empty() {
            let title = item.video["title"].as_str().unwrap_or_default();
            let insight = item.video["insight"].as_str().unwrap_or_default();
            let _ = write!(
                b,
                "<figure class=\"video\"><video src=\"/media/{date}/{}\" controls muted playsinline preload=\"metadata\"></video>\
                 <figcaption><span class=\"acc\">▶</span> {}{}</figcaption></figure>",
                esc(&item.video_file),
                esc(title),
                if insight.is_empty() { String::new() } else { format!(" <span class=\"muted\">— {}</span>", esc(insight)) }
            );
        }
        if !item.about.is_empty() {
            let _ = write!(b, "<div class=\"about\">{}</div>", md(&item.about));
        }
        b.push_str("<div class=\"stage\">");
        if has_vis {
            let _ = write!(b, "<div class=\"vis\"><script type=\"application/json\">{}</script></div>", json_attr(&item.visual));
        }
        if !item.steps.is_empty() {
            let _ = write!(b, "<ol class=\"steps{}\">", if checklist { " check" } else { "" });
            for (i, step) in item.steps.iter().enumerate() {
                if checklist {
                    let _ = write!(b, "<li><label><input type=\"checkbox\" data-key=\"done:{date}:{}:{i}\"><span>{}</span></label></li>", esc(&item.id), md_inline(step));
                } else {
                    let _ = write!(b, "<li data-i=\"{i}\"><span>{}</span></li>", md_inline(step));
                }
            }
            b.push_str("</ol>");
        }
        b.push_str("</div>");
        if item.steps.is_empty() && !item.body.is_empty() {
            b.push_str(&md(&item.body));
        }
        if has_vis || item.steps.len() > 1 && !checklist {
            b.push_str("<div class=\"ctl\"><button class=\"replay\" title=\"Replay\">↻ Replay</button></div>");
        }
        if !item.links.is_empty() {
            b.push_str("<ul class=\"links\">");
            for l in &item.links {
                let _ = write!(
                    b,
                    "<li><span class=\"ltype\">{}</span><a href=\"{}\" target=\"_blank\" rel=\"noopener\">{}</a></li>",
                    esc(&l.kind),
                    safe_url(&l.url),
                    esc(&l.title)
                );
            }
            b.push_str("</ul>");
        }
        if !item.why.is_empty() || !item.objective.is_empty() {
            b.push_str("<div class=\"why\"><b>Why you're seeing this</b>");
            if !item.objective.is_empty() {
                let none = item.objective.eq_ignore_ascii_case("none");
                let _ = write!(
                    b,
                    "<span class=\"obj{}\">{}</span>",
                    if none { " none" } else { "" },
                    if none { "serves none of your objectives".into() } else { format!("for: {}", esc(&item.objective)) }
                );
            }
            if !item.why.is_empty() {
                let _ = write!(b, "<p>{}</p>", md_inline(&item.why));
            }
            b.push_str("</div>");
        }
        b.push_str(&feedback_controls(mine.get(item.id.as_str()).copied(), if item.kind == "resource" { "Want more like this?" } else { "Useful?" }));
        b.push_str("</article>");
    }

    // Closing card: the brief as a whole, and where to go next.
    let _ = write!(
        b,
        "<section class=\"card item end\" data-date=\"{date}\" data-id=\"\" data-title=\"\"><h2>That's the brief</h2>\
         <p class=\"rated muted\"></p><p>How was it overall? Anything it should do differently tomorrow?</p>{}<p class=\"endnav\">",
        feedback_controls(mine.get("").copied(), "")
    );
    if let Some(d) = older {
        let _ = write!(b, "<a href=\"/brief/{d}\">← Previous brief ({})</a>", short(d));
    }
    if let Some(d) = newer {
        let _ = write!(b, "<a href=\"/brief/{d}\">Next brief ({}) →</a>", short(d));
    }
    b.push_str("<a href=\"/\">All briefs</a></p>");
    b.push_str(&versions_html(&date, brief.version.max(1), versions));
    if let Some(c) = brief.cost_usd {
        let _ = write!(b, "<p class=\"muted small\">Generated with Claude Code · ${c:.2} equivalent usage</p>");
    }
    b.push_str("</section></div><div class=\"decknav\"><button class=\"prev\">← Back</button><span class=\"muted small hint\">← → keys</span><button class=\"next primary\">Next →</button></div></div>");
    layout(&brief.headline, &b)
}

/// "v1 · v2 · v3" links and the rewrite button.
fn versions_html(date: &str, shown: u32, v: &Versions) -> String {
    let mut b = String::from("<div class=\"versions\"><span class=\"muted\">versions</span>");
    for n in &v.all {
        let label = if *n == v.current { format!("v{n} latest") } else { format!("v{n}") };
        if *n == shown {
            let _ = write!(b, "<b class=\"on\">{label}</b>");
        } else {
            let _ = write!(b, "<a href=\"/brief/{date}?v={n}\">{label}</a>");
        }
    }
    if v.rewriting {
        b.push_str("<span class=\"pill\">Writing… reload in a few minutes</span>");
    } else {
        let _ = write!(b, "<button class=\"rewrite\" onclick=\"rewrite(this, '{date}')\" title=\"Write this brief again with the current method and your feedback; this version is kept\">↻ Rewrite with my feedback</button>");
    }
    b.push_str("</div>");
    b
}

fn feedback_controls(existing: Option<&Feedback>, label: &str) -> String {
    let rating = existing.map(|f| f.rating.as_str()).unwrap_or("");
    let comment = existing.map(|f| f.comment.as_str()).unwrap_or("");
    format!(
        "<div class=\"fb\">{}<button class=\"r{}\" data-r=\"up\" onclick=\"rate(this)\">👍</button>\
         <button class=\"r{}\" data-r=\"down\" onclick=\"rate(this)\">👎</button>\
         <input placeholder=\"Comment for tomorrow's brief…\" value=\"{}\" onkeydown=\"if(event.key==='Enter')send(this)\">\
         <button onclick=\"send(this)\">Save</button><span class=\"ok\"></span></div>",
        if label.is_empty() { String::new() } else { format!("<span class=\"fbl\">{}</span>", esc(label)) },
        if rating == "up" { " sel" } else { "" },
        if rating == "down" { " sel" } else { "" },
        esc(comment)
    )
}

pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|c| match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (c as char).to_string(),
            _ => format!("%{c:02X}"),
        })
        .collect()
}

const JS: &str = r#"
async function post(el, rating) {
  const art = el.closest('.item'), fb = el.closest('.fb');
  const comment = fb.querySelector('input').value;
  const res = await fetch('/api/feedback', {method: 'POST', headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({brief_date: art.dataset.date, item_id: art.dataset.id, item_title: art.dataset.title, rating, comment})});
  fb.querySelector('.ok').textContent = res.ok ? 'saved' : 'error';
}
function rate(btn) {
  const fb = btn.closest('.fb'), was = btn.classList.contains('sel');
  fb.querySelectorAll('.r').forEach(b => b.classList.remove('sel'));
  if (!was) btn.classList.add('sel');
  post(btn, was ? '' : btn.dataset.r);
  // In a brief, a rating moves on to the next card, unless you're writing a comment.
  const card = btn.closest('.card');
  card?.dispatchEvent(new CustomEvent('rated', {detail: was ? '' : btn.dataset.r}));
  if (!was && card && !fb.querySelector('input').value.trim()) setTimeout(() => window.deck?.next(), 650);
}
function send(el) {
  const sel = el.closest('.fb').querySelector('.r.sel');
  post(el, sel ? sel.dataset.r : '');
}
async function rewrite(btn, date) {
  btn.disabled = true; btn.textContent = 'Rewriting… (a few minutes)';
  const r = await fetch('/api/regenerate/' + date, {method: 'POST'});
  if (!r.ok) { btn.textContent = 'Couldn\'t start'; return; }
  const t0 = Date.now();
  const poll = setInterval(async () => {
    const s = await fetch('/api/status').then(r => r.json()).catch(() => ({}));
    btn.textContent = `Rewriting… ${Math.round((Date.now() - t0) / 1000)}s`;
    if (s.generating === false) { clearInterval(poll); location.href = '/brief/' + date; }
  }, 5000);
}
async function gen(btn) {
  btn.disabled = true; btn.textContent = 'Starting…';
  await fetch('/api/generate', {method: 'POST'});
  location.reload();
}
if (document.querySelector('.pill') && /Writing/.test(document.body.textContent)) setTimeout(() => location.reload(), 15000);

// Brief page: draw each item's visual and reveal it step by step, in sync with the numbered steps.
const store = { get(k) { try { return localStorage.getItem(k); } catch (e) { return null; } },
                set(k, v) { try { localStorage.setItem(k, v); } catch (e) {} } };
const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = String(text); return e; };
const draw = {
  flow(v, box) {
    return (v.nodes || []).map((n, i) => {
      const g = el('div', 'part');
      if (i) g.append(el('span', 'arrow', '↓'));
      g.append(el('div', 'node' + (i === v.highlight ? ' hi' : ''), n));
      box.append(g); return g;
    });
  },
  compare(v, box) {
    return ['left', 'right'].map((k, i) => {
      const c = v[k] || {}, col = el('div', 'part col' + (i ? ' better' : '')), ul = el('ul');
      col.append(el('div', 'ctitle', c.title || ''));
      (c.points || []).forEach(p => ul.append(el('li', null, p)));
      col.append(ul); box.append(col); return col;
    });
  },
  bars(v, box) {
    const bars = v.bars || [], max = Math.max(1e-9, ...bars.map(b => +b.value || 0));
    return bars.map(b => {
      const row = el('div', 'part bar'), track = el('span', 'track'), fill = el('span', 'fill');
      fill.style.setProperty('--w', (100 * (+b.value || 0) / max) + '%');
      track.append(fill);
      row.append(el('span', 'blabel', b.label), track, el('span', 'bval', b.value + (v.unit ? ' ' + v.unit : '')));
      box.append(row); return row;
    });
  },
  timeline(v, box) {
    return (v.events || []).map(e => {
      const r = el('div', 'part ev');
      r.append(el('span', 'dot'), el('span', 'when', e.when), el('span', 'what', e.what));
      box.append(r); return r;
    });
  },
  stat(v, box) {
    const r = el('div', 'part'), num = el('div', 'num', v.value);
    r.append(num, el('div', 'slabel', v.label || ''));
    const m = String(v.value ?? '').match(/^(\D*)(\d+(?:\.\d+)?)(.*)$/);
    if (m) r.onshow = () => {
      const end = +m[2], dec = (m[2].split('.')[1] || '').length, t0 = performance.now();
      const tick = t => { const k = Math.min(1, (t - t0) / 900);
        num.textContent = m[1] + (end * (1 - Math.pow(1 - k, 3))).toFixed(dec) + m[3];
        if (k < 1) requestAnimationFrame(tick); };
      requestAnimationFrame(tick);
    };
    box.append(r); return [r];
  },
};
const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
document.querySelectorAll('.item').forEach(item => {
  const vis = item.querySelector('.vis');
  let parts = [];
  if (vis) {
    try { const v = JSON.parse(vis.querySelector('script').textContent), f = draw[v.type];
          if (f) { vis.classList.add(v.type); parts = f(v, vis); } } catch (e) {}
    if (!parts.length) { vis.remove(); item.classList.remove('has-vis'); }
  }
  const steps = [...item.querySelectorAll('.steps:not(.check) li')];
  const F = Math.max(parts.length, steps.length);
  if (F < 2) { item.querySelector('.ctl')?.remove(); parts.forEach(p => { p.classList.add('on'); p.onshow?.(); }); return; }
  let timer, frame = -1;
  const at = (f, n) => n ? Math.round(f * (n - 1) / (F - 1)) : -1;
  const show = (f, playing) => {
    frame = f;
    const pv = at(f, parts.length), ps = at(f, steps.length);
    parts.forEach((p, i) => { const on = i <= pv; if (on && !p.classList.contains('on')) p.onshow?.(); p.classList.toggle('on', on); });
    steps.forEach((s, i) => { s.classList.toggle('on', i <= ps); s.classList.toggle('cur', playing && i === ps); });
  };
  const stop = () => { clearInterval(timer); item.classList.remove('playing'); };
  const play = () => {
    stop(); parts.forEach(p => p.classList.remove('on')); void item.offsetWidth;
    item.classList.add('anim', 'playing'); show(0, true);
    timer = setInterval(() => frame >= F - 1 ? (stop(), show(F - 1, false)) : show(frame + 1, true), 2400);
  };
  steps.forEach((s, i) => s.onclick = () => { stop(); item.classList.add('anim'); show(Math.round(i * (F - 1) / Math.max(1, steps.length - 1)), true); });
  item.querySelector('.replay')?.addEventListener('click', play);
  if (reduce) { show(F - 1, false); return; }
  item.classList.add('anim');
  // The deck plays the animation the first time a card comes up, and shows it finished after that.
  let seen = false;
  item.activate = () => { if (seen) { stop(); show(F - 1, false); } else { seen = true; play(); } };
});
// F < 2 items above returned early: they just show their parts.
document.querySelectorAll('.item').forEach(i => { i.activate ||= () => {}; });
document.querySelectorAll('[data-key]').forEach(n => {
  const box = n.matches('input') ? n : n.querySelector('textarea');
  if (!box) return;
  const k = n.dataset.key, v = store.get(k);
  if (box.type === 'checkbox') { box.checked = v === '1'; box.onchange = () => store.set(k, box.checked ? '1' : ''); }
  else { if (v) box.value = v; box.oninput = () => store.set(k, box.value); }
});
const pills = [...document.querySelectorAll('.parts a')];
if (pills.length && 'IntersectionObserver' in window) {
  const io = new IntersectionObserver(es => es.forEach(e => { if (e.isIntersecting)
    pills.forEach(a => a.classList.toggle('on', a.getAttribute('href') === '#' + e.target.id)); }), {rootMargin: '-30% 0px -60% 0px'});
  document.querySelectorAll('section[id]').forEach(s => io.observe(s));
}
// ── explainer clips: one per beat, stepped with Next ──
document.querySelectorAll('figure.clips').forEach(fig => {
  const clips = JSON.parse(fig.dataset.clips), says = JSON.parse(fig.dataset.says || '[]');
  const vid = fig.querySelector('video'), say = fig.querySelector('.say'), cn = fig.querySelector('.cn');
  let i = 0;
  const show = (j, play = true) => {
    i = Math.max(0, Math.min(clips.length - 1, j));
    if (!vid.src.endsWith(clips[i])) vid.src = clips[i];
    say.textContent = says[i] || '';
    cn.textContent = `${i + 1} / ${clips.length}`;
    fig.querySelector('.cprev').disabled = i === 0;
    fig.querySelector('.cnext').disabled = i === clips.length - 1;
    if (play && !reduce) { vid.currentTime = 0; vid.play().catch(() => {}); }
  };
  fig.step = d => { if (i + d < 0 || i + d >= clips.length) return false; show(i + d); return true; };
  fig.start = () => show(i);
  fig.querySelector('.cprev').onclick = () => fig.step(-1);
  fig.querySelector('.cnext').onclick = () => fig.step(1);
  fig.querySelector('.creplay').onclick = () => show(i);
  vid.onclick = () => show(i);
  show(0, false);
});
// ── brief deck: one card at a time ──
(() => {
  const deck = document.querySelector('.deck');
  if (!deck) return;
  const cards = [...deck.querySelectorAll('.card')], dots = [...deck.querySelectorAll('.pip')];
  const prev = deck.querySelector('.decknav .prev'), next = deck.querySelector('.decknav .next');
  const key = 'pos:' + deck.dataset.date;
  let cur = -1, busy = false;
  const fromHash = () => { const n = parseInt(location.hash.slice(1)); return n >= 0 && n < cards.length ? n : null; };
  function go(i, instant) {
    i = Math.max(0, Math.min(cards.length - 1, i));
    if (i === cur || busy) return;
    const dir = i > cur ? 1 : -1, old = cards[cur], card = cards[i];
    const enter = () => {
      if (old) old.hidden = true;
      card.hidden = false;
      card.classList.remove('in-next', 'in-prev', 'out-next', 'out-prev');
      if (!instant && !reduce) { void card.offsetWidth; card.classList.add(dir > 0 ? 'in-next' : 'in-prev'); }
      cur = i; busy = false;
      dots.forEach((d, j) => d.classList.toggle('on', j === i));
      prev.disabled = i === 0;
      next.textContent = i === 0 ? 'Start →' : i === cards.length - 1 ? 'Done' : 'Next →';
      next.disabled = i === cards.length - 1;
      history.replaceState(null, '', '#' + i);
      store.set(key, String(i));
      card.activate?.();
      cards.forEach(c => c !== card && c.querySelector('video')?.pause());
      const vid = card.querySelector('video');
      const fig = card.querySelector('figure.clips');
      if (fig && !fig.dataset.played) { fig.dataset.played = '1'; fig.start(); }
      else if (vid && !fig && !reduce && !vid.dataset.played) { vid.dataset.played = '1'; vid.play().catch(() => {}); }
      if (card.classList.contains('end')) summarize();
      if (deck.getBoundingClientRect().top < 0) deck.scrollIntoView({behavior: instant ? 'auto' : 'smooth'});
    };
    if (old && !instant && !reduce) {
      busy = true;
      old.classList.add(dir > 0 ? 'out-next' : 'out-prev');
      setTimeout(enter, 220);
    } else enter();
  }
  function summarize() {
    const items = cards.filter(c => c.classList.contains('item') && !c.classList.contains('end'));
    const up = items.filter(c => c.querySelector('.r.sel[data-r=up]')).length, down = items.filter(c => c.querySelector('.r.sel[data-r=down]')).length;
    cards[cards.length - 1].querySelector('.rated').textContent =
      `You rated ${up + down} of ${items.length} items: ${up} 👍, ${down} 👎. Tomorrow's brief learns from these.`;
  }
  cards.forEach((c, i) => {
    c.hidden = true;
    c.addEventListener('rated', e => { dots[i].classList.remove('up', 'down'); if (e.detail) dots[i].classList.add(e.detail); });
  });
  // Next and Back step through a card's clips before leaving the card.
  const clipsOf = () => cards[cur]?.querySelector('figure.clips');
  const fwd = () => { if (!clipsOf()?.step(1)) go(cur + 1); };
  const back = () => { if (!clipsOf()?.step(-1)) go(cur - 1); };
  window.deck = { next: () => go(cur + 1), prev: () => go(cur - 1) };
  prev.onclick = back;
  next.onclick = fwd;
  deck.querySelectorAll('[data-to]').forEach(b => b.addEventListener('click', e => { e.preventDefault(); go(+b.dataset.to); }));
  document.addEventListener('keydown', e => {
    if (e.target.closest('input, textarea') || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === 'ArrowRight' || e.key === 'j') fwd();
    else if (e.key === 'ArrowLeft' || e.key === 'k') back();
    else if (e.key === '[') document.querySelector('.briefnav a.older')?.click();
    else if (e.key === ']') document.querySelector('.briefnav a.newer')?.click();
  });
  // Swipe on phones.
  let x0 = null;
  deck.addEventListener('touchstart', e => { x0 = e.touches[0].clientX; }, {passive: true});
  deck.addEventListener('touchend', e => {
    if (x0 == null) return;
    const dx = e.changedTouches[0].clientX - x0; x0 = null;
    if (Math.abs(dx) > 60) go(cur + (dx < 0 ? 1 : -1));
  });
  addEventListener('hashchange', () => { const n = fromHash(); if (n != null) go(n); });
  go(fromHash() ?? (+store.get(key) || 0), true);
})();


"#;

const CSS: &str = r#"
/* Like the terminal app: black, white, one orange. High contrast, monospace, square corners. */
:root { color-scheme: dark;
  --bg:#000; --fg:#fff; --muted:#b3b3b3; --faint:#7a7a7a; --line:#333; --line2:#555; --card:#0a0a0a; --panel:#111; --chip:#1a1a1a;
  --accent:#ff8700; --accent2:#ffaf5f; --warn:#ffaf00; --purple:#d7afff; --red:#ff5f5f; --blue:#5fd7ff; --green:#5fd787;
  --mono: "JetBrains Mono", "SF Mono", ui-monospace, Menlo, Consolas, monospace; }
* { box-sizing: border-box; }
html { background: var(--bg); }
body { margin:0; background:var(--bg); color:var(--fg); font: 15px/1.6 var(--mono); -webkit-font-smoothing: antialiased; }
::selection { background: var(--accent); color: #000; }
header { display:flex; gap:24px; align-items:center; padding:12px 20px; border-bottom:1px solid var(--line); font-size:13px; }
header .brand { font-weight:700; color:var(--accent); text-decoration:none; display:flex; align-items:center; gap:8px; letter-spacing: .02em; }
header .brand img { width:20px; height:20px; }
header nav { display:flex; gap:16px; flex-wrap:wrap; }
header nav a { color:var(--muted); text-decoration:none; } header nav a.on { color:var(--fg); }
header nav a.on::before { content: "› "; color: var(--accent); }
main { max-width: 760px; margin: 0 auto; padding: 24px 16px 80px; }
h1 { font-size: 26px; line-height:1.25; margin: 8px 0 16px; font-weight: 700; letter-spacing: -.01em; }
h2 { font-size: 12px; text-transform: uppercase; letter-spacing:.12em; color:var(--muted); margin: 40px 0 12px; font-weight: 600; }
h3 { font-size: 19px; margin: 0 0 6px; }
a { color: var(--accent); text-underline-offset: 3px; } a:hover { color: var(--accent2); }
.acc { color: var(--accent); }
.muted { color: var(--muted); } .small { font-size: 12px; }
.row { display:flex; align-items:center; justify-content:space-between; gap:12px; flex-wrap:wrap; }
.summary { font-size: 16px; color: var(--muted); } .summary p { margin: 0 0 8px; } .summary strong { color: var(--fg); }
.item, .entry { background: var(--card); border:1px solid var(--line); border-radius: 0; padding: 18px 20px; margin: 12px 0; }
.item p:first-of-type { margin-top: 8px; }
.meta { display:flex; gap:8px; flex-wrap:wrap; align-items:center; font-size: 12px; }
.kind, .proj, .pill, .ltype, .chip { font-size: 11px; border: 1px solid var(--line2); border-radius: 0; padding: 1px 7px; color: var(--muted); text-decoration:none; text-transform: uppercase; letter-spacing: .06em; }
.proj { color: var(--fg); } .pill { color: var(--accent); border-color: var(--accent); }
.links { list-style:none; padding:0; margin: 12px 0 0; } .links li { margin: 6px 0; display:flex; gap:8px; align-items:baseline; }
.links li::before { content: "→"; color: var(--accent); }
.fb { display:flex; gap:6px; align-items:center; margin-top: 16px; padding-top: 12px; border-top: 1px dashed var(--line2); font-size: 13px; }
.fb input { flex:1; min-width:0; }
.fb .ok { color: var(--muted); font-size: 12px; min-width: 36px; }
button, input, textarea { font: 13px var(--mono); color: var(--fg); background: var(--bg); border: 1px solid var(--line2); border-radius: 0; padding: 7px 12px; }
input:focus, textarea:focus { outline: none; border-color: var(--accent); }
button { cursor: pointer; } button:hover { border-color: var(--fg); }
button.sel { background: var(--accent); border-color: var(--accent); color: #000; }
textarea { width: 100%; font-size: 14px; padding: 12px; }
.menu { list-style:none; padding:0; } .menu li a { display:grid; grid-template-columns: 110px 1fr; gap: 2px 14px; padding: 14px 6px; border-bottom: 1px solid var(--line); color: var(--fg); text-decoration:none; }
.menu li a:hover { background: var(--chip); } .menu li a:hover .head::before { content: "› "; color: var(--accent); }
.menu .date { font-size: 12px; color: var(--muted); grid-row: span 2; padding-top: 3px; }
.menu .pill { justify-self:start; grid-column: 2; }
.menu .head { font-size: 16px; font-weight: 600; }
@media (max-width: 520px) { .menu li a { grid-template-columns: 1fr; } .menu .date { grid-row:auto; } }
.chips { display:flex; gap:6px; flex-wrap:wrap; } .chip.on { background: var(--accent); color: #000; }
.note { border: 1px solid var(--accent); padding: 10px 14px; } .err { color: var(--red); }
code { font-size: 13px; color: var(--accent2); }
.prompt { margin: 0 0 4px; color: var(--fg); }

/* brief page */
.think { border:1px solid var(--line); border-left: 3px solid var(--warn); padding: 14px 18px; margin: 18px 0; font-size: 14px; }
.think p { margin: 0 0 10px; } .think b { color: var(--warn); }
.think textarea { margin-bottom: 8px; }
.think .answer { margin: 4px 0 0; padding: 10px 12px; background: var(--chip); }
.terms { display:flex; flex-wrap:wrap; gap:6px; margin: 14px 0; }
.term { max-width: 100%; font-size: 12px; line-height:1.4; border:1px solid var(--line); padding: 3px 8px; color: var(--muted); }
.term b { color: var(--fg); font-weight: 600; }
.research { margin: 14px 0; font-size: 12px; color: var(--muted); border: 1px solid var(--line); padding: 8px 12px; }
.research summary { cursor: pointer; list-style: none; } .research summary::-webkit-details-marker { display: none; }
.research b { color: var(--fg); } .research ul { margin: 8px 0 0; padding-left: 18px; } .research li { margin: 2px 0; }
.research li::marker { content: "$ "; color: var(--faint); }
.item { --k: var(--muted); border-left: 3px solid var(--k); }
.k-feedback { --k: var(--warn); } .k-connection { --k: var(--purple); } .k-blindspot { --k: var(--red); }
.k-resource { --k: var(--accent); } .k-next_step { --k: var(--green); } .k-question { --k: var(--accent); }
.k-time { --k: var(--blue); } .k-reprioritise { --k: var(--accent); } .k-gap { --k: var(--red); } .k-learn { --k: var(--blue); }
.item .kind { color: var(--k); border-color: var(--k); }
.item h3 { margin: 8px 0 12px; font-size: 19px; line-height: 1.3; font-weight: 700; }
.stage { display:grid; gap: 16px; }
.has-vis .stage { grid-template-columns: minmax(0, 1.15fr) minmax(0, 1fr); align-items: start; }
@media (max-width: 640px) { .has-vis .stage { grid-template-columns: 1fr; } }
.steps { list-style:none; padding:0; margin:0; counter-reset: s; font-size: 14px; line-height: 1.5; }
.steps li { counter-increment: s; display:flex; gap: 10px; padding: 6px 8px; cursor: pointer; transition: opacity .4s, background .3s; }
.steps li::before { content: counter(s) "."; flex:none; min-width: 20px; color: var(--faint); font-weight: 700; transition: color .3s; }
.anim .steps li:not(.on) { opacity: .3; }
.steps li.cur { background: var(--chip); } .steps li.cur::before { color: var(--k); }
.steps.check li { cursor: default; } .steps.check li::before { display:none; }
.steps.check label { display:flex; gap: 10px; align-items: flex-start; cursor: pointer; }
.steps.check input { margin-top: 4px; accent-color: var(--accent); width: 15px; height: 15px; padding: 0; }
.steps.check input:checked + span { color: var(--faint); text-decoration: line-through; }
.ctl { margin-top: 8px; } .ctl button { font-size: 11px; padding: 3px 10px; color: var(--muted); }
.playing .ctl button { border-color: var(--k); color: var(--k); }
.vis { background: var(--panel); border:1px solid var(--line); padding: 16px; min-height: 120px; display:flex; font-size: 13px; line-height: 1.35; }
.vis .part { transition: opacity .5s ease, transform .5s ease; }
.anim .vis .part:not(.on) { opacity: 0; transform: translateY(8px); }
.vis.flow { flex-direction: column; align-items: center; justify-content: center; }
.vis.flow .part { display:flex; flex-direction: column; align-items:center; width: 100%; }
.vis .arrow { color: var(--faint); line-height: 1; padding: 3px 0; }
.vis .node { border: 1px solid var(--line2); background: var(--bg); padding: 7px 12px; text-align:center; width: min(100%, 240px); }
.vis .node.hi { border-color: var(--k); color: var(--k); font-weight: 700; box-shadow: 0 0 0 3px color-mix(in srgb, var(--k) 25%, transparent); }
.on .node.hi { animation: pulse 1.6s ease-in-out 2; }
@keyframes pulse { 50% { box-shadow: 0 0 0 8px color-mix(in srgb, var(--k) 10%, transparent); } }
.vis.compare { gap: 10px; align-items: stretch; }
.vis .col { flex:1; min-width:0; border:1px solid var(--line2); padding: 10px 12px; background: var(--bg); }
.vis .col:not(.better) { border-color: var(--red); } .vis .col:not(.better) .ctitle { color: var(--red); }
.vis .col.better { border-color: var(--green); }
.vis .ctitle { font-weight: 700; font-size: 11px; text-transform: uppercase; letter-spacing: .08em; color: var(--muted); margin-bottom: 6px; }
.vis .better .ctitle { color: var(--green); }
.vis .col ul { margin:0; padding-left: 16px; } .vis .col li { margin: 3px 0; }
.vis.bars, .vis.timeline { flex-direction: column; justify-content: center; gap: 10px; }
.vis .bar { display:grid; grid-template-columns: minmax(60px, 34%) 1fr auto; gap: 10px; align-items:center; }
.vis .blabel { overflow:hidden; text-overflow: ellipsis; white-space: nowrap; }
.vis .track { height: 12px; background: var(--chip); overflow:hidden; }
.vis .fill { display:block; height:100%; width: var(--w); background: var(--k); transition: width 1s cubic-bezier(.2,.8,.2,1) .15s; }
.anim .vis .bar:not(.on) .fill { width: 0; }
.vis .bval { color: var(--muted); font-variant-numeric: tabular-nums; font-size: 12px; }
.vis.timeline { gap: 0; padding-left: 20px; }
.vis .ev { position: relative; display:grid; grid-template-columns: 92px 1fr; gap: 10px; padding: 7px 0 7px 14px; border-left: 1px solid var(--line2); }
.vis .dot { position:absolute; left: -6px; top: 12px; width: 11px; height: 11px; background: var(--bg); border: 2px solid var(--k); }
.vis .ev.on .dot { background: var(--k); }
.vis .when { color: var(--muted); font-variant-numeric: tabular-nums; }
.vis.stat { align-items: center; justify-content: center; text-align: center; }
.vis .num { font-size: 48px; font-weight: 700; line-height:1.1; color: var(--k); font-variant-numeric: tabular-nums; }
.vis .slabel { color: var(--muted); margin-top: 4px; }
.video { margin: 0 0 18px; border: 1px solid var(--k); background: #000; }
.video video { display:block; width: 100%; aspect-ratio: 16 / 9; background: #000; }
.video figcaption { padding: 8px 12px; font-size: 12px; border-top: 1px solid var(--line); }
.clips video { cursor: pointer; }
.clips .say { margin: 2px 0 10px; font-size: 15px; line-height: 1.45; color: var(--fg); min-height: 2.9em; }
.clipnav { display: flex; align-items: center; gap: 8px; }
.clipnav button { padding: 2px 10px; font-size: 14px; } .clipnav .cnext:not(:disabled) { border-color: var(--accent); color: var(--accent); }
.clipnav .cn { font-variant-numeric: tabular-nums; color: var(--muted); min-width: 44px; text-align: center; }
.clipnav .ctitle { margin-left: auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
@media (prefers-reduced-motion: reduce) { .vis .part, .vis .fill, .steps li { transition: none; } .on .node.hi { animation: none; } }
[hidden] { display: none !important; }
button.primary, a.btn { background: var(--accent); color: #000; border-color: var(--accent); font-weight: 700; }
button.primary:hover { background: var(--accent2); border-color: var(--accent2); }
a.btn { font-size: 13px; padding: 7px 12px; text-decoration: none; }
button.small { font-size: 11px; padding: 3px 9px; }
button:disabled { opacity: .35; cursor: default; }

/* between briefs */
.briefnav { display:grid; grid-template-columns: 1fr auto 1fr; align-items:center; gap: 10px; margin: -8px 0 14px; font-size: 13px; }
.briefnav a, .briefnav span { text-decoration: none; color: var(--muted); padding: 6px 10px; }
.briefnav a:hover { background: var(--chip); color: var(--fg); }
.briefnav .all { color: var(--fg); font-weight: 700; text-align: center; }
.briefnav .newer { justify-self: end; } .briefnav .older { justify-self: start; } .briefnav .off { opacity: .45; }

/* the deck */
.deck { scroll-margin-top: 12px; }
.pips { display:flex; gap: 4px; margin-bottom: 14px; }
.pip { flex: 1; height: 4px; min-width: 0; padding: 0; border: 0; background: var(--line); transition: background .3s, transform .3s; }
.pip:hover { transform: scaleY(2); background: var(--line2); }
.pip.up { background: var(--green); } .pip.down { background: var(--red); }
.pip.on { background: var(--accent); }
.card { min-height: 340px; }
.card.intro h1 { margin-top: 4px; }
.cards .item { margin: 0; padding: 22px 24px; }
.in-next { animation: in-next .32s cubic-bezier(.2,.8,.2,1); } .in-prev { animation: in-prev .32s cubic-bezier(.2,.8,.2,1); }
.out-next { animation: out-next .18s ease-in forwards; } .out-prev { animation: out-prev .18s ease-in forwards; }
@keyframes in-next { from { opacity: 0; transform: translateX(32px); } }
@keyframes in-prev { from { opacity: 0; transform: translateX(-32px); } }
@keyframes out-next { to { opacity: 0; transform: translateX(-32px); } }
@keyframes out-prev { to { opacity: 0; transform: translateX(32px); } }
.decknav { display:flex; justify-content: space-between; align-items:center; margin-top: 16px; }
@media (max-width: 520px) { .decknav .hint { display: none; } }
.count { font-size: 12px; color: var(--faint); font-variant-numeric: tabular-nums; }
.toc { padding-left: 0; list-style: none; margin: 18px 0; font-size: 14px; counter-reset: t; border-top: 1px solid var(--line); }
.toc li { counter-increment: t; border-bottom: 1px solid var(--line); }
.toc a { display:flex; gap: 10px; align-items: baseline; padding: 8px 6px; color: var(--fg); text-decoration: none; }
.toc a:hover { background: var(--chip); } .toc a::before { content: counter(t, decimal-leading-zero); color: var(--faint); font-size: 12px; min-width: 18px; }
.toc a:hover::before { content: "›"; color: var(--accent); }
.toc .kind { flex: none; color: var(--k, var(--muted)); border-color: var(--k, var(--line2)); }
.takeaway { margin: 4px 0 18px; padding: 12px 14px; border: 1px solid var(--k); background: color-mix(in srgb, var(--k) 12%, #000); font-size: 16px; line-height: 1.45; font-weight: 700; }
.takeaway span { display:block; font-size: 11px; text-transform: uppercase; letter-spacing: .12em; color: var(--k); margin-bottom: 4px; }
.about { font-size: 15px; margin: 0 0 16px; color: #e6e6e6; } .about p { margin: 0 0 8px; }
.why { margin-top: 16px; padding: 10px 14px; border-left: 2px solid var(--line2); background: var(--panel); font-size: 13px; line-height: 1.5; color: var(--muted); }
.why b { display:block; color: var(--fg); font-size: 11px; text-transform: uppercase; letter-spacing: .1em; margin-bottom: 4px; }
.why p { margin: 4px 0 0; }
.why .obj { font-size: 11px; border: 1px solid var(--line2); padding: 1px 7px; color: var(--fg); } .why .obj.none { color: var(--red); border-color: var(--red); }
.fbl { color: var(--muted); font-size: 12px; margin-right: 2px; }
.end h2 { margin-top: 0; color: var(--fg); font-size: 18px; text-transform: none; letter-spacing: 0; }
.endnav { display:flex; gap: 16px; flex-wrap: wrap; font-size: 13px; margin-top: 18px; }
.versions { display:flex; gap: 10px; flex-wrap: wrap; align-items: center; margin-top: 18px; padding-top: 12px; border-top: 1px dashed var(--line2); font-size: 12px; }
.versions a { color: var(--muted); } .versions b.on { color: var(--accent); }
.versions .rewrite { margin-left: auto; color: var(--accent); border-color: var(--accent); }
"#;
