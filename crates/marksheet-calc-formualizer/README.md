# Formualizer adapter spike

This is an intentionally isolated, non-workspace feasibility probe for
`marksheet-calc`. It pins [`formualizer` 0.8.4](https://crates.io/crates/formualizer/0.8.4)
with `default-features = false` and the documented `portable-wasm` feature
profile. It must not become a production dependency without an integration
review.

## What is proven

- Stable Marksheet sheet IDs and name IDs lower to engine-private formula-safe
  symbols, so user-facing sheet-label edits do not rewrite engine references.
- Marksheet coordinates are rejected above the engine's Excel grid limits
  (16,384 columns by 1,048,576 rows) before an engine call.
- A deterministic fixed UTC clock evaluates `TODAY()` reproducibly with the
  ambient `system-clock` feature excluded.
- Formualizer's demand evaluation recalculates a dependent after a changed
  input. The test executes a two-cell dependency and asks only for the dirty
  dependent.
- Engine-native cross-sheet cells and defined names evaluate through the
  private mappings.
- Resolved structured data-column and current-row references can lower safely
  to finite absolute A1 references before Formualizer parses them. The test
  evaluates a lowered `SUM` over a data column.
- Source diagnostics can remain Marksheet-owned by retaining a source anchor
  next to the engine-cell mapping. No Formualizer AST/error type is exported
  by this crate.

Run the probe without changing the root workspace or lockfile:

```sh
cargo test --manifest-path crates/marksheet-calc-formualizer/Cargo.toml --features calc-link
cargo clippy --manifest-path crates/marksheet-calc-formualizer/Cargo.toml \
  --all-targets --features calc-link -- -D warnings
cargo tree --manifest-path crates/marksheet-calc-formualizer/Cargo.toml -e features
```

Once `marksheet-calc` exposes a buildable integration boundary, also verify the
path dependency explicitly:

```sh
cargo check --manifest-path crates/marksheet-calc-formualizer/Cargo.toml --features calc-link
```

## Findings and adoption constraints

`formualizer` 0.8.4 is dual licensed `MIT OR Apache-2.0`, compatible with the
repository's MIT licensing policy. Its `portable-wasm` preset includes eval,
workbook, parser, SheetPort, and common support while omitting its
`system-clock` and `js-runtime` features. It is substantial: evaluation brings
Arrow and Rayon; this resolved profile has 158 unique normal dependency lines.
Native/wasm size and startup still need a measured product build before
adoption. See [EVIDENCE.md](EVIDENCE.md) for reproducible command results.

Do not pass structured-reference text directly to Formualizer. Its current
resolver explicitly reports `ThisRow` (`[@column]`) and complex structured
references as `NImpl`; lower Marksheet's already-resolved table semantics to
A1 as demonstrated here. The adapter must also reject portable-profile
volatile functions during Marksheet AST validation: omitting `system-clock`
removes ambient time, but it does not remove volatile functions such as
`RAND()` from Formualizer.

The engine exposes inspection data keyed by engine cells, but has no Marksheet
source spans. The future `marksheet-calc` adapter must retain the source/cell
map used by `AdapterDiagnostic` and translate engine errors into stable
Marksheet diagnostic codes.

The `calc-link` feature verifies that the spike can coexist with the public
`marksheet-calc` API without leaking Formualizer types into that API. This is a
positive feasibility result for translation and incremental evaluation, not a
recommendation to adopt Formualizer as the portable-profile semantic authority.
A complete differential run against the formula corpus remains an explicit
prerequisite for any production adoption.
