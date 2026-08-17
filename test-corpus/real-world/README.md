# Real-world XLSX files

137 files that were **not** written by this project: 86 copied from the test
suites of seven permissively-licensed spreadsheet libraries (pinned by commit
SHA), 48 exported from public Google Sheets, and 3 statistical workbooks
published by public bodies. Unlike everything under
`../xlsx/`, none of these are synthetic. They carry the accumulated real-world
weirdness — genuine Microsoft Excel, Google Sheets, LibreOffice, Go, PHP, .NET,
C++ and Python output, real bug-report attachments, fuzzer-discovered crash
inputs — that a generated corpus cannot reproduce.

The point of the spread is **producer diversity**. Each writer has its own
habits, and nearly every defect below was found because some particular writer
does something the others do not.

| Producer | Files | Source | License |
| --- | --- | --- | --- |
| Apache POI (real Excel + bug reports) | 47 | `apache/poi` `test-data/spreadsheet` | Apache-2.0 |
| xlnt (C++) | 10 | `tfussell/xlnt` `tests/data` | MIT |
| calamine (Rust; Excel + LibreOffice files) | 9 | `tafia/calamine` `tests` | MIT |
| **Google Sheets** (live export) | 48 | public shared templates | see below |
| pandas (openpyxl / xlsxwriter) | 6 | `pandas-dev/pandas` | BSD-3-Clause |
| excelize (Go) | 5 | `qax-os/excelize` `test` | BSD-3-Clause |
| PhpSpreadsheet (PHP) | 5 | `PHPOffice/PhpSpreadsheet` | MIT |
| ClosedXML (.NET) | 4 | `ClosedXML/ClosedXML` | MIT |
| **Public bodies** (real published Excel) | 3 | ONS, World Bank, US Census | OGL v3 / CC BY 4.0 / public domain |

Per-file provenance is in [`manifest.json`](manifest.json). Regenerate
everything with `./download.sh` then `python3 build_manifest.py`. License texts
for every source are carried in [`LICENSES/`](LICENSES/).

## The Google Sheets group

Google Sheets is one of the most widely used spreadsheet producers in the
world and its XLSX export is unlike any of the library writers, so it gets its
own group. `download.sh` fetches each one through the public
`/export?format=xlsx` endpoint.

Two caveats, both deliberate:

- **They are not byte-stable.** Google re-serializes on every export, so the
  bytes differ run to run even when the document has not been edited. The
  manifest records `sha256_first_seen` so a genuine content change can still be
  spotted; `download.sh` prints the new digest rather than failing.
- **They are someone else's documents.** Every candidate is screened before
  inclusion: each workbook is opened and every cell scanned for email addresses
  and phone numbers, and anything carrying real-looking contact details is
  dropped. Placeholder tokens (`Your Company`, `(123) 456-7890`, `555` numbers)
  are fine; person-shaped addresses are not. Of 45 candidates in the second
  round, 4 were rejected on that basis, along with an otherwise-useful public
  COVID dataset carrying an institutional email. That care is not hypothetical:
  the SheetJS corpus, the obvious first choice for a project like this, is
  access-blocked by GitHub for exactly this reason.

  The templates span budgets, Gantt charts, inventory, project management,
  invoices, timesheets, schedules and dashboards, from 6 KB to 1.9 MB, sourced
  from Smartsheet, Coefficient, SpreadsheetPoint, Tiller, Unito and others.

## The published-workbook group

Three statistical workbooks published by public bodies, fetched by URL:

| File | Publisher | Licence |
| --- | --- | --- |
| `ons_consumer_price_inflation.xlsx` | UK Office for National Statistics | Open Government Licence v3.0 |
| `worldbank_gdp.xlsx` | World Bank DataBank | CC BY 4.0 |
| `us_census_state_population.xlsx` | U.S. Census Bureau | Public domain (17 U.S.C. 105) |

These are the only files in the corpus that are *real working documents* rather
than fixtures or templates — genuine Excel output at production scale. The ONS
workbook is 81 sheets and 2.1 MB, and by itself surfaced three separate defects
(the carriage return in cell text, the 21-decimal number format, and the
performance figure below). Like the Google Sheets, they are fetched live and
their publishers refresh them, so `download.sh` records a first-seen digest
instead of pinning bytes.

## Why these sources, not others

Researched candidates and verdicts (full writeup in
[`../README.md`](../README.md#external-real-world-corpora-researched-not-included)):

| Source | License | Verdict |
| --- | --- | --- |
| **Apache POI, calamine, xlnt, pandas, excelize, PhpSpreadsheet, ClosedXML** | Apache-2.0 / MIT / BSD-3 | **Used** — see the producer table above. All permissive, all actively maintained, all commit-pinnable. |
| **Public Google Sheets** | template content, vetted | **Used** — the single most productive source, see above. |
| SheetJS `test_files` (the "canonical" one) | was Apache-2.0 | **Not used.** GitHub has it access-blocked for embedded private/personal information (confirmed via API) -- do not reconstruct or mirror it. |
| SheetJS `enron_xls` | CC0-1.0 | Not used here. Mostly legacy BIFF2/TSV/SYLK, not modern OOXML; still contains real employee names/business data. |
| EUSES / FUSE academic corpora | unclear / dead host | Not used. No stated license on the current EUSES mirror; FUSE's distribution point 404s. |
| Sheetpedia (2025) | CC BY-SA 4.0 | Not used. Share-alike terms are real friction against this MIT-licensed repo. |

Every source's license text is carried in [`LICENSES/`](LICENSES/), including
POI's NOTICE file as Apache-2.0 requires.

## License note on `xlsx/`

`xlsx/` is gitignored, same as the synthetic corpus's `xlsx/` -- these are
copied third-party files, appropriate to attribute and reference by pinned
commit, but not to carry as binary blobs in this repo's history. Run
`./download.sh` to fetch them locally.

## Verification results

Run `../verify.sh`. **130 of 137 import cleanly.** Six of the seven that do
not are *supposed* to fail; the seventh is a recorded gap:

| File | Result |
| --- | --- |
| `calamine/pass_protected.xlsx` | correctly refused: encrypted, an OLE/CFB container rather than a ZIP |
| `poi/clusterfuzz-testcase-minimized-...xlsx` | correctly refused: truncated fuzzer input, no ZIP end-of-directory record |
| `poi/poc-xmlbomb.xlsx` | correctly refused in 3 ms by the compression-ratio budget |
| `poi/xlsx-corrupted.xlsx` | correctly refused: no root `officeDocument` relationship |
| `excelize/BadWorkbook.xlsx` | correctly refused: declares `<sheets></sheets>` |
| `poi/duplicate-filename-case-insensitive.xlsx` | correctly refused: two entries whose names differ only by case |
| `xlnt/2_minimal.xlsx` | **known gap** — see below |

**Performance.** The 81-sheet ONS workbook converts in **17.9 s** in a release
build, producing 975k lines of Marksheet. In a debug build it exceeds four
minutes, which is why `verify.sh` takes a generous per-file timeout; pass it a
release binary (`./verify.sh ./target/release/marksheet`) when running the whole
corpus.

## Bugs this corpus found

Fourteen defects so far, every one from a file some real tool actually wrote.
Regression tests accompany each fix.

### Found by the first 22 files

1. **`mc:Ignorable` extension namespaces were rejected outright** — the
   highest-impact one. Every worksheet Excel has written since about 2010
   carries `x14ac:dyDescent` on its rows under an `mc:Ignorable="x14ac"`
   declaration that ECMA-376 Part 3 defines as safe to ignore. Fixed by
   `prepare_consumed_part`, which applies both ECMA-376 extensibility
   mechanisms — `mc:Ignorable`/`mc:AlternateContent` and `extLst` — before any
   semantic parsing. Stripping rather than tolerating matters: real files put
   `x15:workbookPr` inside `extLst`, which a local-name parser would otherwise
   read as the workbook's own `workbookPr`.
2. **`r:id` was allow-listed on too few elements** (`pageSetup`,
   `externalReference`, `pivotCache`, `pivotSelection`).
3. **Ordinary ZIP directory entries were rejected** — `xl/`, `_rels/` are valid
   content-free records that Java's writer, and so Apache POI, emits.
4. **A defined name resembling a cell address was mangled or fatal** — three
   disagreeing copies of the "looks like an address" test, now one
   `resembles_cell_address` bounded to the addressable grid.
5. **A non-UTF-8 `customXml` part failed the whole import.**
6. **A `#REF!` defined name was fatal** rather than dropped.

### Found by the 37 added later

7. **Google Sheets currency formats produced an unserializable style.** Google
   writes `[$£-809]#,##0`; the importer only matched the symbol-less `[$-409]`
   date form, so it set `number=currency` with no code — which SPEC requires and
   its own serializer then rejected. Now parses the real `[$<symbol>-<LCID>]`
   shape, maps the locale (the reliable half — `$` alone is used by a dozen
   currencies) to an ISO 4217 code, and falls back to a plain decimal when no
   code can be determined. Hit 3 of the 7 Google Sheets.
8. **A carriage return in cell text could be imported but never written.**
   Marksheet decodes a CRLF inside a quoted field to one LF (SPEC section 6.3),
   so the serializer's refusal was correct — the bug was the importer admitting
   CR into the model at all. In-cell line breaks now normalize to LF on import.
   Confirmed independently on a Google Sheets export *and* on a real 81-sheet
   UK government workbook.
9. **OPC part names were compared case-sensitively.** excelize stores
   `xl/SharedStrings.xml` and then relates to `sharedStrings.xml`; OPC compares
   part names case-insensitively. Fixed in both part lookup and content-type
   resolution.
10. **A dangling relationship to an unconsumed part was fatal.** xlnt's file
    references a `calcChain.xml` that is not in the package. A calculation cache
    is regenerable and a GUI drops it too, so a missing part is now only fatal
    when it is one this converter would actually open.
11. **A table whose header row disagreed with its table part killed the
    workbook.** Now the table is imported as plain cells with the loss
    reported — the data survives, only the structure is lost.

### Found by the 37 added after that

12. **A defined name with no representable target killed the workbook.**
    Workbooks accumulate names Marksheet has no target for — array constants,
    formulas such as `OFFSET(Sheet1!$A$1,3,1)`, whole-table selectors like
    `Table[#All]`, sort bookkeeping like `_Order1`, and names left behind by
    deleted columns. Each was fatal. All are now dropped with the reason
    reported. Found on the **World Bank** GDP table and three POI files.
13. **A chart sheet killed the workbook.** A workbook may hold chart and dialog
    sheets beside its worksheets. They carry no cells, so the sheet is now
    omitted (and any name targeting it dropped) instead of refused.
14. **Google Sheets writes a relationship with no `Id` and no `Target`.** Its
    pivot-cache part carries `<Relationship Type="..." TargetMode="External"/>`
    and nothing else, which OPC does not permit. Because the owning part is one
    this converter never opens, the entry is now skipped rather than failing
    the package; a malformed relationship on a part we do read is still an
    error.
15. **A number format with more than 15 decimal places was unserializable.**
    Draft 0.1 `@style` accepts `decimals` 0..=15; the importer counted the
    zeros in the format string with no bound. The ONS inflation tables carry a
    21-decimal format, so the count is now clamped. This was the last thing
    standing between the corpus and a 2.1 MB, 81-sheet real government
    workbook importing end to end.

### Recorded, not fixed

12. **`xlnt/2_minimal.xlsx` — the workbook part at the package root.** OOXML
    locates parts through the relationship graph; it does not mandate
    `xl/workbook.xml`. The importer hardcodes that path in several places, so a
    conforming package laid out differently is refused. Fixing it properly means
    making part discovery fully relationship-driven, which is a real refactor
    rather than a patch, so the fixture is kept and the gap recorded. Every
    mainstream writer uses the conventional layout, so the practical impact is
    low.
13. **Style identifier ordering is not stable across an XLSX round-trip** — see
    below. No data is lost and it converges after one pass.

## Round-trip stability

`./test-corpus/roundtrip.sh` runs `xlsx -> A.ms -> xlsx -> B.ms` and asserts
`A.ms == B.ms`.

Comparing the re-exported `.xlsx` against the *original* would be the wrong
test: Marksheet is deliberately a subset of XLSX -- themes, charts, drawings,
macros, pivot tables and most styling have no representation, which is why
every import reports `fidelity: "lossy"`. What must hold is that the surviving
subset is a fixed point. Files that cannot import at all are skipped rather
than failed, since several are deliberately defective.

Current state over all 94 corpus files: **61 stable, 27 unstable, 6 skipped**
— and every one of the 27 is the same benign class, with no export or
re-import failures left at all.

**Style records are not deduplicated on import.** The first import can emit
several identical style records where one would do; exporting and re-importing
collapses them. On `gsheets/gantt_chart.xlsx` the count goes 105 → 60 → 60:
different identifiers, identical cell values, and a fixed point after one pass.
Nothing is lost — the same formatting resolves for every cell — but the `.ms` a
workbook produces depends on how it arrived, which is not ideal for a
Git-friendly format that promises canonical output. Deduplicating equal style
property sets as they are interned during import would close it.

It is recorded rather than fixed, per the same call as before: it costs no
data, and the fix belongs with a deliberate decision about canonical style
ordering in the exporter.
