#!/usr/bin/env python3
"""Builds manifest.json: per-file source, license, pinned commit and reason.

Reads sources.json (GitHub repos, pinned by commit SHA) and gsheets.json
(public Google Sheets exported to XLSX). Run after download.sh.

Verification outcomes are deliberately not recorded here -- they change as the
importer evolves. `../verify.sh` is the live report; README.md summarises it.
"""
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent

# Why each file earns its place. Anything not listed still ships, with the
# group's generic note.
NOTES = {
    # Apache POI -- real bug-report attachments and fuzzer findings.
    "poc-xmlbomb.xlsx": "XML entity-expansion (\"billion laughs\") payload.",
    "clusterfuzz-testcase-minimized-XLSX2CSVFuzzer-5636439151607808.xlsx": "OSS-Fuzz minimized crash input.",
    "MalformedSSTCount.xlsx": "Shared-string table with a wrong declared count.",
    "xlsx-corrupted.xlsx": "Deliberately corrupted OOXML package.",
    "link-external-workbook-a.xlsx": "External-workbook reference pair (references -b).",
    "link-external-workbook-b.xlsx": "Other half of the external-workbook pair.",
    "unicodeSheetName.xlsx": "Non-ASCII sheet name.",
    "NewlineInFormulas.xlsx": "Formula text containing an embedded newline.",
    "shared_formulas.xlsx": "OOXML shared-formula groups, plus a #REF! defined name.",
    "ExcelPivotTableSample.xlsx": "Genuine Excel-authored pivot table.",
    "WithTwoCharts.xlsx": "Two embedded charts and a workbook-level defined name.",
    "SimpleMacro.xlsm": "Real macro-enabled workbook with a VBA project.",
    "workbookProtection-workbook_password-2013.xlsx": "Password-protected workbook structure.",
    # calamine
    "table_with_absolute_paths.xlsx": "calamine's own fixture for package-absolute OPC targets.",
    "hyperlinks.xlsx": "Real hyperlink relationships.",
    "merged_range.xlsx": "Merged cell ranges.",
    "errors.xlsx": "Excel-authored error values (#DIV/0!, #N/A, ...).",
    "date_1904.xlsx": "1904 date system; LibreOffice-written, with a loext extension.",
    "pivots.xlsx": "Real pivot tables, plus a UTF-16 customXml part.",
    "vba.xlsm": "Real macro-enabled workbook.",
    "strict_iso_paths.xlsx": "OOXML Strict (ISO/IEC 29500) variant.",
    "pass_protected.xlsx": "Encrypted workbook: an OLE/CFB container, not a ZIP.",
    # xlnt -- C++ writer, conformance-minded fixtures.
    "2_minimal.xlsx": "Minimal conforming package: the workbook part sits at the package root, not under xl/.",
    "4_every_style.xlsx": "Exercises the full style surface in one file.",
    "10_comments_hyperlinks_formulae.xlsx": "Comments, hyperlinks and formulas, with a dangling calcChain relationship.",
    "11_print_settings.xlsx": "Print setup, including pageSetup relationships.",
    "12_advanced_properties.xlsx": "Extended and custom document properties.",
    "15_phonetics.xlsx": "Japanese phonetic (furigana) runs.",
    "17_xlsm.xlsm": "Macro-enabled workbook from a non-Microsoft writer.",
    "18_formulae.xlsx": "Formula-dense sheet.",
    "19_defined_names.xlsx": "Defined names of several shapes.",
    "9_unicode_Λ_😇.xlsx": "Non-ASCII content and a non-ASCII file name including an emoji.",
    # pandas -- openpyxl/xlsxwriter output plus dimension edge cases.
    "dimension_missing.xlsx": "Worksheet with no <dimension> element.",
    "dimension_small.xlsx": "<dimension> smaller than the actual used range.",
    "dimension_large.xlsx": "<dimension> larger than the actual used range.",
    "times_1904.xlsx": "1904 date system with time values.",
    "times_1900.xlsx": "1900 date system with time values.",
    "test_spaces.xlsm": "Macro-enabled workbook with significant whitespace.",
    # excelize -- Go writer.
    "SharedStrings.xlsx": "Shared-string table from a Go writer.",
    "MergeCell.xlsx": "Merged cells from a Go writer.",
    "CalcChain.xlsx": "Declares xl/SharedStrings.xml but relates to sharedStrings.xml -- OPC part names are case-insensitive.",
    "BadWorkbook.xlsx": "Deliberately invalid: declares <sheets></sheets>.",
    "OverflowNumericCell.xlsx": "Numeric cell beyond the representable range.",
    # PhpSpreadsheet -- PHP writer.
    "ConditionalFormattingConditions.xlsx": "Conditional-formatting rule variety from a PHP writer.",
    "ColourScale.xlsx": "Colour-scale conditional formatting.",
    "31docproperties.xlsx": "Document properties from a PHP writer.",
    "TableFormat.xlsx": "Excel Table written by PhpSpreadsheet.",
    "26template.xlsx": "General-purpose template workbook.",
    # ClosedXML -- .NET writer.
    "AddingComments.xlsx": "Cell comments from a .NET writer.",
    "CFDataBarNegative.xlsx": "Data-bar conditional formatting with negative values.",
    "CFColorScaleLowMidHigh.xlsx": "Three-stop colour-scale conditional formatting.",
    "RegularAutoFilter.xlsx": "AutoFilter definition.",
}


def build():
    entries = []
    for group, spec in json.loads((HERE / "sources.json").read_text()).items():
        for name in spec["files"]:
            base = name.rsplit("/", 1)[-1]
            entries.append({
                "file": f"{group}/{base}",
                "producer": group,
                "kind": "github",
                "source_repo": spec["repo"],
                "source_path": f"{spec['path']}/{name}",
                "source_url": f"https://github.com/{spec['repo']}/blob/{spec['sha']}/{spec['path']}/{name}",
                "commit": spec["sha"],
                "license": spec["license"],
                "note": NOTES.get(base, "Real-world fixture from this project's own test suite."),
            })

    govdata = HERE / "govdata.json"
    if govdata.exists():
        for entry in json.loads(govdata.read_text()):
            entries.append({
                "file": f"govdata/{entry['file']}",
                "producer": "govdata",
                "kind": "published-workbook",
                "source_url": entry["url"],
                "publisher": entry["publisher"],
                "license": entry["license"],
                "sha256_first_seen": entry["sha256_first_seen"],
                "bytes_first_seen": entry["bytes_first_seen"],
                "stability": entry["stability"],
                "note": entry["note"],
            })

    gsheets = HERE / "gsheets.json"
    if gsheets.exists():
        for entry in json.loads(gsheets.read_text()):
            entries.append({
                "file": f"gsheets/{entry['file']}",
                "producer": "google-sheets",
                "kind": "google-sheet",
                "source_url": f"https://docs.google.com/spreadsheets/d/{entry['id']}/edit",
                "export_url": f"https://docs.google.com/spreadsheets/d/{entry['id']}/export?format=xlsx",
                "license": "Publicly shared template; placeholder content only, no personal data.",
                "sha256_first_seen": entry["sha256_first_seen"],
                "bytes_first_seen": entry["bytes_first_seen"],
                "note": entry["note"],
            })

    (HERE / "manifest.json").write_text(json.dumps(entries, indent=2) + "\n")
    print(f"Wrote {len(entries)} entries to manifest.json")


if __name__ == "__main__":
    build()
