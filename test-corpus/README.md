# XLSX test corpus

172 `.xlsx`/`.xlsm` files for exercising `marksheet-convert`'s XLSX importer,
`marksheet-calc`'s formula engine, and the viewer: **35 synthetic** files
(this directory) spanning a deliberate complexity gradient and nearly every
Excel worksheet function, plus **137 real-world files** (in
[`real-world/`](real-world/)) written by nine different producers — Microsoft
Excel, Google Sheets, LibreOffice, the C++, Go, PHP, .NET and Python
spreadsheet libraries, and three public bodies publishing real statistical
workbooks — each with its source and license tracked. See
[Why generated, not downloaded](#why-generated-not-downloaded) for why the two
subsets are built differently.

**Synthetic corpus (this directory):**
- **All 35 files import cleanly** via `marksheet convert --to marksheet`
  (all report `fidelity: "lossy"`, which is expected: most files deliberately
  use formulas and features outside the current `portable-a1@1` profile).
- The five tier-6 files were minimal regression fixtures for compatibility
  gaps found while building this corpus. **All of them now import**, along
  with the rest -- see [Fixed](#fixed) below.
- **531 formula rows, 514 distinct Excel function names** in
  `30_formula_function_showcase.xlsx` alone, spanning every standard Excel
  function category. Verified end to end: `marksheet convert` reports 35
  formulas `translated` (exactly the 35 functions in `portable-a1@1`) and 496
  `replaced` (everything else, gracefully downgraded rather than failing).

**Real-world subset ([`real-world/`](real-world/)):** **130 of 137** import
cleanly; 6 of the other 7 are *supposed* to fail (encrypted, deliberately
corrupted, a truncated fuzzer input, an XML bomb, a workbook declaring no
sheets, and case-aliased duplicate ZIP entries), and the last is a recorded
gap. This subset has found **17 defects**,
the largest being that `mc:Ignorable`-marked Microsoft extension attributes
(`x14ac:dyDescent`, present in nearly every file Excel has saved since ~2010)
were rejected outright rather than honored as ignorable per ECMA-376 Part 3.
Public **Google Sheets** exports were the single most productive source, at
one defect per two files. Per-file provenance, licenses, and the full list of
bugs and fixes are in [`real-world/README.md`](real-world/README.md).

**Round-trip:** `roundtrip.sh` checks that `xlsx -> ms -> xlsx -> ms` is a
fixed point -- **61 stable, 27 unstable, 6 skipped**. Comparing the
re-exported `.xlsx` byte-for-byte against the original would be the wrong bar,
since Marksheet is deliberately a subset of XLSX; what must hold is that the
surviving subset stops changing. All 27 are one benign class: style records are not
deduplicated on import, so a round trip collapses them (105 -> 60 -> 60 on one
file) with identical cell values and a fixed point after one pass. It found
further defects along the way, all fixed --
[details](real-world/README.md#round-trip-stability).

**`--strict`:** conversion is permissive by default and reports what it
degraded. `convert --strict` refuses any import that is not `lossless`, and
any export whose formulas evaluate to `#CIRC!`/`#NAME?` -- for callers that
want a gate rather than a report.

## Regenerating

```bash
cd test-corpus
python3 -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt
python3 generate.py
```

This rewrites everything under `xlsx/` and `manifest.json` deterministically
(same script, same output, modulo the intentionally-random-but-seeded sample
data in a few files). `xlsx/` is gitignored; `generate.py`,
`functions_catalog.py`, `manifest.json`, and this README are the checked-in,
reproducible artifacts.

To re-verify against the app:

```bash
cargo build -p marksheet-cli
./test-corpus/verify.sh
```

And to check that conversion is a fixed point (`xlsx -> ms -> xlsx -> ms`):

```bash
./test-corpus/roundtrip.sh
```

## Why generated, not downloaded

[`tests/conversion/README.md`](../tests/conversion/README.md) states this
project's own conformance fixtures "deliberately does not include copied
Office files" -- `sources/*.xlsx.json` describes deterministic generated
OOXML instead of checking in binaries. This corpus follows the same spirit:
`generate.py` + `functions_catalog.py` are the checked-in, reviewable,
regenerable source; the `.xlsx`/`.xlsm` binaries it produces are gitignored
build output, not something to diff in a PR.

That means this directory's corpus is 100% synthetic. It's good for
exercising specific formulas/features and for scale (2000-row sheets,
51-sheet workbooks, 514-function showcases), but it can't reproduce the
accumulated real-world weirdness of files that actually passed through five
different tools and a decade of manual edits -- that's what
[`real-world/`](real-world/) is for. Those files genuinely are copied
third-party files (from Apache POI and calamine, both permissively licensed
with full attribution in `real-world/LICENSES/`), so they live in their own
subdirectory with their own provenance manifest rather than being mixed into
this generated set.

## Directory layout

```
test-corpus/
├── README.md              this file
├── requirements.txt        openpyxl>=3.1
├── generate.py              the generator (run this)
├── functions_catalog.py      531-row Excel function catalog, used by generate.py
├── manifest.json            machine-readable file/tier/description index (generate.py's output)
├── verify.sh                imports every corpus file and reports fidelity per file
├── roundtrip.sh             asserts xlsx -> ms -> xlsx -> ms is a fixed point
├── xlsx/                    generated .xlsx/.xlsm files (gitignored)
└── real-world/              137 real files from 9 producers -- see real-world/README.md
    ├── README.md              source/license/findings for this subset
    ├── manifest.json          machine-readable provenance per file
    ├── sources.json          the GitHub sources, pinned by commit SHA
    ├── gsheets.json          the public Google Sheets group
    ├── govdata.json          workbooks published by public bodies
    ├── download.sh            fetches everything
    ├── build_manifest.py      regenerates manifest.json
    ├── LICENSES/              license text for every source, incl. POI NOTICE
    └── xlsx/                  downloaded files (gitignored), one subdir per producer
```

## The corpus

### Tier 1 -- Minimal

| File | Title | Description |
| --- | --- | --- |
| `01_empty_workbook.xlsx` | Empty workbook | One sheet, zero cells. Lower bound for a workbook the importer must still accept cleanly. |
| `02_single_cell.xlsx` | Single cell | One sheet, one literal number in A1. |
| `03_scalar_values_no_formulas.xlsx` | Scalar values, no formulas | Every literal scalar type (int, negative, decimal, text, blank, bool, date, datetime, currency/percent number formats) with no formulas at all, plus a sparse gap before a trailing value. |

### Tier 2 -- Simple, everyday

| File | Title | Description |
| --- | --- | --- |
| `04_personal_budget.xlsx` | Personal monthly budget | Classic budget sheet: SUM totals, a per-row subtraction formula, currency formatting. |
| `05_grade_book.xlsx` | Class grade book | AVERAGE per student, nested IF letter grading, and MAX/MIN/COUNT class statistics -- every one of these functions is inside Marksheet's portable-a1@1 profile. |
| `06_todo_checklist.xlsx` | To-do checklist | Dropdown data validation, conditional-formatting highlight, and two functions outside the portable profile (TODAY, COUNTIF) alongside a supported IF. |
| `07_invoice.xlsx` | Client invoice | Merged header, a line-item table with per-row multiplication, SUM subtotal, tax formula, and grand total. Borders and currency formatting throughout. |
| `08_unit_conversion.xlsx` | Unit conversion table | A single workbook-scoped defined name (km_to_miles) referenced by ten independent formulas -- tests one-to-many defined-name fan-out. |
| `09_loan_payment_calculator.xlsx` | Loan payment calculator | Single-sheet amortization inputs feeding a PMT() financial formula -- PMT is outside the portable-a1@1 profile, so this exercises the lossy/replaced formula path. |

### Tier 3 -- Intermediate

| File | Title | Description |
| --- | --- | --- |
| `10_multi_sheet_sales_report.xlsx` | Multi-sheet sales report | Five sheets (RawData + 3 region sheets + Summary) chained by cross-sheet references, SUMIF/COUNTIF/AVERAGEIF per region, then a Summary sheet that reads each region sheet's own formula result. |
| `11_lookup_directory.xlsx` | Employee lookup directory | Two sheets: an Employees table and a Lookups sheet exercising VLOOKUP, HLOOKUP, INDEX+MATCH (both in the portable profile), and XLOOKUP (not) against it. |
| `12_named_ranges_tax_calc.xlsx` | Named-range tax calculator | Two workbook-scoped defined names (tax_rate, standard_deduction) resolved inside ordinary arithmetic formulas. (Sheet-scoped names are exercised separately in 32_sheet_scoped_defined_name.xlsx.) |
| `13_excel_table_orders.xlsx` | Excel Table with calculated column | A real Excel Table (ListObject) named 'Orders' with a calculated column driven by structured references ([@Qty]*[@Price]). |
| `14_conditional_formatting_dashboard.xlsx` | KPI dashboard with conditional formatting | Color scale, data bar, icon set, and a formula-based cell rule stacked on the same KPI table, alongside a plain division formula. |
| `15_data_validation_form.xlsx` | Data validation form | One cell each for list, whole-number range, decimal range, date range, text-length, and custom-formula data validation rules. |
| `16_frozen_panes_large_grid.xlsx` | Frozen-pane large grid | 200x14 numeric grid with a frozen header row/column, explicit column widths, a color-scale heat map, and SUM column totals. |

### Tier 4 -- Complex

| File | Title | Description |
| --- | --- | --- |
| `17_financial_model_3_statement.xlsx` | Three-statement financial model | Assumptions + Income Statement + Balance Sheet + Cash Flow, chained with defined-name driven formulas across sheets, plus NPV and IRR. |
| `18_large_dataset_2000_rows.xlsx` | Large transaction dataset | 2000 rows of transactional data plus a 16-row SUMIFS/COUNTIFS/AVERAGEIFS cross-tab summary reading the full range -- a scale/performance stress case. |
| `19_array_and_dynamic_formulas.xlsx` | Array and dynamic-array formulas | Legacy CSE-style array formulas (SUMPRODUCT, array-entered SUM/IF) next to modern dynamic-array functions (FILTER, SORT, SORTBY, UNIQUE, SEQUENCE) and a spill reference. |
| `20_error_handling_showcase.xlsx` | Every Excel runtime error, deliberately triggered | One row per error class SPEC.md section 13 actually requires (#DIV/0!, #N/A, #VALUE!, #REF!, #NAME?, #NUM!) plus the matching IFERROR/IFNA-wrapped version and ISERROR/ISNA checks. (#NULL! is out of scope for portable-a1@1 and is exercised separately in 33_null_error_token.xlsx.) |
| `21_circular_reference.xlsx` | Circular references | A mutual two-cell circular pair, a single self-loop, and a downstream formula that depends on the circular pair -- SPEC.md requires every cell in the strongly connected component (and its dependents) to resolve to #CIRC!. |
| `22_charts.xlsx` | Embedded charts | A bar chart, line chart, and pie chart embedded alongside their source data -- exercises the Chart/Unsupported import path (crates/marksheet-convert import.rs treats any xl/charts/* part as outside the initial import profile). |
| `23_pivot_table_source.xlsx` | Pivot table (stub) over source data | Real source data plus a minimal, hand-authored xl/pivotCache + xl/pivotTables part pair -- enough to exercise Marksheet's PivotTable/Unsupported detection, though it is not a substitute for a real Excel-authored pivot table (see README.md). |

### Tier 5 -- Edge cases and interop

| File | Title | Description |
| --- | --- | --- |
| `24_unicode_and_special_characters.xlsx` | Unicode and special characters | Emoji, CJK, RTL Arabic/Hebrew, accented Latin, a doubled-double-quote escape sequence, and a unicode sheet name referenced from a formula. |
| `25_many_sheets.xlsx` | Many sheets | 51 sheets total, each formula-chained to the previous one (Sheet02!A1 = Sheet01!A1+1, and so on), read back from an Index sheet. |
| `26_merged_cells_comments_hyperlinks.xlsx` | Merged cells, comments, and hyperlinks | A merged title band, a 2x2 merged block, two cell comments, an internal same-sheet hyperlink, and a hidden column. (An external hyperlink is exercised separately in 34_external_hyperlink.xlsx -- see its notes.) |
| `27_macro_enabled.xlsm` | Macro-enabled workbook | Package-level content type flipped to macroEnabled.main plus a placeholder xl/vbaProject.bin part -- exercises is_macro_enabled() and the vbaproject-name detection path in crates/marksheet-convert/src/xlsx/import.rs. |
| `28_number_formats_and_styles.xlsx` | Number formats and styles matrix | Currency (3 locales), accounting, percentage, scientific, fraction, a custom parenthesized-negative format, four date/time formats, and a custom text wrapper, plus a second sheet of font/fill/border styling and row/column outline (grouping) levels. (A '[Red]'-colored negative format is exercised separately in 35_custom_format_date_false_positive.xlsx -- see its notes.) |
| `29_external_links_and_name_errors.xlsx` | External workbook links and #NAME? errors | A formula authored against another workbook ('[Budget-2023.xlsx]Summary'!$B$2), a stub xl/externalLinks/ part exercising the ExternalLink/Unsupported path, and a reference to an undefined name (#NAME?). |
| `30_formula_function_showcase.xlsx` | Every Excel function, one row each | 531 formula rows across 14 category sheets covering 514 distinct Excel function names (Math & Trig, Statistical, Lookup & Reference, Text, Logical, Date & Time, Financial, Information, Engineering, Database, Web, Dynamic Array, Cube, and legacy Compatibility aliases) -- the direct "every Excel formula represented" file. |
| `33_null_error_token.xlsx` | #NULL! / space-intersection formula | A range-intersection formula (space operator) that evaluates to #NULL! in real Excel. SPEC.md section 13's required runtime-failure table does not include #NULL! (or #SPILL!) -- verified behavior: the importer does NOT reject the file for this, it gracefully substitutes the formula ("unsupported Excel formula was replaced with #NAME?", a normal lossy/approximated outcome), unlike the true rejection cases in the 6-regression tier below. |

### Tier 6 -- Regression fixtures (each was a real bug found here, now fixed)

| File | Title | Description |
| --- | --- | --- |
| `31_openpyxl_absolute_relationship_targets.xlsx` | Regression: openpyxl's absolute relationship targets | Left exactly as openpyxl wrote it (every other file in this corpus has this normalized away). openpyxl always emits package-absolute relationship targets (Target="/xl/worksheets/sheet1.xml") for worksheets/tables/charts/comments. crates/marksheet-convert/src/xlsx/package.rs's resolve_target() rejects any target starting with "/" outright (MS4105, 'relationship target is absolute, external, or malformed'), which used to fail this file outright. Since openpyxl is one of the most common tools people use to generate .xlsx programmatically, this was a real compatibility gap rather than a contrived edge case. Now fixed; kept as the regression fixture, with the original targets preserved rather than normalized. |
| `32_sheet_scoped_defined_name.xlsx` | Sheet-scoped (local) defined name | A single defined name scoped to one sheet rather than the workbook. crates/marksheet-convert used to reject the whole file for this ("sheet-local defined names are outside the initial profile", MS4105). Now fixed; kept as the regression fixture. |
| `34_external_hyperlink.xlsx` | External hyperlink relationship | One ordinary external hyperlink (TargetMode="External"). crates/marksheet-convert/src/xlsx/package.rs rejects any package relationship with TargetMode="External" during its upfront package-wide relationship validation, before per-feature handling runs ("external OOXML relationships are rejected", MS4105) -- so an external hyperlink anywhere in the workbook used to take down the entire import, not just that cell's link. Now fixed; kept as the regression fixture. Internal same-sheet hyperlinks were always unaffected -- see 26_merged_cells_comments_hyperlinks.xlsx. |
| `35_custom_format_date_false_positive.xlsx` | Custom number format false-positives as a date | A negative number under the extremely common "0.00;[Red]-0.00" custom format (red negatives, no color scale). crates/marksheet-convert/src/xlsx/import.rs's apply_number_format classified any custom format code containing the letter "y" or "d" as Date/DateTime -- and "[Red]" contains a "d", so this ordinary format was misread as a date and the negative value rejected outright ("negative Excel date serial is outside the initial profile", MS4105). Now fixed; kept as the regression fixture. |

## Fixed

Building and then actually verifying every file against `marksheet convert`
(rather than assuming success) surfaced four gaps, each isolated into its own
minimal tier-6 fixture above. **All four are now fixed**, with regression
tests; the fixtures remain as the regression corpus. The real-world subset
found six more -- see
[`real-world/README.md`](real-world/README.md#bugs-this-subset-found-and-how-they-were-fixed).

1. **openpyxl's absolute relationship targets** (`31_...xlsx`) --
   `resolve_target()` in
   [`package.rs`](../crates/marksheet-convert/src/xlsx/package.rs) rejected any
   OPC relationship `Target` starting with `/`. openpyxl -- arguably the single
   most common way to generate `.xlsx` files programmatically -- always writes
   worksheet/table/chart/comment relationships that way, so **every file
   openpyxl produced was rejected outright**. The other 34 files in this corpus
   sidestep it via `normalize_absolute_rels` in `generate.py`; this one is left
   unnormalized so the fixture keeps testing the real shape.
2. **Sheet-scoped defined names** (`32_...xlsx`) -- one sheet-local (rather
   than workbook-scoped) defined name rejected the entire workbook.
3. **External relationships, including ordinary hyperlinks** (`34_...xlsx`) --
   the upfront package-wide relationship validation rejected *any*
   `TargetMode="External"` relationship anywhere in the package, before
   per-feature handling (the `hyperlinks` → omitted-feature path) ever ran, so a
   single ordinary web hyperlink took down the whole import. A same-sheet
   internal hyperlink authored via `<hyperlink location="...">`, which needs no
   relationship at all, was always fine -- see `26_...xlsx`.
4. **Custom number formats false-positived as dates** (`35_...xlsx`) --
   `apply_number_format`'s custom-format sniffer treated any format code
   containing the letter `y` or `d` as a date/time format. `0.00;[Red]-0.00`
   (an extremely common "red negatives" format) matched on the `d` in `"Red"`,
   so a plain negative number was read as a date and then rejected as a
   "negative Excel date serial".

None of these needed adversarial input -- they all came from ordinary
workbooks built with a standard Python library and standard Excel formatting
conventions. Two of them (1 and 3) were independently confirmed against real
Excel- and LibreOffice-authored files in [`real-world/`](real-world/).

## CLI note: `.xlsm` inference (fixed)

`27_macro_enabled.xlsm` is a well-formed macro-enabled workbook -- verified
by renaming it to `.xlsx` before invoking the CLI, which then reports the
expected `macro`/`omitted` outcomes (`"VBA macros are not imported"` and
`"macro-enabled workbook content type was imported without VBA semantics"`)
cleanly. `marksheet convert` run directly against the `.xlsm` path used to
fail with `cannot infer input format ...`, because the CLI's
extension-inference list omitted it. It now accepts `.xlsm`, `.xltx` and
`.xltm` alongside `.xlsx` -- all the same OOXML package, whose macro content
the importer already reports as omitted rather than refusing.

## External real-world corpora

Synthetic files can't replicate real-world accumulated weirdness (legacy
formats, five-tools-and-a-decade-of-edits artifacts, genuinely adversarial
malformed OOXML), so [`real-world/`](real-world/) pulls 22 curated files
from the two best candidate sources found. Full research writeup, including
why the other candidates below were passed over, is in
[`real-world/README.md`](real-world/README.md#why-these-two-sources-not-others):

| Source | License | Verdict |
| --- | --- | --- |
| [Apache POI `test-data/spreadsheet`](https://github.com/apache/poi/tree/trunk/test-data/spreadsheet) | Apache-2.0 | **Used** (13 files). 829 files total, real bug-report attachments, deep formula/chart/pivot/macro coverage, actively maintained. |
| [calamine `tests/`](https://github.com/tafia/calamine/tree/master/tests) | MIT | **Used** (9 files). 136 files total, zero attribution friction, good for parser edge cases (dates, merges, malformed OOXML). |
| [SheetJS `enron_xls`](https://github.com/SheetJS/enron_xls) | CC0-1.0 | Not used. Mostly legacy formats (BIFF2/TSV/SYLK), not modern OOXML; contains real employee names/business data despite the clean license. |
| SheetJS `test_files` (the "canonical" one) | was Apache-2.0 | **Skip.** GitHub has it access-blocked for embedded private/personal information (confirmed via API, not just archived/moved) -- do not attempt to reconstruct or mirror it. |
| EUSES / FUSE academic corpora | unclear / dead host | **Skip.** No stated license on the current EUSES mirror; FUSE's distribution point 404s. |
| Sheetpedia (2025) | CC BY-SA 4.0 | **Skip for direct copying.** Share-alike terms are real friction against this MIT-licensed repo; also shipped as WebDataset shards, not individual files. |
| data.gov / data.gov.uk | OGL v3 / public domain | Fine license-wise, but realistic government reporting workbooks skew toward straightforward layouts -- a nice-to-have complement, not a stress test. Not pulled in. |
