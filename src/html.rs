//! Server-rendered pages. No JS framework; a little inline JS posts feedback.

use crate::store::{Brief, Entry, Feedback, Store};
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

fn layout(title: &str, active: &str, body: &str) -> String {
    let nav = [("/", "Briefs"), ("/log", "Journal"), ("/feedback", "Feedback"), ("/objectives", "Objectives")]
        .iter()
        .map(|(href, label)| {
            let cls = if *label == active { " class=\"on\"" } else { "" };
            format!("<a href=\"{href}\"{cls}>{label}</a>")
        })
        .collect::<String>();
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" href="/icon.svg"><link rel="apple-touch-icon" href="/icon.png">
<title>{title}</title><style>{CSS}</style></head>
<body><header><a class="brand" href="/"><img src="/icon.svg" alt="">brief</a><nav>{nav}</nav></header>
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
        b.push_str("<p class=\"note\">You haven't set objectives yet. <a href=\"/onboarding\">Let Claude interview you</a> so the brief knows what you're working towards.</p>");
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
        let tag = if d == today { "<span class=\"pill\">today</span>" } else { "" };
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
    layout("Daily briefs", "Briefs", &b)
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
        "resource" => "read",
        "time" => "your time",
        "reprioritise" => "reprioritise",
        other => other,
    }
}

/// One brief, shown as a deck: an intro card, then one card per item (rate it, and it moves on
/// to the next), then a closing card. ← → keys or the buttons move between cards; the bar at the
/// top moves between briefs.
pub fn brief(brief: &Brief, feedback: &[Feedback], older: Option<NaiveDate>, newer: Option<NaiveDate>) -> String {
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
        "<section class=\"card intro\"><p class=\"muted small\">~{} min · {} items</p><h1>{}</h1><div class=\"summary\">{}</div>",
        brief.reading_minutes,
        items.len(),
        esc(&brief.headline),
        md(&brief.summary)
    );
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
    if let Some(c) = brief.cost_usd {
        let _ = write!(b, "<p class=\"muted small\">Generated with Claude Code · ${c:.2} equivalent usage</p>");
    }
    b.push_str("</section></div><div class=\"decknav\"><button class=\"prev\">← Back</button><span class=\"muted small hint\">← → keys</span><button class=\"next primary\">Next →</button></div></div>");
    layout(&brief.headline, "Briefs", &b)
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

pub fn log(entries: &[Entry], project: Option<&str>) -> String {
    let mut projects: Vec<&str> = entries.iter().map(|e| e.project.as_str()).collect();
    projects.sort_unstable();
    projects.dedup();

    let mut b = String::from("<h1>Journal</h1><p class=\"muted\">Everything your chatbots have logged.</p><p class=\"chips\">");
    let _ = write!(b, "<a href=\"/log\" class=\"chip{}\">all</a>", if project.is_none() { " on" } else { "" });
    for p in &projects {
        let on = if project == Some(*p) { " on" } else { "" };
        let _ = write!(b, "<a href=\"/log?project={}\" class=\"chip{on}\">{}</a>", esc(&url_encode(p)), esc(p));
    }
    b.push_str("</p>");
    let shown: Vec<_> = entries.iter().rev().filter(|e| project.map_or(true, |p| e.project == p)).collect();
    if shown.is_empty() {
        b.push_str("<p class=\"muted\">Nothing logged yet. Connect a chatbot with <code>brief connect</code>.</p>");
    }
    let mut day = String::new();
    for e in shown {
        let d = e.ts.format("%A, %B %-d").to_string();
        if d != day {
            let _ = write!(b, "<h3 class=\"day\">{d}</h3>");
            day = d;
        }
        let _ = write!(
            b,
            "<div class=\"entry\"><div class=\"meta\"><span class=\"proj\">{0}</span><span class=\"kind k-{1}\">{1}</span>\
             <span class=\"muted small\">{2} · via {3}</span></div><p>{4}</p>",
            esc(&e.project),
            esc(&e.kind),
            e.ts.format("%H:%M"),
            esc(&e.source),
            esc(&e.summary)
        );
        if !e.details.is_empty() {
            let _ = write!(b, "<details><summary>details</summary>{}</details>", md(&e.details));
        }
        b.push_str("</div>");
    }
    layout("Journal", "Journal", &b)
}

pub fn feedback_page(feedback: &[Feedback]) -> String {
    let mut b = String::from("<h1>Your feedback</h1><p class=\"muted\">Fed into every new brief so it learns what is useful to you.</p>");
    if feedback.is_empty() {
        b.push_str("<p class=\"muted\">No feedback yet. Rate items inside a brief.</p>");
    }
    b.push_str("<ul class=\"menu\">");
    for f in feedback.iter().rev() {
        let icon = match f.rating.as_str() {
            "up" => "👍",
            "down" => "👎",
            _ => "💬",
        };
        let title = if f.item_title.is_empty() { "Whole brief" } else { &f.item_title };
        let _ = write!(
            b,
            "<li><a href=\"/brief/{}\"><span class=\"date\">{icon} {}</span><span class=\"head\">{}</span><span class=\"muted small\">{}</span></a></li>",
            f.brief_date,
            f.brief_date.format("%b %-d"),
            esc(title),
            esc(&f.comment)
        );
    }
    b.push_str("</ul>");
    layout("Feedback", "Feedback", &b)
}

/// The objectives interview: a chat with Claude that ends in a list of objectives to edit and save.
pub fn onboarding(existing: &[String]) -> String {
    let lead = if existing.is_empty() {
        "Every brief compares where your time goes with what you're working towards, so it needs to know your goals well: \
         why they matter, what done looks like, by when, and how they rank. Claude will ask, one question at a time. About ten minutes."
    } else {
        "Claude reads your current objectives and asks about what's missing or vague: why each matters, what done looks like, \
         deadlines, time and ranking. Nothing changes until you save."
    };
    let b = format!(
        "<div class=\"row\"><h1>Your objectives</h1><button class=\"restart small\" hidden>Start over</button></div>\
         <p class=\"muted\">{lead}</p>\
         <div class=\"chat\" id=\"chat\"></div><div class=\"sugg\" id=\"sugg\"></div>\
         <form class=\"ask\" id=\"ask\"><textarea rows=\"2\" placeholder=\"Your answer… (⏎ to send, ⇧⏎ for a new line)\"></textarea><button class=\"primary\">Send</button></form>\
         <div class=\"proposal\" id=\"proposal\" hidden><h2>Proposed objectives</h2>\
         <p class=\"muted small\">Ranked, most important first. Edit anything, reorder with the arrows, or keep talking to change them.</p>\
         <div id=\"objs\"></div><p class=\"row\"><button class=\"primary save\">Save these objectives</button>\
         <span class=\"saved muted\"></span></p></div>"
    );
    layout("Your objectives", "Objectives", &b)
}

pub fn objectives(text: &str) -> String {
    let b = format!(
        "<div class=\"row\"><h1>Objectives</h1><a class=\"btn\" href=\"/onboarding\">Refine them with Claude →</a></div>\
         <p class=\"muted\">What you're working towards. Markdown is fine. Every brief is written against this.</p>\
         <form method=\"post\" action=\"/objectives\"><textarea name=\"text\" rows=\"18\" placeholder=\"- Ship chessLLM v1 by December&#10;- Get better at systems programming in Rust&#10;- …\">{}</textarea>\
         <p><button type=\"submit\">Save</button></p></form>",
        esc(text)
    );
    layout("Objectives", "Objectives", &b)
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
  window.deck = { next: () => go(cur + 1), prev: () => go(cur - 1) };
  prev.onclick = () => go(cur - 1);
  next.onclick = () => go(cur + 1);
  deck.querySelectorAll('[data-to]').forEach(b => b.addEventListener('click', e => { e.preventDefault(); go(+b.dataset.to); }));
  document.addEventListener('keydown', e => {
    if (e.target.closest('input, textarea') || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === 'ArrowRight' || e.key === 'j') go(cur + 1);
    else if (e.key === 'ArrowLeft' || e.key === 'k') go(cur - 1);
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

// ── objectives interview ──
(() => {
  const chat = document.getElementById('chat');
  if (!chat) return;
  const form = document.getElementById('ask'), box = form.querySelector('textarea'), send = form.querySelector('button');
  const sugg = document.getElementById('sugg'), proposal = document.getElementById('proposal'), objs = document.getElementById('objs');
  const restart = document.querySelector('.restart');
  const KEY = 'interview';
  let messages = [], objectives = [];
  try { ({messages = [], objectives = []} = JSON.parse(store.get(KEY) || '{}')); } catch (e) {}
  const save = () => store.set(KEY, JSON.stringify({messages, objectives}));

  function bubble(m) {
    const d = el('div', 'msg ' + m.role);
    d.innerHTML = m.text.replace(/[&<>]/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;'})[c]).replace(/\n/g, '<br>');
    chat.append(d);
    return d;
  }
  function render() {
    chat.replaceChildren();
    messages.forEach(bubble);
    restart.hidden = !messages.length;
    renderObjectives();
  }
  const FIELDS = [['why', 'Why it matters'], ['done_when', 'Done when'], ['deadline', 'Deadline'], ['time', 'Time'], ['obstacles', 'In the way'], ['notes', 'Notes']];
  function renderObjectives() {
    proposal.hidden = !objectives.length;
    objs.replaceChildren(...objectives.map((o, i) => {
      const card = el('div', 'obj'), head = el('div', 'ohead');
      const title = el('input', 'otitle'); title.value = o.title || ''; title.oninput = () => { o.title = title.value; save(); };
      const rank = el('span', 'rank', i + 1);
      const up = el('button', 'small', '↑'), down = el('button', 'small', '↓'), del = el('button', 'small', '×');
      up.disabled = i === 0; down.disabled = i === objectives.length - 1;
      up.onclick = () => { [objectives[i - 1], objectives[i]] = [objectives[i], objectives[i - 1]]; save(); renderObjectives(); };
      down.onclick = () => { [objectives[i + 1], objectives[i]] = [objectives[i], objectives[i + 1]]; save(); renderObjectives(); };
      del.onclick = () => { objectives.splice(i, 1); save(); renderObjectives(); };
      head.append(rank, title, up, down, del);
      card.append(head);
      FIELDS.forEach(([k, label]) => {
        const row = el('label', 'ofield'), inp = el('input');
        inp.value = o[k] || ''; inp.placeholder = '—';
        inp.oninput = () => { o[k] = inp.value; save(); };
        row.append(el('span', null, label), inp);
        card.append(row);
      });
      return card;
    }));
  }
  async function ask() {
    sugg.replaceChildren();
    const typing = bubble({role: 'assistant', text: '…'}); typing.classList.add('typing');
    chat.lastChild.scrollIntoView({behavior: 'smooth', block: 'nearest'});
    box.disabled = send.disabled = true;
    try {
      const res = await fetch('/api/interview', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({messages})});
      if (!res.ok) throw new Error(await res.text());
      const r = await res.json();
      messages.push({role: 'assistant', text: r.message || '(no reply)'});
      if (r.objectives?.length) objectives = r.objectives;
      save(); render();
      (r.suggestions || []).forEach(t => { const b = el('button', 'chip', t); b.onclick = () => { box.value = t; form.requestSubmit(); }; sugg.append(b); });
      if (r.objectives?.length) proposal.scrollIntoView({behavior: 'smooth'});
    } catch (e) {
      typing.classList.remove('typing'); typing.classList.add('error');
      typing.textContent = 'Claude couldn\'t answer: ' + e.message + ' — press Send to try again.';
    } finally {
      box.disabled = send.disabled = false; box.focus();
    }
  }
  form.onsubmit = e => {
    e.preventDefault();
    const text = box.value.trim();
    if (text) { messages.push({role: 'user', text}); box.value = ''; save(); render(); }
    else if (messages.length && messages[messages.length - 1].role === 'assistant' && !chat.querySelector('.error')) return;
    ask();
  };
  box.onkeydown = e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); form.requestSubmit(); } };
  restart.onclick = () => { if (confirm('Start the interview over? (Saved objectives stay until you save new ones.)')) { messages = []; objectives = []; save(); render(); ask(); } };
  proposal.querySelector('.save').onclick = async e => {
    const msg = proposal.querySelector('.saved');
    const res = await fetch('/api/objectives', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({objectives})});
    if (!res.ok) { msg.textContent = 'Couldn\'t save.'; return; }
    msg.innerHTML = 'Saved. Every brief is now written against these. <a href="/objectives">See them</a> · <a href="/" class="gen">Write a brief now</a>';
    msg.querySelector('.gen').onclick = async ev => { ev.preventDefault(); await fetch('/api/generate', {method: 'POST'}); location.href = '/'; };
    store.set(KEY, '');
  };
  render();
  if (!messages.length) ask();
})();

"#;

const CSS: &str = r#"
:root { --bg:#fbfaf7; --fg:#1d1d1b; --muted:#6b6a65; --line:#e6e3dc; --card:#fff; --accent:#c2410c; --chip:#f1eee7; --panel:#f5f3ee;
  --warn:#b45309; --purple:#7c3aed; --red:#b91c1c; --blue:#0369a1; --green:#15803d; }
@media (prefers-color-scheme: dark) { :root { --bg:#161614; --fg:#ecebe6; --muted:#9a988f; --line:#2c2b28; --card:#1e1e1b; --accent:#fb923c; --chip:#272622; --panel:#121210;
  --warn:#f5b84c; --purple:#b49cfc; --red:#f87171; --blue:#7dc3f5; --green:#6fd59a; } }
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font: 17px/1.6 ui-serif, Georgia, serif; }
header { display:flex; gap:24px; align-items:center; padding:14px 20px; border-bottom:1px solid var(--line); font-family: ui-sans-serif, system-ui, sans-serif; font-size:14px; }
header .brand { font-weight:700; color:var(--accent); text-decoration:none; display:flex; align-items:center; gap:8px; }
header .brand img { width:22px; height:22px; }
header nav { display:flex; gap:16px; flex-wrap:wrap; }
header nav a { color:var(--muted); text-decoration:none; } header nav a.on { color:var(--fg); }
main { max-width: 720px; margin: 0 auto; padding: 24px 16px 80px; }
h1 { font-size: 30px; line-height:1.2; margin: 8px 0 16px; }
h2 { font: 600 13px ui-sans-serif, system-ui, sans-serif; text-transform: uppercase; letter-spacing:.08em; color:var(--muted); margin: 40px 0 12px; }
h3 { font-size: 20px; margin: 0 0 6px; }
a { color: var(--accent); }
.muted { color: var(--muted); } .small { font-size: 13px; }
.row { display:flex; align-items:center; justify-content:space-between; gap:12px; flex-wrap:wrap; }
.summary { font-size: 18px; }
.item, .entry { background: var(--card); border:1px solid var(--line); border-radius: 10px; padding: 18px 20px; margin: 12px 0; }
.item p:first-of-type { margin-top: 8px; }
.meta { display:flex; gap:8px; flex-wrap:wrap; align-items:center; font: 12px ui-sans-serif, system-ui, sans-serif; }
.kind, .proj, .pill, .ltype, .chip { font: 12px ui-sans-serif, system-ui, sans-serif; background: var(--chip); border-radius: 99px; padding: 2px 9px; color: var(--muted); text-decoration:none; }
.proj { color: var(--fg); } .pill { color: var(--accent); }
.entry .k-blindspot, .entry .k-blocker { color: var(--red); } .entry .k-idea { color: var(--purple); } .entry .k-learning { color: var(--blue); } .entry .k-progress { color: var(--green); }
.links { list-style:none; padding:0; margin: 10px 0 0; } .links li { margin: 6px 0; display:flex; gap:8px; align-items:baseline; }
.fb { display:flex; gap:6px; align-items:center; margin-top: 14px; padding-top: 12px; border-top: 1px dashed var(--line); font: 14px ui-sans-serif, system-ui, sans-serif; }
.fb input { flex:1; min-width:0; }
.fb .ok { color: var(--muted); font-size: 12px; min-width: 36px; }
button, input, textarea { font: 14px ui-sans-serif, system-ui, sans-serif; color: var(--fg); background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 7px 12px; }
button { cursor: pointer; } button:hover { border-color: var(--muted); }
button.sel { background: var(--chip); border-color: var(--accent); }
textarea { width: 100%; font: 15px/1.5 ui-monospace, Menlo, monospace; padding: 12px; }
.menu { list-style:none; padding:0; } .menu li a { display:grid; grid-template-columns: 110px 1fr; gap: 2px 14px; padding: 14px 4px; border-bottom: 1px solid var(--line); color: var(--fg); text-decoration:none; }
.menu li a:hover { background: var(--chip); }
.menu .date { font: 600 13px ui-sans-serif, system-ui, sans-serif; color: var(--muted); grid-row: span 2; padding-top: 3px; }
.menu .pill { justify-self:start; grid-column: 2; }
.menu .head { font-size: 18px; }
@media (max-width: 520px) { .menu li a { grid-template-columns: 1fr; } .menu .date { grid-row:auto; } }
.chips { display:flex; gap:6px; flex-wrap:wrap; } .chip.on { background: var(--accent); color: var(--bg); }
.day { margin-top: 28px; font-size: 15px; color: var(--muted); }
.entry p { margin: 8px 0 0; }
.note { background: var(--chip); padding: 12px 16px; border-radius: 8px; } .err { color: #b91c1c; }
code { font-size: 14px; }

/* brief page */
.summary { color: var(--muted); } .summary p { margin: 0 0 8px; }
.think { background: var(--card); border:1px solid var(--line); border-left: 3px solid var(--warn); border-radius: 10px; padding: 14px 18px; margin: 18px 0; font: 15px/1.5 ui-sans-serif, system-ui, sans-serif; }
.think p { margin: 0 0 10px; } .think b { color: var(--warn); }
.think textarea { font: 14px ui-sans-serif, system-ui, sans-serif; margin-bottom: 8px; }
.think .answer { margin: 4px 0 0; padding: 10px 12px; background: var(--chip); border-radius: 8px; }
.terms { display:flex; flex-wrap:wrap; gap:6px; margin: 14px 0; }
.term { max-width: 100%; }
.term { font: 12px/1.4 ui-sans-serif, system-ui, sans-serif; background: var(--chip); border:1px solid var(--line); border-radius: 6px; padding: 3px 8px; color: var(--muted); }
.term b { color: var(--fg); font-weight: 600; }
.parts { position: sticky; top: 0; z-index: 2; display:flex; gap:6px; overflow-x:auto; padding: 10px 0; margin: 18px 0 0; background: var(--bg); border-bottom: 1px solid var(--line); scrollbar-width: none; }
.parts a { flex: none; display:flex; align-items:center; gap:7px; font: 13px ui-sans-serif, system-ui, sans-serif; color: var(--muted); text-decoration:none; border:1px solid var(--line); border-radius: 99px; padding: 4px 12px 4px 5px; }
.parts a.on { color: var(--fg); border-color: var(--accent); }
.n { display:inline-grid; place-items:center; width: 20px; height: 20px; border-radius: 99px; background: var(--chip); color: var(--muted); font: 600 11px ui-sans-serif, system-ui, sans-serif; }
.parts a.on .n { background: var(--accent); color: var(--bg); }
h2 .n { margin-right: 8px; vertical-align: 1px; }
section[id] { scroll-margin-top: 56px; }
.item { --k: var(--muted); border-left: 3px solid var(--k); }
.k-feedback { --k: var(--warn); } .k-connection { --k: var(--purple); } .k-blindspot { --k: var(--red); }
.k-resource { --k: var(--blue); } .k-next_step { --k: var(--green); } .k-question { --k: var(--accent); }
.item .kind { color: var(--k); }
.item h3 { margin: 6px 0 10px; font: 600 19px/1.3 ui-sans-serif, system-ui, sans-serif; }
.stage { display:grid; gap: 16px; }
.has-vis .stage { grid-template-columns: minmax(0, 1.15fr) minmax(0, 1fr); align-items: start; }
@media (max-width: 640px) { .has-vis .stage { grid-template-columns: 1fr; } }
.steps { list-style:none; padding:0; margin:0; counter-reset: s; font: 15px/1.45 ui-sans-serif, system-ui, sans-serif; }
.steps li { counter-increment: s; display:flex; gap: 10px; padding: 7px 8px; border-radius: 8px; cursor: pointer; transition: opacity .4s, background .3s; }
.steps li::before { content: counter(s); flex:none; display:grid; place-items:center; width: 20px; height: 20px; margin-top: 1px; border-radius: 99px; background: var(--chip); color: var(--muted); font: 600 11px ui-sans-serif, system-ui, sans-serif; transition: background .3s, color .3s; }
.anim .steps li:not(.on) { opacity: .35; }
.steps li.cur { background: var(--chip); } .steps li.cur::before { background: var(--k); color: var(--bg); }
.steps.check li { cursor: default; } .steps.check li::before { display:none; }
.steps.check label { display:flex; gap: 10px; align-items: flex-start; cursor: pointer; }
.steps.check input { margin-top: 4px; accent-color: var(--green); width: 16px; height: 16px; padding: 0; }
.steps.check input:checked + span { color: var(--muted); text-decoration: line-through; }
.ctl { margin-top: 8px; } .ctl button { font-size: 12px; padding: 3px 10px; color: var(--muted); }
.playing .ctl button { border-color: var(--k); }
.vis { background: var(--panel); border:1px solid var(--line); border-radius: 10px; padding: 16px; min-height: 120px; display:flex; font: 14px/1.35 ui-sans-serif, system-ui, sans-serif; }
.vis .part { transition: opacity .5s ease, transform .5s ease; }
.anim .vis .part:not(.on) { opacity: 0; transform: translateY(8px); }
.vis.flow { flex-direction: column; align-items: center; justify-content: center; }
.vis.flow .part { display:flex; flex-direction: column; align-items:center; width: 100%; }
.vis .arrow { color: var(--muted); line-height: 1; padding: 3px 0; font-size: 15px; }
.vis .node { border: 1.5px solid var(--line); background: var(--card); border-radius: 8px; padding: 7px 12px; text-align:center; width: min(100%, 240px); }
.vis .node.hi { border-color: var(--k); box-shadow: 0 0 0 4px color-mix(in srgb, var(--k) 18%, transparent); font-weight: 600; }
.on .node.hi { animation: pulse 1.6s ease-in-out 2; }
@keyframes pulse { 50% { box-shadow: 0 0 0 9px color-mix(in srgb, var(--k) 8%, transparent); } }
.vis.compare { gap: 10px; align-items: stretch; }
.vis .col { flex:1; min-width:0; border:1px solid var(--line); border-radius: 8px; padding: 10px 12px; background: var(--card); }
.vis .col.better { border-color: var(--green); }
.vis .ctitle { font-weight: 600; font-size: 12px; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); margin-bottom: 6px; }
.vis .better .ctitle { color: var(--green); }
.vis .col ul { margin:0; padding-left: 16px; } .vis .col li { margin: 3px 0; }
.vis.bars, .vis.timeline { flex-direction: column; justify-content: center; gap: 10px; }
.vis .bar { display:grid; grid-template-columns: minmax(60px, 32%) 1fr auto; gap: 10px; align-items:center; }
.vis .blabel { overflow:hidden; text-overflow: ellipsis; white-space: nowrap; }
.vis .track { height: 12px; background: var(--chip); border-radius: 99px; overflow:hidden; }
.vis .fill { display:block; height:100%; width: var(--w); background: var(--k); border-radius: 99px; transition: width 1s cubic-bezier(.2,.8,.2,1) .15s; }
.anim .vis .bar:not(.on) .fill { width: 0; }
.vis .bval { color: var(--muted); font-variant-numeric: tabular-nums; font-size: 13px; }
.vis.timeline { gap: 0; padding-left: 20px; }
.vis .ev { position: relative; display:grid; grid-template-columns: 92px 1fr; gap: 10px; padding: 7px 0 7px 14px; border-left: 2px solid var(--line); }
.vis .dot { position:absolute; left: -7px; top: 12px; width: 12px; height: 12px; border-radius: 99px; background: var(--panel); border: 2px solid var(--k); }
.vis .ev.on .dot { background: var(--k); }
.vis .when { color: var(--muted); font-variant-numeric: tabular-nums; }
.vis.stat { align-items: center; justify-content: center; text-align: center; }
.vis .num { font: 700 48px/1.1 ui-sans-serif, system-ui, sans-serif; color: var(--k); font-variant-numeric: tabular-nums; }
.vis .slabel { color: var(--muted); margin-top: 4px; }
@media (prefers-reduced-motion: reduce) { .vis .part, .vis .fill, .steps li { transition: none; } .on .node.hi { animation: none; } }
[hidden] { display: none !important; }
.k-time { --k: var(--blue); } .k-reprioritise { --k: var(--accent); }
button.primary, a.btn { background: var(--accent); color: var(--bg); border-color: var(--accent); font-weight: 600; }
a.btn { font: 600 14px ui-sans-serif, system-ui, sans-serif; border-radius: 8px; padding: 7px 12px; text-decoration: none; }
button.small, .small { font-size: 12px; } button.small { padding: 3px 9px; }
button:disabled { opacity: .4; cursor: default; }

/* between briefs */
.briefnav { display:grid; grid-template-columns: 1fr auto 1fr; align-items:center; gap: 10px; margin: -8px 0 14px; font: 14px ui-sans-serif, system-ui, sans-serif; }
.briefnav a, .briefnav span { text-decoration: none; color: var(--muted); padding: 6px 10px; border-radius: 8px; }
.briefnav a:hover { background: var(--chip); color: var(--fg); }
.briefnav .all { color: var(--fg); font-weight: 600; text-align: center; }
.briefnav .newer { justify-self: end; } .briefnav .older { justify-self: start; } .briefnav .off { opacity: .5; }

/* the deck */
.deck { scroll-margin-top: 12px; }
.pips { display:flex; gap: 5px; margin-bottom: 14px; }
.pip { flex: 1; height: 5px; min-width: 0; padding: 0; border: 0; border-radius: 99px; background: var(--line); transition: background .3s, transform .3s; }
.pip:hover { transform: scaleY(1.8); }
.pip.up { background: color-mix(in srgb, var(--green) 55%, var(--line)); } .pip.down { background: color-mix(in srgb, var(--red) 55%, var(--line)); }
.pip.on { background: var(--accent); }
.card { min-height: 340px; }
.card.intro h1 { margin-top: 0; }
.cards .item { margin: 0; padding: 22px 24px; box-shadow: 0 1px 0 var(--line), 0 12px 30px -18px rgba(0,0,0,.25); }
.in-next { animation: in-next .42s cubic-bezier(.2,.8,.2,1); } .in-prev { animation: in-prev .42s cubic-bezier(.2,.8,.2,1); }
.out-next { animation: out-next .22s ease-in forwards; } .out-prev { animation: out-prev .22s ease-in forwards; }
@keyframes in-next { from { opacity: 0; transform: translateX(48px) scale(.98); } }
@keyframes in-prev { from { opacity: 0; transform: translateX(-48px) scale(.98); } }
@keyframes out-next { to { opacity: 0; transform: translateX(-48px) scale(.98); } }
@keyframes out-prev { to { opacity: 0; transform: translateX(48px) scale(.98); } }
.decknav { display:flex; justify-content: space-between; align-items:center; margin-top: 16px; }
.decknav .hint { font-family: ui-sans-serif, system-ui, sans-serif; } @media (max-width: 520px) { .decknav .hint { display: none; } }
.count { font: 600 12px ui-sans-serif, system-ui, sans-serif; color: var(--muted); font-variant-numeric: tabular-nums; }
.toc { padding-left: 0; list-style: none; margin: 18px 0; font: 15px ui-sans-serif, system-ui, sans-serif; counter-reset: t; }
.toc li { counter-increment: t; } .toc a { display:flex; gap: 10px; align-items: baseline; padding: 7px 8px; border-radius: 8px; color: var(--fg); text-decoration: none; }
.toc a:hover { background: var(--chip); } .toc a::before { content: counter(t); color: var(--muted); font-size: 12px; min-width: 14px; }
.toc .kind { flex: none; color: var(--k, var(--muted)); }
.takeaway { margin: 4px 0 16px; padding: 12px 14px; border-radius: 10px; background: color-mix(in srgb, var(--k) 10%, var(--card)); font: 600 17px/1.45 ui-sans-serif, system-ui, sans-serif; }
.takeaway span { display:block; font: 600 11px ui-sans-serif, system-ui, sans-serif; text-transform: uppercase; letter-spacing: .08em; color: var(--k); margin-bottom: 3px; }
.about { font-size: 16px; margin: 0 0 14px; } .about p { margin: 0 0 8px; }
.why { margin-top: 16px; padding: 10px 14px; border-left: 3px solid var(--line); background: var(--panel); border-radius: 0 8px 8px 0; font: 14px/1.5 ui-sans-serif, system-ui, sans-serif; color: var(--muted); }
.why b { display:block; color: var(--fg); font-size: 12px; text-transform: uppercase; letter-spacing: .06em; margin-bottom: 4px; }
.why p { margin: 4px 0 0; }
.why .obj { font-size: 12px; background: var(--chip); border-radius: 99px; padding: 2px 9px; color: var(--fg); } .why .obj.none { color: var(--red); }
.fbl { color: var(--muted); font-size: 13px; margin-right: 2px; }
.end h2 { margin-top: 0; } .endnav { display:flex; gap: 16px; flex-wrap: wrap; font: 14px ui-sans-serif, system-ui, sans-serif; margin-top: 18px; }

/* objectives interview */
.chat { display:flex; flex-direction: column; gap: 10px; margin: 20px 0 10px; font: 16px/1.5 ui-sans-serif, system-ui, sans-serif; }
.msg { max-width: 85%; padding: 10px 14px; border-radius: 14px; animation: in-next .35s cubic-bezier(.2,.8,.2,1); }
.msg.assistant { align-self: flex-start; background: var(--card); border: 1px solid var(--line); border-bottom-left-radius: 4px; }
.msg.user { align-self: flex-end; background: var(--accent); color: var(--bg); border-bottom-right-radius: 4px; }
.msg.typing { color: var(--muted); animation: blink 1.2s ease-in-out infinite; } .msg.error { color: var(--red); }
@keyframes blink { 50% { opacity: .4; } }
.sugg { display:flex; gap: 6px; flex-wrap: wrap; margin-bottom: 8px; } .sugg .chip { border: 1px solid var(--line); cursor: pointer; }
.ask { display:flex; gap: 8px; align-items: flex-end; position: sticky; bottom: 0; background: var(--bg); padding: 10px 0; }
.ask textarea { font: 15px/1.5 ui-sans-serif, system-ui, sans-serif; }
.proposal { margin-top: 24px; }
.obj { background: var(--card); border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; margin: 10px 0; animation: in-next .35s ease; }
.ohead { display:flex; gap: 6px; align-items: center; margin-bottom: 6px; }
.rank { flex: none; display:grid; place-items:center; width: 22px; height: 22px; border-radius: 99px; background: var(--accent); color: var(--bg); font: 600 12px ui-sans-serif, system-ui, sans-serif; }
.otitle { flex: 1; min-width: 0; font-weight: 600; font-size: 15px; }
.ofield { display:grid; grid-template-columns: 110px 1fr; gap: 8px; align-items: center; margin: 4px 0; font: 13px ui-sans-serif, system-ui, sans-serif; color: var(--muted); }
.ofield input { width: 100%; padding: 4px 8px; font-size: 13px; }
@media (max-width: 520px) { .ofield { grid-template-columns: 1fr; gap: 2px; } }
"#;
