# Local structured-tool server

`marksheet_tool_server.py` is the reference local adapter for
`marksheet-tools@1`. It reads one JSON request per line on standard input and
writes one correlated JSON response per line on standard output. It delegates
all workbook semantics and mutations to the installed `marksheet` CLI; it is
not a second parser.

Start it with an explicit workspace boundary:

```sh
python3 integrations/mcp/marksheet_tool_server.py \
  --workspace /absolute/path/to/project \
  --marksheet /absolute/path/to/marksheet
```

Example request:

```json
{"id":"query-1","tool":"get","arguments":{"path":"budget.ms","target":"tax_rate","calculated":true}}
```

The stable operations are `check`, `inspect`, `get`, `set`,
`append_table_row`, `calculate`, `format`, `convert`, and `semantic_diff`.
Their argument shapes are listed in `tool-schema.json`; stable CLI and server
response envelopes are listed in `response-schema.json`. Exit code 0 becomes
`status:"ok"`, semantic refusal/difference code 1 becomes
`status:"rejected"`, and operational code 2 becomes `status:"error"`. A
successfully applied edit that leaves extension assertion errors is the one
exception: it retains CLI exit code 1 but returns `ok:true` and
`status:"committed_invalid"`; clients must inspect `changed` and must not retry
a non-idempotent operation.

Every path is resolved against the configured workspace after symlink
resolution. Paths outside that directory are refused. The adapter has no
network, package installation, clock, or workbook-selected executable surface.
The reference adapter is intended for one cooperative local project: it guards
content replacement races, but another hostile process that can rename
ancestor directories during a request is outside this pathname-based boundary.
Run it inside an OS sandbox or replace its path bridge with descriptor-relative
I/O when serving a mutually untrusted or multi-tenant filesystem.
Requests are limited to 8 MiB, input files and responses to 32 MiB, subprocess
argument text to 1 MiB, and one calculation request to 32 targets/100,000
returned cells. Subprocess stdout and stderr are streamed into one hard 32 MiB
in-memory cap with a 30-second deadline before JSON decoding. `set` and
`append_table_row` return the exact source patches produced by the Rust edit
engine. A mutating `format` returns its one exact whole-source replacement;
conversion returns the converter's fidelity report.

Run the integration tests after building the CLI:

```sh
cargo build -p marksheet-cli
python3 integrations/mcp/test_tool_server.py
```

The JSON-lines transport is intentionally protocol-neutral. Harness adapters
may bridge it to MCP or another local tool protocol without changing these
operation names or duplicating Marksheet guidance.
