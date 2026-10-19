# `make` rebuilds the `brief` command (~/.cargo/bin/brief links to target/release/brief)
# and restarts the background service if it's installed, so it runs the new code. Running
# `brief mcp` processes (Claude Desktop, Claude Code) switch to the new build on their next
# message, ~/.brief/brief.mcpb is rebuilt for the next Claude Desktop install, and the browser
# extension's folder is rewritten (it reloads itself within a minute).

LABELS := com.brief.serve com.brief.serve.brieff

install:
	cargo build --release
	@mkdir -p $(HOME)/.cargo/bin
	@ln -sf $(CURDIR)/target/release/brief $(HOME)/.cargo/bin/brief
	@$(CURDIR)/target/release/brief extension --no-open >/dev/null
	@$(CURDIR)/target/release/brief browser update | grep -v "up to date\|isn't installed" || true
	@for l in $(LABELS); do \
		if launchctl print gui/$$(id -u)/$$l >/dev/null 2>&1; then \
			launchctl kickstart -k gui/$$(id -u)/$$l && echo "restarted $$l"; \
		fi; \
	done

.PHONY: install
