# `make` rebuilds the `brief` command (~/.cargo/bin/brief links to target/release/brief)
# and restarts the background service if it's installed, so it runs the new code. Running
# `brief mcp` processes (Claude Desktop, Claude Code) switch to the new build on their next
# message, ~/.brief/brief.mcpb is rebuilt for the next Claude Desktop install, and the browser
# extension's folder is rewritten (it reloads itself within a minute).

LABEL := com.brief.serve

install:
	cargo build --release
	@ln -sf $(CURDIR)/target/release/brief $(HOME)/.cargo/bin/brief
	@$(CURDIR)/target/release/brief extension --no-open >/dev/null
	@$(CURDIR)/target/release/brief browser update | grep -v "up to date\|isn't installed" || true
	@if launchctl print gui/$$(id -u)/$(LABEL) >/dev/null 2>&1; then \
		launchctl kickstart -k gui/$$(id -u)/$(LABEL) && echo "restarted background service"; \
	fi

.PHONY: install
