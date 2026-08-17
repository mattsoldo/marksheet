# Codex adapter

Install or symlink the canonical `integrations/skill` directory as
`.codex/skills/marksheet` in the target project. Do not copy and modify the
guidance: `harness.json` deliberately points to the one canonical skill.

Codex can use the ordinary `marksheet` CLI from its shell. Hosts that bridge
local structured tools may start `integrations/mcp/marksheet_tool_server.py`
with the project root as `--workspace` and expose the operations from
`tool-schema.json`.

The adapter adds no workbook syntax, parser, network access, or installation
behavior.
