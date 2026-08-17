# Browser and GUI conformance fixtures

This corpus specifies the observable Milestone 4 browser-session contract. It
does not prescribe a JavaScript framework, Wasm toolchain, DOM shape, or the
names of public methods. A runner opens the source named by each case, executes
the requested session operation, and compares the normalized observations in
the fixture.

All records use `marksheet-view-conformance@1`. Coordinates are A1 strings,
source spans are UTF-8 half-open byte ranges, and source links use the exact
source snapshot associated with the response revision. `authored` is distinct
from an absent coordinate; `virtual` identifies a fill-derived formula that
has no CSV field of its own.

For the reference `marksheet-worker@1` profile, `visible_region` accepts only
`sheet` and `range`; it always returns the six standard layers: `authored`,
`virtual`, `calculated`, `presentation`, `geometry`, and `source_links`.
`expect_layers` records that required response profile for a fixture. It is not
a wire-request option. A different general-purpose renderer binding MAY offer
layer selection, but it cannot use that option when claiming this reference
worker profile.

`unsupported_assertions` is required whenever a fixture expectation needs a
host capability not observable through the native worker protocol. The only
current values are `max_rendered_grid_cells`, `max_coordinate_probes`, and
`writes`. Native runners must validate all remaining assertions and explicitly
report these exclusions; browser-host runners are expected to implement them.

## Cases

- `budget_open` proves the user-visible workbook path: ordered tabs, calculated
  values, resolved presentation, a focused edit/save result, and source links
  for `examples/budget.ms`.
- `layers_geometry` proves authored, formula, calculated, virtual, resolved
  style, and effective geometry layers remain distinct.
- `distant_sparse` proves separate near and far viewport requests do not
  materialize the rectangle between the two blocks.
- `worker_revision` proves request identity, cancellation, and stale-result
  suppression.
- `diagnostic_source` proves diagnostics carry source links suitable for a
  source view.
- `external_change` proves a local-file save detects and refuses an unsafe
  external modification.

`validate.sh` validates the corpus's file references, required fields,
coordinate syntax, and self-consistency. It does not exercise a browser. A
browser or Wasm conformance runner must additionally execute the operations and
assert the fixture's expected observations.

## Sparse proof metrics

The `budget` field describes observations that the runner must collect. They
are deliberately independent of process memory measurement:

- `max_returned_cells` limits sparse cell records returned by the request;
- `max_rendered_grid_cells` limits realized grid-cell elements after the
  implementation's finite overscan; and
- `max_coordinate_probes` limits calls to a per-coordinate backing store while
  producing the viewport.

The final metric prevents a superficially sparse response that first iterates
every coordinate between `A1` and the distant block. The native runner marks
the latter two metrics in `unsupported_assertions`; browser hosts must enforce
them.
