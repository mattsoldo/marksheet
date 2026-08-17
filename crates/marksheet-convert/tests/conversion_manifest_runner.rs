//! Executable coverage for the checked-in conversion fixture corpus.
//!
//! The converter is intentionally a bytes/IR library: it neither parses
//! Marksheet source nor chooses or writes destination paths.  This runner
//! therefore parses each declared Marksheet source through the public syntax
//! API and generates the documented OOXML fixture archives in memory. CLI
//! tests own canonical Marksheet serialization and atomic file output.

use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use marksheet_convert::{
    ConversionLimits, ConvertError, CsvExportSelection, CsvImportSelection, FormatDescriptor,
    export_csv, export_xlsx, import_csv, import_xlsx,
};
use marksheet_model::{
    Block, Cell, Coordinate, Range, Sheet, SheetId, SheetItem, TableId, Value, Workbook,
};
use marksheet_syntax::parse;
use serde_json::{Map, Value as JsonValue};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

const FIXTURE_PROTOCOL: &str = "marksheet-conversion-fixture@1";
const MANIFEST_PROTOCOL: &str = "marksheet-conversion-conformance@1";

#[test]
fn conversion_manifest_runner() {
    let root = fixture_root();
    let manifest: JsonValue = read_json(&root.join("manifest.json"));
    let report_schema = read_json(&root.join("report.schema.json"));
    assert_eq!(manifest["version"], 1);
    assert_eq!(manifest["protocol"], MANIFEST_PROTOCOL);
    let cases = manifest["cases"].as_array().expect("manifest cases array");
    assert_eq!(
        cases.len(),
        10,
        "all known conversion cases stay executable"
    );

    for case in cases {
        let fixture_name = case["fixture"].as_str().expect("case fixture");
        let fixture: JsonValue = read_json(&root.join(fixture_name));
        assert_eq!(fixture["protocol"], FIXTURE_PROTOCOL, "{fixture_name}");
        run_case(&fixture, &root, &report_schema);
    }
}

#[test]
fn generated_chart_and_macro_parts_are_relationship_bound() {
    let root = fixture_root();
    let descriptor = read_json(&root.join("sources/import_unsupported.xlsx.json"));
    let bytes = materialize_xlsx_descriptor(&descriptor);
    assert_descriptor_inventory(&bytes, &descriptor);
    let conversion = import_xlsx(&bytes, ConversionLimits::default())
        .expect("relationship-bound chart and macro package imports safely");
    let features = conversion
        .report
        .outcomes()
        .iter()
        .map(|event| event.feature.as_str())
        .collect::<Vec<_>>();
    assert!(features.contains(&"chart"));
    assert!(features.contains(&"macro"));
}

fn run_case(fixture: &JsonValue, root: &Path, report_schema: &JsonValue) {
    let request = fixture["request"].as_object().expect("fixture request");
    let expect = fixture["expect"].as_object().expect("fixture expectation");
    let source = request["source"].as_object().expect("request source");
    let source_format = source["format"].as_str().expect("source format");
    let destination = request["destination"].as_str().expect("destination format");
    match (source_format, destination) {
        ("marksheet", "xlsx" | "csv") => {
            run_marksheet_export_case(source, request, expect, report_schema, root);
        }
        ("csv", "marksheet") => run_csv_import_case(source, request, expect, report_schema, root),
        ("xlsx", "marksheet") => run_xlsx_import_case(source, request, expect, report_schema, root),
        unsupported => panic!("unsupported fixture conversion {unsupported:?}"),
    }
}

fn run_marksheet_export_case(
    source: &Map<String, JsonValue>,
    request: &Map<String, JsonValue>,
    expect: &Map<String, JsonValue>,
    report_schema: &JsonValue,
    root: &Path,
) {
    let workbook = load_marksheet_source(source, root);
    match request["destination"].as_str().expect("destination format") {
        "xlsx" => {
            let conversion =
                export_xlsx(&workbook, ConversionLimits::default()).expect("Marksheet XLSX export");
            assert!(
                conversion.value.starts_with(b"PK"),
                "XLSX is a ZIP artifact"
            );
            assert_report(&conversion.report, request, expect, report_schema, root);
        }
        "csv" if request.contains_key("selection") => {
            let conversion = export_csv(
                &workbook,
                &csv_export_selection(request["selection"].as_object().expect("CSV selection")),
                ConversionLimits::default(),
            )
            .expect("CSV export with explicit selection");
            assert!(
                conversion.value.ends_with(b"\n"),
                "CSV has terminal line feed"
            );
            assert_report(&conversion.report, request, expect, report_schema, root);
        }
        "csv" => assert_missing_csv_selection(request, expect, report_schema, root),
        destination => panic!("unsupported Marksheet destination {destination:?}"),
    }
}

fn assert_missing_csv_selection(
    request: &Map<String, JsonValue>,
    expect: &Map<String, JsonValue>,
    report_schema: &JsonValue,
    root: &Path,
) {
    // There is no `None` selection in the library API: its type makes an
    // explicit selection mandatory. The CLI owns this argument-absence adapter.
    let report = ConvertError::invalid_selection("CSV conversion requires one selection")
        .unsupported_report(
            FormatDescriptor::marksheet_ir(),
            FormatDescriptor::csv(),
            "csv_selection",
        );
    assert_report(&report, request, expect, report_schema, root);
}

fn run_csv_import_case(
    source: &Map<String, JsonValue>,
    request: &Map<String, JsonValue>,
    expect: &Map<String, JsonValue>,
    report_schema: &JsonValue,
    root: &Path,
) {
    let Some(target) = request.get("import_target") else {
        let report = ConvertError::invalid_selection("CSV import requires one target")
            .unsupported_report(
                FormatDescriptor::csv(),
                FormatDescriptor::marksheet_ir(),
                "csv_import_target",
            );
        assert_report(&report, request, expect, report_schema, root);
        return;
    };
    let conversion = import_csv(
        &load_source_bytes(source, root),
        &csv_import_selection(target.as_object().expect("CSV import target")),
        ConversionLimits::default(),
    )
    .expect("CSV import with explicit target");
    assert_eq!(conversion.value.sheets.len(), 1);
    assert_report(&conversion.report, request, expect, report_schema, root);
}

fn run_xlsx_import_case(
    source: &Map<String, JsonValue>,
    request: &Map<String, JsonValue>,
    expect: &Map<String, JsonValue>,
    report_schema: &JsonValue,
    root: &Path,
) {
    let descriptor_path = resolve_source_fixture(source, root);
    let descriptor = read_json(&descriptor_path);
    let bytes = materialize_xlsx_descriptor(&descriptor);
    assert_descriptor_inventory(&bytes, &descriptor);
    let mut limits = ConversionLimits::default();
    if let Some(maximum) = request
        .get("limits")
        .and_then(|limits| limits["max_uncompressed_bytes"].as_u64())
    {
        limits.max_zip_total_uncompressed_bytes = maximum;
    }
    match import_xlsx(&bytes, limits) {
        Ok(conversion) => {
            assert_eq!(
                conversion
                    .value
                    .sheets
                    .iter()
                    .map(|sheet| &sheet.label)
                    .collect::<Vec<_>>(),
                descriptor["sheets"]
                    .as_array()
                    .expect("descriptor sheets")
                    .iter()
                    .map(|label| label.as_str().expect("sheet label"))
                    .collect::<Vec<_>>(),
                "descriptor sheet labels must be materialized"
            );
            assert_report(&conversion.report, request, expect, report_schema, root);
        }
        Err(failure) => {
            let feature = expect["outcomes"]
                .as_array()
                .expect("expected outcomes")
                .iter()
                .find(|outcome| outcome["outcome"] == "unsupported")
                .and_then(|outcome| outcome["feature"].as_str())
                .expect("unsupported import fixture feature");
            assert_eq!(failure.report.outcomes()[0].feature, feature);
            assert_report(&failure.report, request, expect, report_schema, root);
        }
    }
}

fn assert_report(
    report: &marksheet_convert::ConversionReport,
    request: &Map<String, JsonValue>,
    expectation: &Map<String, JsonValue>,
    report_schema: &JsonValue,
    root: &Path,
) {
    let actual = serde_json::to_value(report).expect("report serializes");
    assert_report_schema(&actual, report_schema);
    assert_eq!(actual["schema"], "marksheet-conversion@1");
    assert_eq!(actual["source"]["format"], request["source"]["format"]);
    assert_eq!(actual["destination"]["format"], request["destination"]);
    assert_eq!(actual["fidelity"], expectation["fidelity"]);
    assert_report_invariants(&actual);
    let source_bytes = request["source"]
        .as_object()
        .filter(|source| source["format"] == "marksheet")
        .map(|source| load_source_bytes(source, root));

    let outcomes = actual["outcomes"].as_array().expect("report outcomes");
    assert_ordered_outcomes(
        outcomes,
        expectation["outcomes"]
            .as_array()
            .expect("expected outcomes"),
        source_bytes.as_deref(),
    );
    let diagnostics = actual["diagnostics"]
        .as_array()
        .expect("report diagnostics");
    assert_ordered_diagnostics(
        diagnostics,
        expectation["diagnostics"]
            .as_array()
            .expect("expected diagnostics"),
        source_bytes.as_deref(),
    );

    // Fixture `artifact` is a CLI path contract. At this public API boundary
    // successful calls return the artifact in memory; errors return no value.
    if expectation.contains_key("artifact") {
        assert_ne!(actual["fidelity"], "unsupported");
    } else {
        assert_eq!(actual["fidelity"], "unsupported");
    }
}

fn assert_ordered_outcomes(
    actual: &[JsonValue],
    expected: &[JsonValue],
    source_bytes: Option<&[u8]>,
) {
    assert_ordered_subsequence(
        actual,
        expected,
        |actual, expected| event_matches(actual, expected, source_bytes),
        "outcome",
    );
}

fn assert_ordered_diagnostics(
    actual: &[JsonValue],
    expected: &[JsonValue],
    source_bytes: Option<&[u8]>,
) {
    assert_ordered_subsequence(
        actual,
        expected,
        |actual, expected| diagnostic_matches(actual, expected, source_bytes),
        "diagnostic",
    );
}

fn assert_ordered_subsequence<F>(
    actual: &[JsonValue],
    expected: &[JsonValue],
    matches: F,
    kind: &str,
) where
    F: Fn(&JsonValue, &JsonValue) -> bool,
{
    let mut next_expected = 0_usize;
    for actual_entry in actual {
        if expected
            .get(next_expected)
            .is_some_and(|expected_entry| matches(actual_entry, expected_entry))
        {
            next_expected += 1;
        }
    }
    assert_eq!(
        next_expected,
        expected.len(),
        "expected {kind} sequence is not an ordered subsequence; expected: {expected:#?}; actual: {actual:#?}"
    );
}

fn assert_report_invariants(report: &JsonValue) {
    let outcomes = report["outcomes"].as_array().expect("report outcomes");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("report diagnostics");
    match report["fidelity"].as_str().expect("fidelity string") {
        "lossless" => {
            assert!(outcomes.iter().all(|event| event["outcome"] == "exact"));
            assert!(diagnostics.is_empty());
        }
        "lossy" => assert!(outcomes.iter().any(|event| {
            matches!(event["outcome"].as_str(), Some("approximated" | "omitted"))
        })),
        "unsupported" => {
            assert!(
                outcomes
                    .iter()
                    .any(|event| event["outcome"] == "unsupported")
            );
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic["severity"] == "error")
            );
        }
        unexpected => panic!("unexpected fidelity {unexpected:?}"),
    }
}

/// Lightweight validation of the checked-in `report.schema.json` contract.
/// Keeping this dependency-free makes the conformance runner usable in the
/// same minimal crate configuration as the converter itself.
fn assert_report_schema(report: &JsonValue, schema: &JsonValue) {
    assert_eq!(
        schema["$id"],
        "https://marksheet.dev/schema/conversion-report-1.json"
    );
    assert_object_keys(
        report,
        &[
            "schema",
            "source",
            "destination",
            "fidelity",
            "outcomes",
            "diagnostics",
        ],
        "report",
    );
    assert_eq!(report["schema"], schema["properties"]["schema"]["const"]);
    for key in ["source", "destination"] {
        assert_object_keys(&report[key], &["format", "version"], key);
        assert!(report[key]["format"].is_string(), "{key}.format");
        assert!(report[key]["version"].is_string(), "{key}.version");
    }
    assert!(
        matches!(
            report["fidelity"].as_str(),
            Some("lossless" | "lossy" | "unsupported")
        ),
        "report fidelity"
    );
    for outcome in report["outcomes"]
        .as_array()
        .expect("schema outcomes array")
    {
        assert_object_keys(
            outcome,
            &["feature", "outcome", "formula", "detail", "locations"],
            "outcome",
        );
        assert!(outcome["feature"].is_string());
        assert!(matches!(
            outcome["outcome"].as_str(),
            Some("exact" | "approximated" | "omitted" | "unsupported")
        ));
        if let Some(formula) = outcome.get("formula") {
            assert!(matches!(
                formula.as_str(),
                Some("preserved" | "translated" | "replaced")
            ));
        }
        if let Some(detail) = outcome.get("detail") {
            assert!(detail.as_str().is_some_and(|value| !value.is_empty()));
        }
        for location in outcome["locations"].as_array().expect("outcome locations") {
            assert_location_schema(location);
        }
    }
    for diagnostic in report["diagnostics"]
        .as_array()
        .expect("schema diagnostics array")
    {
        assert_object_keys(
            diagnostic,
            &["code", "severity", "message", "locations"],
            "diagnostic",
        );
        assert!(
            diagnostic["code"]
                .as_str()
                .is_some_and(|code| code.len() == 6 && code.starts_with("MS"))
        );
        assert!(matches!(
            diagnostic["severity"].as_str(),
            Some("warning" | "error")
        ));
        assert!(
            diagnostic["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
        for location in diagnostic["locations"]
            .as_array()
            .expect("diagnostic locations")
        {
            assert_location_schema(location);
        }
    }
}

fn assert_object_keys(value: &JsonValue, allowed: &[&str], context: &str) {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("{context} must be an object"));
    assert!(
        object.keys().all(|key| allowed.contains(&key.as_str())),
        "{context} contains a field absent from report.schema.json: {object:?}"
    );
}

fn assert_location_schema(location: &JsonValue) {
    let object = location.as_object().expect("location object");
    let kind = object["kind"].as_str().expect("location kind");
    let allowed: &[&str] = match kind {
        "cell" => &["kind", "sheet", "cell"],
        "range" => &["kind", "sheet", "range"],
        "table" => &["kind", "sheet", "table"],
        "sheet" => &["kind", "sheet"],
        "source" => &["kind", "source"],
        "xlsx" => &["kind", "part", "reference"],
        invalid => panic!("invalid report location kind {invalid:?}"),
    };
    assert!(object.keys().all(|key| allowed.contains(&key.as_str())));
    match kind {
        "cell" => {
            assert!(object["sheet"].is_string());
            assert!(object["cell"].is_string());
        }
        "range" => {
            assert!(object["sheet"].is_string());
            assert!(object["range"].is_string());
        }
        "table" => {
            assert!(object["table"].is_string());
            // Optional `sheet` must be omitted when absent. `null` is not in
            // the schema and would leak an implementation detail to clients.
            assert!(object.get("sheet").is_none_or(JsonValue::is_string));
        }
        "sheet" => assert!(object["sheet"].is_string()),
        "source" => assert!(object["source"].is_string()),
        "xlsx" => {
            assert!(object["part"].is_string());
            assert!(
                object
                    .get("reference")
                    .is_none_or(|reference| reference.is_string() || reference.is_null())
            );
        }
        _ => unreachable!(),
    }
}

fn event_matches(actual: &JsonValue, expected: &JsonValue, source_bytes: Option<&[u8]>) -> bool {
    ["feature", "outcome", "formula", "detail"]
        .into_iter()
        .all(|key| {
            expected
                .get(key)
                .is_none_or(|expected_value| actual.get(key) == Some(expected_value))
        })
        && expected.get("location").is_none_or(|expected_location| {
            actual["locations"].as_array().is_some_and(|locations| {
                locations
                    .iter()
                    .any(|actual| location_matches(actual, expected_location, source_bytes))
            })
        })
}

fn diagnostic_matches(
    actual: &JsonValue,
    expected: &JsonValue,
    source_bytes: Option<&[u8]>,
) -> bool {
    ["code", "severity"].into_iter().all(|key| {
        expected
            .get(key)
            .is_none_or(|expected_value| actual.get(key) == Some(expected_value))
    }) && expected.get("location").is_none_or(|expected_location| {
        actual["locations"].as_array().is_some_and(|locations| {
            locations
                .iter()
                .any(|actual| location_matches(actual, expected_location, source_bytes))
        })
    })
}

fn location_matches(actual: &JsonValue, expected: &JsonValue, source_bytes: Option<&[u8]>) -> bool {
    let Some(expected) = expected.as_object() else {
        return false;
    };
    // Fixture source locations intentionally cover both source syntax and
    // OOXML parts. The converter correctly exposes the latter as `kind:xlsx`.
    if let Some(source) = expected.get("source") {
        return actual["source"] == *source
            || actual["part"] == *source
            || source_bytes
                .is_some_and(|bytes| source_span_matches(&actual["source"], source, bytes));
    }
    expected
        .iter()
        .all(|(key, value)| actual.get(key) == Some(value))
}

fn source_span_matches(actual: &JsonValue, expected: &JsonValue, source: &[u8]) -> bool {
    let Some(span) = actual
        .as_str()
        .and_then(|value| value.strip_prefix("bytes:"))
    else {
        return false;
    };
    let Some((start, end)) = span.split_once('-') else {
        return false;
    };
    let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) else {
        return false;
    };
    let Some(bytes) = source.get(start..end) else {
        return false;
    };
    std::str::from_utf8(bytes)
        .ok()
        .is_some_and(|text| text.starts_with(expected.as_str().expect("source expectation")))
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conversion")
}

fn read_json(path: &Path) -> JsonValue {
    serde_json::from_slice(
        &fs::read(path)
            .unwrap_or_else(|error| panic!("cannot read fixture {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("invalid JSON fixture {}: {error}", path.display()))
}

fn read_zip_parts(bytes: Vec<u8>) -> BTreeMap<String, Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("open generated XLSX");
    let mut parts = BTreeMap::<String, Vec<u8>>::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read generated XLSX part");
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .expect("read generated part bytes");
        assert!(parts.insert(entry.name().to_owned(), bytes).is_none());
    }
    parts
}

fn write_zip_parts(parts: BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(&mut output);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    for (name, bytes) in parts {
        writer
            .start_file(name, options)
            .expect("write generated part header");
        writer
            .write_all(&bytes)
            .expect("write generated part bytes");
    }
    writer.finish().expect("finish generated XLSX archive");
    output.into_inner()
}

fn load_marksheet_source(source: &Map<String, JsonValue>, root: &Path) -> Workbook {
    let bytes = load_source_bytes(source, root);
    let parsed = parse(&bytes);
    assert!(
        !parsed.has_errors(),
        "fixture Marksheet source has diagnostics: {:?}",
        parsed.diagnostics
    );
    parsed
        .workbook
        .expect("valid Marksheet fixture must lower to workbook")
}

fn load_source_bytes(source: &Map<String, JsonValue>, root: &Path) -> Vec<u8> {
    let path = source["path"].as_str().expect("source path");
    fs::read(root.join(path)).expect("declared source bytes")
}

fn resolve_source_fixture(source: &Map<String, JsonValue>, root: &Path) -> PathBuf {
    root.join(
        source["fixture"]
            .as_str()
            .expect("XLSX source descriptor path"),
    )
}

fn csv_export_selection(selection: &Map<String, JsonValue>) -> CsvExportSelection {
    match (
        selection.get("table"),
        selection.get("sheet"),
        selection.get("range"),
    ) {
        (Some(table), None, None) => CsvExportSelection::Table {
            table: table_id(table.as_str().expect("table identifier")),
        },
        (None, Some(sheet), Some(range_value)) => CsvExportSelection::Range {
            sheet: sheet_id(sheet.as_str().expect("sheet identifier")),
            range: range(range_value.as_str().expect("A1 range")),
        },
        invalid => panic!("invalid CSV selection fixture {invalid:?}"),
    }
}

fn csv_import_selection(target: &Map<String, JsonValue>) -> CsvImportSelection {
    let sheet = sheet_id(target["sheet"].as_str().expect("target sheet"));
    let label = target["label"].as_str().expect("target label").to_owned();
    match (target.get("table"), target.get("range")) {
        (Some(table), None) => CsvImportSelection::Table {
            sheet,
            label,
            table: table_id(table.as_str().expect("target table")),
            anchor: coordinate(
                target
                    .get("anchor")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("A1"),
            ),
        },
        (None, Some(range_value)) => CsvImportSelection::Range {
            sheet,
            label,
            range: range(range_value.as_str().expect("target range")),
        },
        invalid => panic!("CSV import target must name exactly one table or range: {invalid:?}"),
    }
}

fn materialize_xlsx_descriptor(descriptor: &JsonValue) -> Vec<u8> {
    assert_eq!(descriptor["fixture"], "marksheet-xlsx-source@1");
    assert_eq!(descriptor["generated"], true);
    let labels = descriptor["sheets"].as_array().expect("descriptor sheets");
    let workbook = Workbook {
        sheets: labels
            .iter()
            .enumerate()
            .map(|(index, label)| Sheet {
                id: sheet_id(&descriptor_sheet_id(
                    label.as_str().expect("descriptor sheet label"),
                    index,
                )),
                label: label.as_str().expect("descriptor sheet label").to_owned(),
                items: vec![SheetItem::Block(block(
                    "A1",
                    vec![vec![Cell::new(Value::Text("Value".to_owned()))]],
                ))],
                origin: None,
            })
            .collect(),
        ..Workbook::default()
    };
    let base = export_xlsx(&workbook, ConversionLimits::default())
        .expect("descriptor base XLSX export")
        .value;
    let mut parts = read_zip_parts(base);
    let features = descriptor["features"]
        .as_array()
        .expect("descriptor features")
        .iter()
        .map(|feature| feature.as_str().expect("descriptor feature"))
        .collect::<Vec<_>>();
    if features.contains(&"chart") {
        attach_chart(&mut parts);
    }
    if features.contains(&"macro") {
        attach_macro(&mut parts);
    }
    let declared_bytes = descriptor["declared_uncompressed_bytes"].as_u64();
    for part in descriptor["parts"]
        .as_array()
        .expect("descriptor parts")
        .iter()
        .filter_map(JsonValue::as_str)
    {
        if part.starts_with("xl/worksheets/sheet")
            || part.starts_with("xl/charts/")
            || part.to_ascii_lowercase().contains("vbaproject")
        {
            continue;
        }
        if part == "xl/sharedStrings.xml" {
            // The descriptor intentionally contains no copied OOXML bytes.
            // Its declared size drives a deterministic synthetic member.
            parts.insert(
                part.to_owned(),
                vec![b'x'; usize::try_from(declared_bytes.unwrap_or(1)).expect("part size")],
            );
            continue;
        }
        panic!("descriptor part {part:?} has no deterministic fixture materializer");
    }
    write_zip_parts(parts)
}

fn attach_chart(parts: &mut BTreeMap<String, Vec<u8>>) {
    const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const PKG_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    let sheet = xml_part(parts, "xl/worksheets/sheet1.xml");
    let sheet = if sheet.contains("xmlns:r=") {
        sheet
    } else {
        sheet.replacen(
            "<worksheet xmlns=",
            &format!("<worksheet xmlns:r=\"{REL_NS}\" xmlns="),
            1,
        )
    };
    parts.insert(
        "xl/worksheets/sheet1.xml".to_owned(),
        append_xml_before_end(&sheet, "worksheet", "<drawing r:id=\"rIdChartDrawing\"/>"),
    );
    parts.insert(
        "xl/worksheets/_rels/sheet1.xml.rels".to_owned(),
        relationships_xml(
            PKG_REL_NS,
            "rIdChartDrawing",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing",
            "../drawings/drawing1.xml",
        ),
    );
    parts.insert(
        "xl/drawings/drawing1.xml".to_owned(),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"{REL_NS}\"><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>8</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>20</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id=\"2\" name=\"Chart 1\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm/><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart r:id=\"rIdChart\"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"
        )
        .into_bytes(),
    );
    parts.insert(
        "xl/drawings/_rels/drawing1.xml.rels".to_owned(),
        relationships_xml(
            PKG_REL_NS,
            "rIdChart",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart",
            "../charts/chart1.xml",
        ),
    );
    parts.insert(
        "xl/charts/chart1.xml".to_owned(),
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart><c:plotArea/><c:plotVisOnly val=\"1\"/><c:dispBlanksAs val=\"gap\"/></c:chart></c:chartSpace>".to_vec(),
    );
    add_content_type_override(
        parts,
        "/xl/drawings/drawing1.xml",
        "application/vnd.openxmlformats-officedocument.drawing+xml",
    );
    add_content_type_override(
        parts,
        "/xl/charts/chart1.xml",
        "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
    );
}

fn attach_macro(parts: &mut BTreeMap<String, Vec<u8>>) {
    let relationships = xml_part(parts, "xl/_rels/workbook.xml.rels");
    parts.insert(
        "xl/_rels/workbook.xml.rels".to_owned(),
        append_xml_before_end(
            &relationships,
            "Relationships",
            "<Relationship Id=\"rIdVba\" Type=\"http://schemas.microsoft.com/office/2006/relationships/vbaProject\" Target=\"vbaProject.bin\"/>",
        ),
    );
    // The OLE payload is opaque to the converter, but its compound-file
    // signature keeps the synthetic part recognizably macro-shaped.
    parts.insert(
        "xl/vbaProject.bin".to_owned(),
        vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
    );
    let content_types = xml_part(parts, "[Content_Types].xml").replace(
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
    );
    parts.insert("[Content_Types].xml".to_owned(), content_types.into_bytes());
    add_content_type_override(
        parts,
        "/xl/vbaProject.bin",
        "application/vnd.ms-office.vbaProject",
    );
}

fn xml_part(parts: &BTreeMap<String, Vec<u8>>, name: &str) -> String {
    String::from_utf8(parts.get(name).expect("generated XML part").clone())
        .expect("generated XML is UTF-8")
}

fn append_xml_before_end(xml: &str, element: &str, addition: &str) -> Vec<u8> {
    xml.replacen(
        &format!("</{element}>"),
        &format!("{addition}</{element}>"),
        1,
    )
    .into_bytes()
}

fn relationships_xml(namespace: &str, id: &str, kind: &str, target: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"{namespace}\"><Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{target}\"/></Relationships>"
    )
    .into_bytes()
}

fn add_content_type_override(
    parts: &mut BTreeMap<String, Vec<u8>>,
    part: &str,
    content_type: &str,
) {
    let content_types = xml_part(parts, "[Content_Types].xml");
    let override_xml = format!("<Override PartName=\"{part}\" ContentType=\"{content_type}\"/>");
    parts.insert(
        "[Content_Types].xml".to_owned(),
        append_xml_before_end(&content_types, "Types", &override_xml),
    );
}

fn assert_descriptor_inventory(bytes: &[u8], descriptor: &JsonValue) {
    let parts = read_zip_parts(bytes.to_vec());
    let names = parts.keys().cloned().collect::<Vec<_>>();
    for expected in descriptor["parts"].as_array().expect("descriptor parts") {
        assert!(
            names.contains(&expected.as_str().expect("descriptor part").to_owned()),
            "descriptor part {expected} was not materialized"
        );
    }
    for feature in descriptor["features"]
        .as_array()
        .expect("descriptor features")
    {
        let feature = feature.as_str().expect("descriptor feature");
        match feature {
            "cells" => assert!(names.iter().any(|name| name.starts_with("xl/worksheets/"))),
            "chart" => assert!(names.iter().any(|name| name.starts_with("xl/charts/"))),
            "macro" => assert!(
                names
                    .iter()
                    .any(|name| name.to_ascii_lowercase().contains("vbaproject"))
            ),
            "shared_strings" => assert!(names.contains(&"xl/sharedStrings.xml".to_owned())),
            unknown => panic!("descriptor feature {unknown:?} has no materializer assertion"),
        }
    }
    assert!(
        !descriptor_has_feature(descriptor, "chart")
            || descriptor_declares_part(descriptor, "xl/charts/chart1.xml"),
        "chart feature must declare its chart part"
    );
    assert!(
        !descriptor_has_feature(descriptor, "macro")
            || descriptor_declares_part(descriptor, "xl/vbaProject.bin"),
        "macro feature must declare its VBA part"
    );
    assert!(
        !descriptor_has_feature(descriptor, "shared_strings")
            || descriptor_declares_part(descriptor, "xl/sharedStrings.xml"),
        "shared_strings feature must declare its shared-string part"
    );
    if descriptor_has_feature(descriptor, "chart") {
        let types = xml_part(&parts, "[Content_Types].xml");
        assert!(types.contains("/xl/charts/chart1.xml"));
        assert!(types.contains("drawingml.chart+xml"));
        assert!(types.contains("/xl/drawings/drawing1.xml"));
        assert!(
            xml_part(&parts, "xl/worksheets/_rels/sheet1.xml.rels")
                .contains("../drawings/drawing1.xml")
        );
        assert!(
            xml_part(&parts, "xl/drawings/_rels/drawing1.xml.rels")
                .contains("../charts/chart1.xml")
        );
    }
    if descriptor_has_feature(descriptor, "macro") {
        let types = xml_part(&parts, "[Content_Types].xml");
        assert!(types.contains("sheet.macroEnabled.main+xml"));
        assert!(types.contains("/xl/vbaProject.bin"));
        assert!(types.contains("application/vnd.ms-office.vbaProject"));
        assert!(xml_part(&parts, "xl/_rels/workbook.xml.rels").contains("vbaProject.bin"));
    }
}

fn descriptor_declares_part(descriptor: &JsonValue, expected: &str) -> bool {
    descriptor["parts"]
        .as_array()
        .expect("descriptor parts")
        .iter()
        .any(|part| part == expected)
}

fn descriptor_has_feature(descriptor: &JsonValue, expected: &str) -> bool {
    descriptor["features"]
        .as_array()
        .expect("descriptor features")
        .iter()
        .any(|feature| feature == expected)
}

fn descriptor_sheet_id(label: &str, index: usize) -> String {
    let mut id = label
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                character
            } else if character.is_ascii_uppercase() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned();
    if id.is_empty() || !id.starts_with(|character: char| character.is_ascii_lowercase()) {
        id.insert_str(0, "sheet_");
    }
    if index > 0 {
        id.push('_');
        id.push_str(&index.to_string());
    }
    id
}

fn block(anchor: &str, rows: Vec<Vec<Cell>>) -> Block {
    Block::new(coordinate(anchor), rows).expect("rectangular fixture block")
}

fn coordinate(value: &str) -> Coordinate {
    Coordinate::parse(value).expect("fixture coordinate")
}

fn range(value: &str) -> Range {
    Range::parse(value).expect("fixture range")
}

fn sheet_id(value: &str) -> SheetId {
    SheetId::parse(value).expect("fixture sheet id")
}

fn table_id(value: &str) -> TableId {
    TableId::parse(value).expect("fixture table id")
}
