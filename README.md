# brief

brief is a shared memory that keeps you accountable. Your agents and chatbots log what you work
on through MCP. Every morning, the agent you picked (Claude Code, Codex, Cline or OpenCode)
compares your objectives with where your time actually went (that log, your calendar,
what you read in your browser, your to-do list and meeting notes) and writes you a brief to
help you reprioritise: what to do more of, less of, stop or move, plus feedback,
connections, blind spots, things to read and next steps for today.

    make                 # build, link `brief` onto PATH, restart the service if running
    brief init           # onboarding, in the terminal: agent, objectives, connections, first brief
    brief calendar       # let the brief read your calendar (Calendar app)
    brief browser        # install (or update) the extension in your browser (Dia, Arc, Chrome…)
    brief install        # run `brief serve` at login (launchd)
    brief connect        # how to hook up each chatbot
    brief open           # http://127.0.0.1:7878, your briefs (the only part on the web)

While developing, just run `make` after each change: ~/.cargo/bin/brief is a symlink
to target/release/brief, so the command, the background service and the chatbots'
MCP connections all pick up the new build.

Other commands: `generate` (write today's brief now), `extension` (install into Claude Desktop with logo and description), `objectives` (edit in $EDITOR),
`log <project> <text>`, `anki [search]` (your decks and cards), `todoist` / `granola` (connect, or show what the brief sees; `todoist off` disconnects), `mcp` (stdio transport), `key` (rotate the public link's key).

Cloud chatbots (ChatGPT, claude.ai, Grok) reach brief through Tailscale Funnel: in
`brief` → Connect chatbots, press ⏎ on "Public link". Funnel forwards to a second local
port (7879) that serves only /mcp and requires the key; the website on 7878 answers
only requests addressed to localhost.

MCP tools: `log_work` (write) and `get_context` (read) for the journal; `anki_search`
(read) and `anki_add_cards` (write) for Anki; `get_todos` (read), `add_todo` and `update_todo`
(write) for your to-do list.

## Onboarding

`brief init` (and `brief` the first time) runs entirely in the terminal (`src/onboard.rs`):

1. **What brief is**: a shared memory that keeps you accountable.
2. **Your accountability agent**: Claude Code, Codex, Cline or OpenCode, run headless on your own
   account (`src/agent.rs`). brief shows which are installed and signed in, offers to install one
   (`npm install -g …`) or run its sign-in, and then asks for a model, recommended one first
   (Claude: Opus; Cline: Claude Sonnet 5.5). "Suggest one" opens an email to hi@manoletre.com.
   The choice is kept in config.json as `agent` and `model`, and this agent also writes every brief.
3. **Your objectives**: a conversation driven by that agent (`src/interview.rs`), shaped after
   Schimpf, Voigt & Bohné, [AI-Assisted Goal Setting Improves Goal Progress Through Social
   Accountability](https://arxiv.org/abs/2603.17887): background, what energises you, priorities, then
   short- and long-term goals formulated together, with a should-versus-want check and a closing
   "can I hold you to these?". Each objective is stored in objectives.md with `Priority:`,
   `Horizon:`, `Why:`, `Done when:`, `Deadline:`, `Time:` lines. In the app, `i` on the objectives
   screen runs the same conversation again.
4. **Connect your agents**: brief's MCP server goes into Claude Code, Claude Desktop, Codex
   (`codex mcp add`), Cline (`~/.cline/data/settings/cline_mcp_settings.json`) and OpenCode
   (`~/.config/opencode/opencode.json`), so they log your work. "Do this later" skips it.
5. **Connect your tools**: Calendar, Todoist, Granola, the browser extension, Anki, and
   [Composio](https://composio.dev) for everything else: brief installs its CLI, runs `composio login`
   and `composio link <app>` for the apps you name, and the brief writer then reads them with
   `composio search` / `composio execute` (read-only tools only).
6. **Every morning**: run in the background (launchd), then a **first brief** written right away,
   which you can open in the browser, and a rating and comment that tomorrow's brief reads.

## The brief page

One card at a time: an intro (headline, summary, contents), one card per item, and a closing card
for feedback on the whole brief. Every item has a **takeaway** (the one thing to remember) and a
**Why you're seeing this** box (the evidence: calendar, browsing, journal, to-dos, and which
objective it serves). Things to read come one per card with a paragraph on what they're about.
👍/👎 on a card records it and slides to the next; ← → (or j/k, or swipe) move between cards,
[ and ] or the bar at the top move between briefs, and /today opens the newest one.

## Calendar

brief reads the Mac's Calendar app, so every account added there (Google, iCloud, Exchange) is
included without another sign-in. `brief calendar` (or Connect → Calendar) compiles a tiny Swift
helper (`src/calendar.swift`, needs the Xcode command line tools) into
~/.brief/calendar/BriefCalendar.app and macOS asks once whether "brief" may see your calendar. The
helper is only rebuilt when its code changes, since a rebuild makes macOS ask again.

The brief writer gets the events since the last brief with hours per day, today's events and its
free blocks (30+ min between 9:00 and 18:00), and the next 7 days. It counts hours per objective,
puts today's next steps into real free blocks, and flags deadlines nothing prepares for.
`brief calendar` shows exactly that; `brief calendar off` stops reading it (revoke access fully in
System Settings → Privacy & Security → Calendars).

## Browser extension

`extension/` is an extension for Chromium browsers (Dia, Arc, Chrome, Brave, Edge) that measures how long each page is actually in front of you
(active tab, focused window, not idle for 2 min) and sends the visits to `brief serve` once a minute.
For pages you stay on for 20+ seconds it also keeps the start of the text (up to 1,500 characters),
so the brief knows what you read, not only where. Query strings are dropped except searches and
YouTube video ids. Mail, banks, payments and password managers are never recorded, and the toolbar
button pauses it for an hour or blocks the current site. Incognito windows are never recorded.

Install, from the terminal:

    brief browser

That writes the extension to ~/.brief/chrome-extension, copies the path and opens your default
browser's extensions page (Dia and Chrome: chrome://extensions, Arc: arc://extensions). Browsers only
let you load a local extension by hand, once: turn on Developer mode, click **Load unpacked**, press
⌘⇧G, paste, ⏎. A small brief window floats over the extensions page with these steps, a Copy path
button and the folder to drag; it says when the extension connects and then closes itself
(`src/guide.swift`, compiled like the Calendar helper by `src/macapp.rs`).

After that, updates need nothing from you: `make`, `brief browser` and the background service
rewrite the folder when the extension's code changed, and the extension sees the newer build on its
next minute and reloads itself. `brief browser status`
says when it last sent, and `brief browser context` prints what the brief writer sees
(`brief chrome` works too).

Visits are stored in `browsing.jsonl` (60 days). The extension authenticates with a key in
`chrome.json` (written into the extension's config.js); /api/browsing is the only route that
accepts requests from a chrome-extension:// origin.

## Anki

brief connects to the Anki app on this Mac through the AnkiConnect add-on (in Anki: Tools →
Add-ons → Get Add-ons… → `2055492159`, then restart Anki). If Anki is closed when a tool needs
it, brief opens it in the background.

### Journal entry or Anki card?

|              | Journal entry (`log_work`)                          | Anki card (`anki_add_cards`)                         |
|--------------|-----------------------------------------------------|------------------------------------------------------|
| What it is   | What happened in your work                          | One fact you already understand and want to keep     |
| About        | You: decisions, progress, struggles, plans          | The subject: a question with one short answer        |
| Read by      | The brief writer, once, tomorrow morning            | You, again and again, on Anki's schedule             |
| Lifetime     | Days to weeks of context                            | Months to years                                      |
| Example      | "Finished Lesson 03; AUROC by hand is still shaky" | Q "What does AUROC equal in terms of pairs?" A "The chance a random positive scores above a random negative" |

Studying a lesson is a journal entry; the five facts from it worth keeping are five cards.
Cards are for remembering what you've already learned (in a lecture or lesson), not for learning
it: a card for something you don't understand yet just gets forgotten again and again.

### How cards get in

1. A chatbot calls `anki_search` to see what you already have and how well you remember it
   (new / learning / young / mature = remembered 3+ weeks, and how many times you forgot it).
2. It proposes cards, and on your yes calls `anki_add_cards` with a deck and the drafts.
3. A reviewer (`claude -p`, model `anki_model` in config.json, default `sonnet`, no tools) checks
   each draft against card-writing rules (one atomic fact, clear on its own, short answer, recall
   not recognition, no journal material) and the deck's existing cards, then decides: **keep**,
   **rewrite**, **split** into several cards, **duplicate** or **reject**. It also reads your taste:
   cards you buried, suspended or deleted count as bad cards, and cards you've remembered for weeks
   as good ones.
4. What passes goes into Anki as a Basic note tagged `brief` (plus `brief::rewrite` or
   `brief::split` when changed). The chatbot is told exactly what was added.

If Anki or the reviewer isn't available, the cards wait in `anki_pending.jsonl` and `brief serve`
adds them once Anki is open. While Anki is open, `brief serve` writes down buried cards every half
hour (Anki unburies them the next day, so this is the only record).

### In the daily brief

The brief writer sees cards due per deck, cards you got wrong this week, cards you keep forgetting,
the cards chatbots added, and what you buried. When cards are due, "Today" gets an Anki session
item with how many, how long and which deck first.

## To-do list (Todoist)

brief sits between your chatbots and your to-do app. Chatbots only see "the user's to-do list"
through `get_todos`, `add_todo` and `update_todo` (reword, reschedule, move, prioritise, tick off);
they never learn which app is behind it. `src/todos.rs` holds that app-agnostic layer, and each app
implements its `TodoApp` trait. Todoist (`src/todoist.rs`) is the only one so far. Chatbots are told
to add or change to-dos only when you ask, or after you agree to one they propose.

In `brief` → Connect, press ⏎ on Todoist: Todoist opens in your browser, you click Agree, done.
No developer account or token to paste: brief registers itself with Todoist on the spot (OAuth
dynamic client registration with PKCE), catches the redirect on a one-off local port, and keeps a
`data:read_write` token in `todoist.json`. `brief todoist` does the same from the terminal.
Connections made before brief could write stay read-only: the connect screen marks Todoist
"read-only", and ⏎ there reconnects with write access. Connecting reads your to-dos once, so a
broken connection shows up right away.

Each morning the brief writer gets your Todoist next to your objectives and journal: overdue tasks,
what's due in the next 7 days, every project with its open tasks, and what you completed since the
last brief. It uses them to point out objectives with no tasks behind them, tasks that keep slipping,
work you did that no task planned, and too much due at once. Next steps in "Today" come from your
real tasks when they fit.

## Granola

Same idea for meeting notes: in `brief` → Connect, press ⏎ on Granola, sign in and allow brief.
Granola has no public API for personal accounts, so brief talks to Granola's official MCP server
(mcp.granola.ai) itself, signing in the same way (dynamic registration + PKCE, token in
`granola.json`), and calls only its read-only tools, `list_meetings` and `get_meetings`.

The brief writer gets the full notes of meetings since the last brief (up to 12) and the titles of
the last 30 days. It looks for promises and follow-ups from meetings that no task or journal entry
picks up, advice that changes a plan, and people who connect to another project.

Sign-in code shared by both lives in `src/oauth.rs`.

## Files

Data lives in plain files in ~/.brief (or $BRIEF_HOME): objectives.md, entries.jsonl,
feedback.jsonl, briefs/YYYY-MM-DD.json, config.json, logs/serve.log, calendar.json (connected) and
calendar/ (the helper app), browsing.jsonl and chrome.json (the extension's visits and key),
chrome-extension/ (the unpacked extension), and for Anki:
anki_cards.jsonl (every proposal, verdict and added card), anki_feedback.jsonl (buried,
suspended and deleted cards), anki_pending.jsonl (cards waiting for Anki), and todoist.json /
granola.json (the tokens, readable only by you).
