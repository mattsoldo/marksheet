# Marksheet conversion conformance fixtures

This corpus defines the Milestone 5 contract for the public
`marksheet-conversion@1` report. Each case supplies a request and the minimum
ordered feature outcomes and diagnostics that a converter must report. It does
not prescribe a converter implementation or require a proprietary workbook.
[`report.schema.json`](report.schema.json) is the strict public response shape;
[`schema.json`](schema.json) validates the checked-in fixture requests and
expectations.

`sources/*.xlsx.json` describes deterministic generated OOXML inputs. A
converter implementation may generate the corresponding binary test archives,
but this repository deliberately does not include copied Office files.

The cases cover:

- exact core Marksheet-to-XLSX conversion;
- lossy extension/chart export and macro/chart XLSX import;
- explicit XLSX resource-limit refusal;
- selected-table and selected-sheet-range CSV output; and
- rejection of a CSV request without an explicit selection; and
- CSV import with explicit table and exact-range Marksheet targets, including
  rejection of an implicit target.

Run `./tests/conversion/validate.sh` to validate fixture shape and report
fidelity invariants. `lossless` requires only exact outcomes and no diagnostics;
`lossy` requires an omission or approximation; `unsupported` requires an error
and cannot name an output artifact.
