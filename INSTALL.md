# Installing brief

These steps are written so that a person **or a coding agent** (Claude Code, Codex, Cline,
OpenCode…) can follow them exactly. If you're an agent: run each step, check its result before
moving on, and stop and tell the user if a check fails. Don't run `brief` itself at the end; it's an
interactive terminal app, so hand that step to the user.

brief runs on **macOS** (it uses launchd, the Calendar app and Swift helpers).

## 1. Check the requirements

| Need | Check | If it's missing |
|---|---|---|
| macOS | `uname -s` prints `Darwin` | brief doesn't run elsewhere yet |
| git | `git --version` | `xcode-select --install` |
| Xcode command line tools (swiftc, for the Calendar and browser helpers) | `xcode-select -p` prints a path | `xcode-select --install` (opens a dialog the user has to click through) |
| Rust (stable), via rustup | `cargo --version` | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh -s -- -y`, then `source "$HOME/.cargo/env"` |
| Node.js / npm (only to install an agent CLI) | `npm --version` | `brew install node`, or https://nodejs.org |
| One agent CLI, signed in: Claude Code, Codex, Cline or OpenCode | `claude --version` (or `codex`, `cline`, `opencode`) | brief's onboarding offers to install and sign in to one, so this can wait |

`~/.cargo/bin` must be on your `PATH` (rustup adds it; check with `echo $PATH | tr ':' '\n' | grep cargo`).
If Rust came from Homebrew instead of rustup, add `export PATH="$HOME/.cargo/bin:$PATH"` to `~/.zshrc`.

## 2. Download and build

```bash
git clone https://github.com/manoletre/brief.git ~/brief
cd ~/brief
make
```

`make` builds a release binary (the first build takes a minute or two) and links it to
`~/.cargo/bin/brief`. Leave the folder where it is: the link points into it.

**Check:** `brief --version` prints `brief 0.1.0` (open a new terminal if the command isn't found).

## 3. Set it up

In a terminal, run:

```bash
brief
```

The first run is onboarding, all in the terminal, about ten minutes:

1. **Your agent**: pick Claude Code, Codex, Cline or OpenCode. brief installs or signs in to it if
   needed. That agent writes your briefs, on your own account.
2. **Your objectives**: a short conversation with that agent about what you're working towards.
3. **Connect your agents**: brief adds its MCP server to the agents and chatbots you use, so they
   log your work as you go.
4. **Connect your tools** (all optional): Calendar, Todoist, Granola, the browser extension, Anki,
   and anything else through Composio.
5. **Research and videos** (optional): which web search the brief uses, and ManimGX for videos.
6. **Every morning**: brief starts in the background at login, and writes your first brief right away.

Everything can be changed later in `brief` → Setup.

**Check:** `brief open` opens your briefs at http://127.0.0.1:7878.

## Updating

```bash
cd ~/brief && git pull && make
```

`make` also restarts the background service and refreshes the browser extension, so nothing else is needed.

## Uninstalling

```bash
brief uninstall
```

```bash
rm ~/.cargo/bin/brief
```

Your data stays in `~/.brief` (objectives, journal, briefs) until you delete that folder. Remove brief's
MCP entry from your agents' settings if you added it.
