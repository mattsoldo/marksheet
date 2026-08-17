# Claude Code adapter

Install or symlink the canonical `integrations/skill` directory as
`.claude/skills/marksheet` in the target project. `harness.json` references the
same guidance and structured-tool schema as every other adapter; there is no
Claude-specific dialect.

Claude Code may call the ordinary `marksheet` CLI through its shell. A local
tool bridge may expose the JSON-lines reference server from
`integrations/mcp`, constrained to the project workspace.

The adapter adds no workbook syntax, parser, network access, or installation
behavior.
