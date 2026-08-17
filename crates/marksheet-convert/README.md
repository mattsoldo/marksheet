# marksheet-convert

`marksheet-convert` provides deterministic, bounded conversions between the
semantic `marksheet-model::Workbook` IR, RFC 4180 CSV, and the supported core
of OOXML `.xlsx`. The library accepts and returns byte buffers; applications
remain responsible for file selection and atomic writes.

## Public API

- `export_xlsx(&Workbook, ConversionLimits) -> ConversionResult<Vec<u8>>`
- `import_xlsx(&[u8], ConversionLimits) -> ConversionResult<Workbook>`
- `export_csv(&Workbook, &CsvExportSelection, ConversionLimits) -> ConversionResult<Vec<u8>>`
- `import_csv(&[u8], &CsvImportSelection, ConversionLimits) -> ConversionResult<Workbook>`

CSV requests require an explicit range or table for export and an explicit
target sheet ID, label, and anchor for import. The API cannot silently infer a
used range or destination sheet.

Every successful result includes a `marksheet-conversion@1` report. Fidelity is
derived from its ordered feature outcomes: any approximation or omission is
`lossy`, while a rejected conversion is represented by an `unsupported` report
carried by the returned `ConversionFailure`; callers cannot accidentally lose
the unsupported report. Formula outcomes distinguish preservation,
translation, and replacement.

## XLSX profile

The supported projection includes workbook sheet order and labels, sparse
scalar and portable formula cells, native tables, table calculated columns,
single-cell/range/table-column names, resolved core styles, row heights, and
column widths. Export has fixed part and relationship ordering, fixed ZIP
metadata and compression settings, stable XML spelling, and no volatile
document properties. Repeated exports of the same IR and limits are byte-equal.
Preserved formulas must both parse as `portable-a1@1` and use evaluator-supported
function signatures; known functions with the wrong argument count are never
reported as exact.

Coordinate fills are expanded to per-cell formulas and reported as an
approximation. Table-column fills use OOXML `calculatedColumnFormula` and
round-trip as table-column fills. Style/apply declarations are projected to
resolved cell formats. Dates and datetimes use OOXML ISO date cells, preserving
the authored datetime offset without inventing a date presentation style.

Macros, external links, charts, pivots, drawings, merged cells, conditional
formatting, rich-text formatting, borders, protection, theme/tint colors, and
other advanced OOXML features are outside the initial semantic profile. Safe
package parts in these categories are explicitly omitted in the report;
external relationships and ambiguous or unsafe packages are rejected.

## Untrusted-input gates

`ConversionLimits` bounds input/output bytes, ZIP entries, per-entry and total
uncompressed bytes, compression ratio, XML events/depth/attributes/text,
relationships, sheets, tables, styles, cells, formulas, shared-string entries,
and decoded strings. XLSX import never
extracts files. It rejects unsafe or aliased ZIP names, directories, symlinks,
encryption, unsupported compression, size inconsistencies, zip-slip targets,
external or dangling relationships, duplicate relationship IDs, relationship
content-type mismatches, DTDs, malformed XML, and limit violations. Rejections
do not return partial workbooks.

## Dependencies and licensing

The crate itself is MIT licensed. OOXML is implemented with a narrow reader and
writer rather than using a spreadsheet library as semantic authority:

- `quick-xml 0.41.0` — MIT.
- `zip 7.2.0` — MIT, pinned exactly with default features disabled and only
  `deflate-flate2-zlib-rs` enabled.
- The selected deflate path uses `flate2` (MIT OR Apache-2.0) and `zlib-rs`
  (Zlib), both pure Rust.

The remaining dependencies are workspace libraries (`marksheet-model`,
`marksheet-calc`, Serde, and `time`).
