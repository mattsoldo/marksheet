# Marksheet Wasm binding

`marksheet-wasm` is the deliberately batched Web Worker
boundary. It accepts one versioned JSON request at a time and returns one JSON
response under the stable `marksheet-worker@1` protocol. It does not offer
per-cell getters or expose a dense workbook extent. The checked-in
[`protocol.d.ts`](protocol.d.ts) is the TypeScript contract consumed by the
worker host.

This package is intentionally an independent Cargo workspace until the parent
workspace publishes it as a release artifact. It path-depends on the Marksheet
core crates, including the sparse, pure-Rust, renderer-neutral
`marksheet-view` layer and the statically linked trusted extension host. The
view layer projects only a requested bounded region and
keeps authored values, virtual fills, calculation, source links, resolved
styles, and row/column geometry separate.

## Prerequisites and verification

Use a current stable Rust toolchain (Rust 1.85 or newer). Wasm build and ABI
checks also require the WebAssembly target and `wasm-bindgen` CLI:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
```

From the repository root, run:

```sh
cargo test --manifest-path bindings/wasm/Cargo.toml
cargo build --manifest-path bindings/wasm/Cargo.toml --target wasm32-unknown-unknown
bash bindings/wasm/check-web-abi.sh
bash bindings/wasm/check-size.sh
(cd bindings/wasm/web && npm test)
```

`protocol.d.ts` is a checked-in Rust protocol artifact. Run
`cargo run --manifest-path bindings/wasm/Cargo.toml --bin generate_protocol`
to emit it, or `bash bindings/wasm/check-web-abi.sh` to fail on protocol or
Wasm ABI declaration drift. The ABI check also loads the generated binary in
Node and exercises extension snapshots, exact-major matching, calculation
gating, opaque-payload privacy, and extension-aware edit reparsing through the
real Wasm boundary.

The Rust conformance tests exercise the browser-session fixture contract on the
native worker/session API. The small Node test suite covers worker-client
revision and cancellation behavior plus the reusable exact-byte external-save
guard. The standalone browser app, its generated-Wasm smoke test, build check,
and manual browser procedure are documented in
[`viewer/README.md`](../../viewer/README.md).

## Default admission and resource limits

The exported `WasmWorkbench` uses `SessionLimits::default()`. Before parsing
an `open` or `replace_source` request—and again while rebuilding the view after
an edit—the worker admits at most a **5 MiB (5,242,880-byte)** source and runs
these fixed source-structure preflight checks:

| Raw source observation | Maximum |
| --- | ---: |
| newline-delimited records (a final newline does not add a record) | 4,096 |
| lines whose first byte is `@` (directive diagnostic candidates) | 4,096 |
| `=` bytes (formula candidates) | 4,096 |
| `,` bytes (CSV field delimiters) | 4,096 |

These are intentionally conservative byte scans, not a partial Marksheet or
CSV parser. In particular, `=` and `,` inside quoted text or comments still
count, and a data line beginning with `@` still counts. A syntactically valid
workbook can therefore be rejected with a `limit` error. This deliberate false
positive prevents malformed quoting or comments from bypassing the bounds
before parser diagnostics and CSV lowering could allocate unbounded work.

Before `serde_json` deserializes a raw Wasm request, the binding caps its UTF-8
JSON at **32 MiB**. The browser client and worker apply the same structural and
exact-message checks before posting to Wasm, returning a correlated `limit`
error rather than leaving a pending request unresolved. An edit additionally
accepts at most **1,024 operations** and **8 MiB** of recursively metered JSON
payload; an exact-byte source expectation is separately capped at the session
source limit. These fixed binding admission limits do not add configuration
fields to `SessionLimits`.

After admission, the default worker limits a visible rectangle to 250,000
cells, a calculation rectangle to 100,000 cells, and sparse projected cells
and intersecting style regions to 100,000 each. Calculation preparation is
bounded at 100,000 output cells and 1,000,000 graph, dirty, and evaluated
cells. At most 1,024 style applications may overlap one projected visible
rectangle, and at most 1,000 resolved style layers per cell are accepted. A
sheet may declare more style applications than that: only the ones whose
target rectangle intersects the requested rectangle on both axes are
resolved, so a viewport away from them still projects. Responses retain at
most 1,000 diagnostics (reporting the omitted count) and serialize at most
32 MiB.
Coordinates and request revisions
crossing the JSON boundary must not exceed 9,007,199,254,740,991, JavaScript's
maximum safe integer. Browser source expectations carry authoritative exact
bytes only: the binding derives the core FNV metadata locally, so an arbitrary
`u64` fingerprint never crosses JavaScript as a rounded number.

## Session and safety semantics

Each request includes a protocol identifier, request id, and source revision;
mutation requests with a stale revision are rejected. A worker restart reopens
the last response-accepted source, so a cancelled in-flight edit cannot be
misreported as committed.

Formula diagnostics retain their exact source links. A document with
error-level formula diagnostics can still be projected for inspection, but the
worker marks it non-editable and refuses semantic transactions until the errors
are resolved.

The worker installs only the statically linked `assertions@1` capability.
Every open, replacement, and edit reparse passes that exact capability to the
syntax layer; another major is never treated as compatible. The trusted host
runs assertions against calculated workbook semantics and merges its structured
diagnostics into the bounded response without duplicating parser availability
diagnostics. `MS3201`, `MS3202`, and `MS3203` are validation findings rather
than source-invalidity findings, so the core workbook remains calculable,
renderable, and editable to repair them.

Snapshots expose typed declaration, instance, support, and completeness
summaries, but never extension payload bytes. An unavailable optional
capability remains a warning with complete core calculation and rendering. An
undeclared opaque instance produces `MS3103` and remains complete. An
unavailable required exact capability produces a recoverable snapshot with
both completeness flags false: range calculation is refused, and sparse
presentation contains authored core values only while explicitly reporting
incomplete rendering and calculation.

The binding's reusable local-save guard follows an exact read/compare/write
sequence: it compares the current bytes with the opening snapshot before it
calls a supplied writer. On any byte difference, including a same-length
change, it returns `external_drift` with the current, base, and proposed source
for reparse/rebase handling and does not call the writer. It also leaves an
unchanged document unwritten. The guard is storage-agnostic: an application
must supply a user-authorized File System Access writer, desktop bridge, or
other storage implementation. The Node tests prove the guard's no-write
behavior; actual browser File System Access writes require the manual browser
proof rather than a headless claim.
