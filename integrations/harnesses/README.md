# Coding-agent harness adapters

These packages adapt installation paths only. Both reference the canonical
skill in `integrations/skill` and the `marksheet-tools@1` schema/server in
`integrations/mcp`; neither contains a fork of authoring guidance.

The executable task corpus in `tests/harness` loads both manifests and runs the
same authoring, editing, calculation, diagnosis, and conversion workflow
through each configured environment profile.

`tests/harness/live.py` additionally invokes the authenticated Codex and Claude
Code clients in separate disposable workspaces for the release acceptance
proof; this hosted-model check is intentionally not part of hermetic CI.

Requests and stable response envelopes are described by
`../mcp/tool-schema.json` and `../mcp/response-schema.json` respectively.
