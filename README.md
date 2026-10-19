<p align="center"><img src="assets/icon.png" width="96" alt="brief"></p>

<h1 align="center">brief</h1>

<p align="center"><b>A shared memory that keeps you accountable.</b><br>
Every agent writes to brief. Every morning, brief writes you.</p>

---

## The problem

All of your work now happens in a box: Claude Code, Codex, Cline. You ran 50 agents today. So what
did you actually do? Was it the right thing? Did it move your goals forward?

And your agents don't talk to each other. Context needs a north star: **your objectives**.

👉 The short version, in eight slides: [**the problem**](slides/problem.pdf)

## What brief does

1. **Your agents log your work.** brief is a local MCP server. Claude Code, Codex, Cline, OpenCode,
   Claude Desktop, ChatGPT and claude.ai call it to note what you decided, built, got stuck on or learned.
2. **It knows what you're aiming for.** On setup, an agent sits down with you and writes your
   objectives together: priority, why, what "done" looks like, deadline. The conversation follows
   [*AI-Assisted Goal Setting Improves Goal Progress Through Social Accountability*](https://arxiv.org/abs/2603.17887)
   (Schimpf, Voigt & Bohné).
3. **Every morning it writes you a brief.** The agent you chose compares your objectives with where
   your time actually went (that log, your calendar, what you read, your to-dos, your meeting notes)
   and tells you, bluntly, what to do more of, less of, or stop. Then: what to do today, papers,
   repos and short explainer videos worth your time, and how to get better.
4. **It learns from you.** Rate each item and the brief reads your feedback the next day.

Everything stays on your Mac, in plain files in `~/.brief`. The brief is written by *your* agent, on
*your* account. There's no brief server or account.

## Install

**The easy way:** open your coding agent (Claude Code, Codex, Cline, OpenCode…) and say:

> Install brief from https://github.com/manoletre/brief by following its INSTALL.md

It checks what you need, installs Rust if it's missing, builds brief, and tells you to run `brief`.

**By hand:** you need macOS, the Xcode command line tools and [Rust](https://rustup.rs). Then:

```bash
git clone https://github.com/manoletre/brief.git ~/brief
```

```bash
cd ~/brief && make
```

```bash
brief
```

The last command starts a ten-minute setup in your terminal: pick your agent, set your objectives,
connect your tools, get your first brief. [INSTALL.md](INSTALL.md) has every step, with checks and fixes.

## Using it

| Command | What it does |
|---|---|
| `brief` | The app: today's brief, your objectives, journal and Setup |
| `brief open` | Your briefs in the browser (http://127.0.0.1:7878) |
| `brief generate` | Write today's brief now |
| `brief log <project> <text>` | Log something yourself |
| `brief objectives` | Edit your objectives in `$EDITOR` |
| `brief --help` | Everything else |

Optional connections, all from `brief` → Setup: Calendar, Todoist, Granola, a browser extension
(what you actually read), Anki, web search (Parallel, Brave, Browserbase) and
[ManimGX](https://manimgx.academa.ai) for explainer videos.

## How it works

[docs/how-it-works.md](docs/how-it-works.md) covers onboarding, how a brief is written, each
connection, privacy of the browser extension, and where every file lives.

## Contributing

Issues and pull requests are welcome. After changing the code, run `make`: it rebuilds, and the
`brief` command, the background service and your agents' MCP connections all pick up the new build.

## License

[MIT](LICENSE)

---

<p align="center">Made with ❤️ by <a href="https://manoletre.com">manoletre</a> 🇨🇴</p>
