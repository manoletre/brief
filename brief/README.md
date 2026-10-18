# brief

Your chatbots log what you work on through MCP. Every morning, your local Claude Code
reads your objectives, that log, and your feedback on past briefs, then writes you a
10–20 minute brief with feedback, connections, blind spots, things to read or watch,
and next steps.

    cargo build --release
    ./target/release/brief init        # objectives, brief hour, model
    ./target/release/brief install     # run `brief serve` at login (launchd)
    ./target/release/brief connect     # how to hook up each chatbot
    ./target/release/brief open        # http://127.0.0.1:7878

Other commands: `generate` (write today's brief now), `objectives` (edit in $EDITOR),
`log <project> <text>`, `mcp` (stdio transport), `key` (access key for tunnels).

MCP tools: `log_work` (write) and `get_context` (read).

Data lives in plain files in ~/.brief (or $BRIEF_HOME): objectives.md, entries.jsonl,
feedback.jsonl, briefs/YYYY-MM-DD.json, config.json, logs/serve.log.
