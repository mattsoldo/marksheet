# Spike evidence

Observed against `formualizer` **0.8.4** on 2026-08-16.

The isolated probe uses Rust 1.88 or newer because Formualizer's current parser
dependency uses stabilized let-chains. This does not change the production
Marksheet workspace's Rust 1.85 minimum.

| Concern | Evidence | Result |
| --- | --- | --- |
| Pinned, clock-free dependency profile | `Cargo.toml` pins `=0.8.4`, sets `default-features = false`, and enables only `portable-wasm`. `cargo tree -e features` contained `portable-wasm` but no `system-clock` or `js-runtime` feature. | Pass |
| Deterministic clock | `deterministic_mode_uses_a_fixed_clock_without_system_clock_feature` constructs two engines with identical fixed UTC timestamps and gets equal `TODAY()` values. | Pass |
| Coordinates | `coordinate_limits_are_checked_before_engine_calls` accepts Excel's last cell and rejects the first larger row/column. | Pass |
| Stable sheets and names | `evaluates_cross_sheet_and_named_range_mappings` evaluates a formula through private stable sheet and name mappings. | Pass |
| Structured references | `structured_references_lower_before_engine_evaluation` lowers both a data-column and current-row reference to absolute A1; the lowered data-column reference evaluates through Formualizer. | Pass, with lowering required |
| Incremental recalculation | `dirty_dependency_updates_are_demand_driven` updates `A1`, then demand-evaluates only `B1 = A1+1`, observing the changed value. | Pass |
| Source diagnostics | `engine_diagnostics_can_be_connected_to_retained_source_locations` proves the adapter diagnostic envelope retains source bytes and a cell coordinate without an engine public type. | Adapter-owned |
| License | The `formualizer` dependency declares `MIT OR Apache-2.0`; both choices are compatible with this spike crate's MIT license and Marksheet's MIT policy. | Compatible |
| Dependency weight | `cargo tree -e normal --prefix none | sort -u | wc -l` reported **158** unique normal dependency lines for this full `portable-wasm` profile. The resolved graph includes Arrow 58.4.0 and Rayon 1.12.0. | Significant; measure binary/Wasm next |

Verification run:

```text
cargo +stable fmt --manifest-path crates/marksheet-calc-formualizer/Cargo.toml --check
cargo +stable test --manifest-path crates/marksheet-calc-formualizer/Cargo.toml --features calc-link
cargo +stable clippy --manifest-path crates/marksheet-calc-formualizer/Cargo.toml --all-targets --features calc-link -- -D warnings

8 passed; 0 failed
```

The optional `calc-link` feature compiles against the public `marksheet-calc`
boundary. The spike proves that both engines can coexist without type leakage;
it does not yet implement a complete production adapter.
