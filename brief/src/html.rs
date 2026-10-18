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
<title>{title}</title><style>{CSS}</style></head>
<body><header><a class="brand" href="/">brief</a><nav>{nav}</nav></header>
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
        b.push_str("<p class=\"note\">You haven't set objectives yet. <a href=\"/objectives\">Write them</a> so the brief knows what you're working towards.</p>");
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

pub fn brief(brief: &Brief, feedback: &[Feedback]) -> String {
    let date = brief.date.map(|d| d.to_string()).unwrap_or_default();
    let mine: HashMap<&str, &Feedback> = feedback
        .iter()
        .filter(|f| f.brief_date.to_string() == date)
        .map(|f| (f.item_id.as_str(), f))
        .collect();

    let mut b = String::new();
    let _ = write!(
        b,
        "<p class=\"muted\">{} · ~{} min read</p><h1>{}</h1><div class=\"summary\">{}</div>",
        brief.date.map(|d| d.format("%A, %B %-d, %Y").to_string()).unwrap_or_default(),
        brief.reading_minutes,
        esc(&brief.headline),
        md(&brief.summary)
    );

    for section in &brief.sections {
        let _ = write!(b, "<section><h2>{}</h2>", esc(&section.title));
        for item in &section.items {
            let _ = write!(b, "<article class=\"item\" data-date=\"{date}\" data-id=\"{}\" data-title=\"{}\">", esc(&item.id), esc(&item.title));
            let _ = write!(b, "<h3>{}</h3><div class=\"meta\">", esc(&item.title));
            if !item.kind.is_empty() {
                let _ = write!(b, "<span class=\"kind k-{0}\">{0}</span>", esc(&item.kind));
            }
            if !item.project.is_empty() {
                let _ = write!(b, "<span class=\"proj\">{}</span>", esc(&item.project));
            }
            let _ = write!(b, "</div>{}", md(&item.body));
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
            b.push_str(&feedback_controls(mine.get(item.id.as_str()).copied()));
            b.push_str("</article>");
        }
        b.push_str("</section>");
    }

    let _ = write!(
        b,
        "<section><h2>How was this brief?</h2><article class=\"item\" data-date=\"{date}\" data-id=\"\" data-title=\"\">{}</article></section>",
        feedback_controls(mine.get("").copied())
    );
    if let Some(c) = brief.cost_usd {
        let _ = write!(b, "<p class=\"muted small\">Generated with Claude Code · ${c:.2} equivalent usage</p>");
    }
    layout(&brief.headline, "Briefs", &b)
}

fn feedback_controls(existing: Option<&Feedback>) -> String {
    let rating = existing.map(|f| f.rating.as_str()).unwrap_or("");
    let comment = existing.map(|f| f.comment.as_str()).unwrap_or("");
    format!(
        "<div class=\"fb\"><button class=\"r{}\" data-r=\"up\" onclick=\"rate(this)\">👍</button>\
         <button class=\"r{}\" data-r=\"down\" onclick=\"rate(this)\">👎</button>\
         <input placeholder=\"Comment for tomorrow's brief…\" value=\"{}\" onkeydown=\"if(event.key==='Enter')send(this)\">\
         <button onclick=\"send(this)\">Save</button><span class=\"ok\"></span></div>",
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

pub fn objectives(text: &str) -> String {
    let b = format!(
        "<h1>Objectives</h1><p class=\"muted\">What you're working towards. Markdown is fine. Every brief is written against this.</p>\
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
"#;

const CSS: &str = r#"
:root { --bg:#fbfaf7; --fg:#1d1d1b; --muted:#6b6a65; --line:#e6e3dc; --card:#fff; --accent:#c2410c; --chip:#f1eee7; }
@media (prefers-color-scheme: dark) { :root { --bg:#161614; --fg:#ecebe6; --muted:#9a988f; --line:#2c2b28; --card:#1e1e1b; --accent:#fb923c; --chip:#272622; } }
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font: 17px/1.6 ui-serif, Georgia, serif; }
header { display:flex; gap:24px; align-items:center; padding:14px 20px; border-bottom:1px solid var(--line); font-family: ui-sans-serif, system-ui, sans-serif; font-size:14px; }
header .brand { font-weight:700; color:var(--accent); text-decoration:none; }
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
.k-blindspot, .k-blocker { color: #b91c1c; } .k-connection, .k-idea { color: #7c3aed; } .k-resource, .k-learning { color: #0369a1; } .k-next_step, .k-progress { color: #15803d; }
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
"#;
