# Independent Python conformance projection

`marksheet_projection.py` is a Python 3.12, standard-library-only structural
consumer for Marksheet source bytes. It intentionally does not invoke a Rust
binary, WebAssembly module, FFI boundary, generated protocol declaration, or
any production parser code. It exists so a future Rust reference projection can
be compared against an independently implemented consumer.

It validates UTF-8/BOM and LF/CRLF physical lines, directive structure,
CSV quoting and multiline records, scalar classification, source byte spans,
workbook declarations, sparse blocks/tables, styles, names, fills, applies,
geometry, opaque extensions, and extension-registry diagnostics `MS3101`–
`MS3103`. Its output is deterministic JSON with schema
`marksheet.conformance-projection@1`.

CSV scalar values normalize CRLF record separators to LF, matching Marksheet's
semantic CSV model. The independent projection keeps raw field spelling and
byte spans separately, so this normalization does not compromise lossless
source inspection.

The checked corpus is enumerated, not hand-picked, in
`tests/conformance/projections/manifest.json`. It contains every `.ms` file in
`tests/conformance/{valid,invalid}`, `tests/roundtrip`, `tests/extensions`,
and `tests/conversion/sources`; source paths and projection file names form a
checked bijection. Projections use an explicitly empty extension registry, so
unavailable-capability diagnostics are deterministic rather than dependent on
the host's installed extensions.

The consumer refuses oversized input, physical-line, directive/token, CSV
row/field/cell, and diagnostic workloads. Refusals emit visible stable
diagnostics (`MS1101`, `MS1102`, or `MS1202`) and never silently truncate a
trusted projection. These are host resource bounds, not Marksheet format
limits.

Formula calculation is deliberately out of scope: formula source is retained
exactly, but this consumer makes no claim of formula-grammar or evaluation
conformance.

Run the full lane from the repository root:

```bash
bash conformance/python/validate.sh
```

Regenerate the independently reviewed checked-in projections after changing
this parser or their source fixtures:

```bash
python3 conformance/python/generate_projections.py
python3 conformance/python/generate_projections.py --check
```

The second command is the deterministic rerun check used by CI. Checked
projections cover the full declared corpus: valid and invalid conformance
sources, round-trip pairs, extension fixtures, and conversion sources. Invalid
projections intentionally compare recovered structure and exact normalized
diagnostic multiplicity, so recovery drift is visible across implementations.
