# Coding-harness task corpus

`manifest.json` defines one shared seven-task workflow: create a workbook, add
a sheet, append a table row, change a named input, repair malformed CSV,
explain a formula error, and convert with an explicit fidelity report.

`run.py` loads both thin harness manifests (`codex` and `claude-code`) and
executes the same workflow through the canonical skill/tool package. Direct
source-authoring tasks use checked expected source files; semantic mutations
must go through `marksheet-tools@1` and are checked for exact patches. The
runner validates the authored file after every material change.

This deterministic runner proves package paths and tool behavior without
depending on a particular hosted model response. A live harness evaluation can
run the same acceptance criteria through authenticated Codex and Claude Code
clients; results are judged from the produced files and CLI semantics rather
than prompt snapshots.

Run it after building the CLI:

```sh
cargo build -p marksheet-cli
python3 tests/harness/run.py
```

For the release-only live proof (uses hosted-model credentials and is therefore
not run in CI):

```sh
python3 tests/harness/live.py --harness all \
  --record tests/harness/live-results.json
```
