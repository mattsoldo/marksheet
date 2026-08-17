//! Differential conformance between the Rust parser and the independent
//! Python consumer's checked projection corpus.
//!
//! The projection intentionally uses only public `marksheet-syntax` and
//! `marksheet-model` data. It does not invoke the Python implementation; the
//! checked JSON files are the independent contract presented to this test.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Component, Path, PathBuf},
};

use marksheet_model::{
    ApplyTarget, ByteSpan, Cell, Extension, FillTarget, HorizontalAlignment, NameTarget,
    NumberFormat, Severity, SheetItem, StyleProperties, TableRegion, Value, VerticalAlignment,
    Workbook,
};
use marksheet_syntax::{ParseOptions, ParsedDocument, parse_with_options};
use serde_json::{Map, Value as JsonValue, json};

const SCHEMA: &str = "marksheet.conformance-projection@1";
const MANIFEST_SCHEMA: &str = "marksheet.conformance-projection-manifest@1";
const PROJECTION_DIRECTORY: &str = "tests/conformance/projections";

const CORPUS_ROOTS: &[&str] = &[
    "tests/conformance/valid",
    "tests/conformance/invalid",
    "tests/roundtrip",
    "tests/extensions",
    "tests/conversion/sources",
];

#[derive(Debug)]
struct ManifestEntry {
    id: String,
    source: String,
    projection: String,
}

#[derive(Debug)]
struct ProjectionManifest {
    available_extensions: Vec<String>,
    fixtures: Vec<ManifestEntry>,
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn object_with_exact_keys<'a>(
    value: &'a JsonValue,
    expected: &[&str],
    description: &str,
) -> &'a Map<String, JsonValue> {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("{description} must be a JSON object"));
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "{description} has unexpected fields");
    object
}

fn required_string(object: &Map<String, JsonValue>, key: &str, description: &str) -> String {
    object[key]
        .as_str()
        .unwrap_or_else(|| panic!("{description}.{key} must be a string"))
        .to_owned()
}

fn relative_path(value: &str, description: &str) -> PathBuf {
    let path = PathBuf::from(value);
    assert!(!value.is_empty(), "{description} must not be empty");
    assert!(!path.is_absolute(), "{description} must be relative");
    assert!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "{description} must contain only normal path components: {value:?}"
    );
    path
}

fn read_manifest(root: &Path) -> ProjectionManifest {
    let path = root.join(PROJECTION_DIRECTORY).join("manifest.json");
    let value: JsonValue = serde_json::from_slice(
        &fs::read(&path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("invalid JSON in {}: {error}", path.display()));
    let manifest = object_with_exact_keys(
        &value,
        &["schema", "available_extensions", "fixtures"],
        "projection manifest",
    );
    assert_eq!(
        manifest["schema"].as_str(),
        Some(MANIFEST_SCHEMA),
        "projection manifest schema must be exact"
    );

    let available_extensions = manifest["available_extensions"]
        .as_array()
        .expect("projection manifest available_extensions must be an array")
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("available_extensions[{index}] must be a string"))
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(
        available_extensions
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "available_extensions must be sorted and unique"
    );

    let fixtures = manifest["fixtures"]
        .as_array()
        .expect("projection manifest fixtures must be an array")
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let description = format!("projection manifest fixtures[{index}]");
            let object =
                object_with_exact_keys(value, &["id", "source", "projection"], &description);
            ManifestEntry {
                id: required_string(object, "id", &description),
                source: required_string(object, "source", &description),
                projection: required_string(object, "projection", &description),
            }
        })
        .collect::<Vec<_>>();
    ProjectionManifest {
        available_extensions,
        fixtures,
    }
}

fn discover_ms_files(root: &Path, directory: &Path, output: &mut BTreeSet<String>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| {
            panic!(
                "could not read corpus directory {}: {error}",
                directory.display()
            )
        })
        .map(|entry| entry.expect("corpus directory entry must be readable"))
        .collect::<Vec<_>>();
    entries.sort_by_key(fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("could not inspect {}: {error}", path.display()));
        assert!(
            !file_type.is_symlink(),
            "corpus entries must not be symlinks: {}",
            path.display()
        );
        if file_type.is_dir() {
            discover_ms_files(root, &path, output);
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "ms")
        {
            let relative = path
                .strip_prefix(root)
                .expect("corpus path remains below repository root")
                .to_string_lossy()
                .into_owned();
            assert!(
                output.insert(relative.clone()),
                "duplicate source {relative}"
            );
        }
    }
}

fn discovered_corpus(root: &Path) -> BTreeSet<String> {
    let mut sources = BTreeSet::new();
    for relative in CORPUS_ROOTS {
        let directory = root.join(relative);
        assert!(
            directory.is_dir(),
            "missing corpus root {}",
            directory.display()
        );
        discover_ms_files(root, &directory, &mut sources);
    }
    assert!(!sources.is_empty(), "projection corpus must not be empty");
    sources
}

fn discovered_projections(root: &Path) -> BTreeSet<String> {
    let directory = root.join(PROJECTION_DIRECTORY);
    fs::read_dir(&directory)
        .unwrap_or_else(|error| {
            panic!(
                "could not read projection directory {}: {error}",
                directory.display()
            )
        })
        .map(|entry| entry.expect("projection directory entry must be readable"))
        .filter_map(|entry| {
            let path = entry.path();
            (path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                && path.file_name().is_some_and(|name| name != "manifest.json"))
            .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

fn validate_manifest_bijection(root: &Path, manifest: &ProjectionManifest) {
    let mut ids = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut projections = BTreeSet::new();
    let mut ordered_sources = Vec::new();
    for fixture in &manifest.fixtures {
        let source_path = relative_path(&fixture.source, "fixture source");
        let projection_path = relative_path(&fixture.projection, "fixture projection");
        assert_eq!(
            projection_path.components().count(),
            1,
            "fixture projection must be a direct child of the projection directory"
        );
        assert_eq!(
            source_path.extension().and_then(|value| value.to_str()),
            Some("ms"),
            "fixture source must end in .ms"
        );
        assert_eq!(
            fixture.id,
            source_path.with_extension("").to_string_lossy(),
            "fixture ID must be its root-relative source path without .ms"
        );
        let expected_projection = format!(
            "{}.json",
            source_path
                .file_stem()
                .and_then(|value| value.to_str())
                .expect("fixture source must have a UTF-8 stem")
        );
        assert_eq!(
            fixture.projection, expected_projection,
            "fixture projection name must derive from the source stem"
        );
        assert!(
            ids.insert(fixture.id.clone()),
            "duplicate fixture ID {:?}",
            fixture.id
        );
        assert!(
            sources.insert(fixture.source.clone()),
            "duplicate fixture source {:?}",
            fixture.source
        );
        assert!(
            projections.insert(fixture.projection.clone()),
            "duplicate fixture projection {:?}",
            fixture.projection
        );
        ordered_sources.push(fixture.source.clone());
    }
    assert!(
        ordered_sources.windows(2).all(|pair| pair[0] < pair[1]),
        "manifest fixtures must be sorted by source and unique"
    );
    assert_eq!(
        sources,
        discovered_corpus(root),
        "manifest source set must be a bijection over every declared corpus .ms file"
    );
    assert_eq!(
        projections,
        discovered_projections(root),
        "manifest projection set must contain every and only checked projection JSON"
    );
}

fn source_slice(source: &[u8], span: ByteSpan) -> &[u8] {
    &source[usize::try_from(span.start).expect("source offset fits usize")
        ..usize::try_from(span.end).expect("source offset fits usize")]
}

fn source_text(source: &[u8], span: ByteSpan) -> &str {
    std::str::from_utf8(source_slice(source, span)).expect("valid fixture source is UTF-8")
}

fn span_json(span: ByteSpan) -> JsonValue {
    json!([span.start, span.end])
}

fn content_span(source: &[u8], line: ByteSpan) -> ByteSpan {
    let bytes = source_slice(source, line);
    let newline_len = if bytes.ends_with(b"\r\n") {
        2
    } else {
        u64::from(bytes.ends_with(b"\n") || bytes.ends_with(b"\r"))
    };
    ByteSpan {
        start: line.start,
        end: line.end - newline_len,
    }
}

fn required_origin(origin: Option<marksheet_model::Origin>, what: &str) -> ByteSpan {
    origin
        .unwrap_or_else(|| panic!("{what} parsed from source must retain an origin"))
        .span
}

fn extension_id(extension: &marksheet_model::ExtensionId) -> String {
    format!("{}@{}", extension.id, extension.major)
}

fn physical_lines(source: &[u8]) -> Vec<JsonValue> {
    let mut lines = Vec::new();
    let mut start = 0_usize;
    for (index, byte) in source.iter().copied().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let crlf = index > start && source[index - 1] == b'\r';
        let content_end = index - usize::from(crlf);
        lines.push(json!({
            "span": [start, index + 1],
            "content_span": [start, content_end],
            "ending": if crlf { "crlf" } else { "lf" },
        }));
        start = index + 1;
    }
    if start < source.len() {
        lines.push(json!({
            "span": [start, source.len()],
            "content_span": [start, source.len()],
            "ending": "none",
        }));
    } else if source.is_empty() {
        lines.push(json!({
            "span": [0, 0],
            "content_span": [0, 0],
            "ending": "none",
        }));
    }
    lines
}

fn authored_book_properties(source: &[u8], span: ByteSpan, workbook: &Workbook) -> JsonValue {
    let line = source_text(source, content_span(source, span));
    let mut properties = Map::new();
    for (key, value) in [
        ("locale", workbook.settings.locale.as_str()),
        ("timezone", workbook.settings.timezone.as_str()),
        (
            "formula-profile",
            workbook.settings.formula_profile.as_str(),
        ),
    ] {
        if line
            .split_ascii_whitespace()
            .any(|token| token.starts_with(&format!("{key}=")))
        {
            properties.insert(key.to_owned(), json!(value));
        }
    }
    JsonValue::Object(properties)
}

fn style_properties(properties: &StyleProperties) -> JsonValue {
    let mut result = Map::new();
    if let Some(value) = properties.bold {
        result.insert("bold".to_owned(), json!(value));
    }
    if let Some(value) = properties.italic {
        result.insert("italic".to_owned(), json!(value));
    }
    if let Some(value) = properties.wrap {
        result.insert("wrap".to_owned(), json!(value));
    }
    if let Some(value) = &properties.text_color {
        result.insert("text-color".to_owned(), json!(value.as_str()));
    }
    if let Some(value) = &properties.fill {
        result.insert("fill".to_owned(), json!(value.as_str()));
    }
    if let Some(value) = properties.font_size {
        result.insert("font-size".to_owned(), json!(value));
    }
    if let Some(value) = properties.align {
        let value = match value {
            HorizontalAlignment::Left => "left",
            HorizontalAlignment::Center => "center",
            HorizontalAlignment::Right => "right",
            HorizontalAlignment::General => "general",
        };
        result.insert("align".to_owned(), json!(value));
    }
    if let Some(value) = properties.valign {
        let value = match value {
            VerticalAlignment::Top => "top",
            VerticalAlignment::Middle => "middle",
            VerticalAlignment::Bottom => "bottom",
        };
        result.insert("valign".to_owned(), json!(value));
    }
    if let Some(value) = properties.number {
        let value = match value {
            NumberFormat::General => "general",
            NumberFormat::Integer => "integer",
            NumberFormat::Decimal => "decimal",
            NumberFormat::Percent => "percent",
            NumberFormat::Currency => "currency",
            NumberFormat::Date => "date",
            NumberFormat::DateTime => "datetime",
        };
        result.insert("number".to_owned(), json!(value));
    }
    if let Some(value) = properties.decimals {
        // The independent parser represents directive numbers as JSON floats.
        result.insert("decimals".to_owned(), json!(f64::from(value)));
    }
    if let Some(value) = &properties.currency {
        result.insert("currency".to_owned(), json!(value));
    }
    JsonValue::Object(result)
}

fn name_target(source: &[u8], document: &ParsedDocument, name: &marksheet_model::Name) -> String {
    let location = document
        .source_map
        .name(&name.id)
        .unwrap_or_else(|| panic!("name {} must have a source-map entry", name.id));
    let target = location
        .target
        .unwrap_or_else(|| panic!("name {} must have a target span", name.id));
    source_text(source, target).to_owned()
}

fn target_text(target: &ApplyTarget) -> String {
    match target {
        ApplyTarget::Range(range) => {
            if range.start == range.end {
                range.start.to_string()
            } else {
                format!("{}:{}", range.start, range.end)
            }
        }
        ApplyTarget::Table { table, region } => match region {
            TableRegion::Headers => format!("{table}[#Headers]"),
            TableRegion::Data => format!("{table}[#Data]"),
            TableRegion::Column { header } => format!("{table}[{header}]"),
        },
    }
}

fn fill_target_text(target: &FillTarget) -> String {
    match target {
        FillTarget::Range(range) => {
            if range.start == range.end {
                range.start.to_string()
            } else {
                format!("{}:{}", range.start, range.end)
            }
        }
        FillTarget::TableColumn { table, header } => format!("{table}[{header}]"),
    }
}

fn decoded_csv_fields(document: &ParsedDocument) -> BTreeMap<ByteSpan, String> {
    let mut fields = BTreeMap::new();
    for node in &document.cst.nodes {
        let marksheet_syntax::cst::Node::CsvBlock(block) = node else {
            continue;
        };
        for record in &block.records {
            for field in &record.fields {
                fields.insert(
                    ByteSpan {
                        start: field.span.start as u64,
                        end: field.span.end as u64,
                    },
                    field.decoded.clone(),
                );
            }
        }
    }
    fields
}

fn cell_value(cell: &Cell, decoded: &str) -> JsonValue {
    match &cell.value {
        Value::Blank => json!({ "kind": "blank" }),
        Value::Text(value) => json!({ "kind": "text", "value": value }),
        Value::Number(_) => json!({ "kind": "number", "value": decoded }),
        Value::Boolean(value) => json!({ "kind": "boolean", "value": value }),
        Value::Date(_) => json!({ "kind": "date", "value": decoded }),
        Value::DateTime(_) => json!({ "kind": "datetime", "value": decoded }),
        Value::Formula(value) => json!({ "kind": "formula", "value": value.as_str() }),
        Value::Error(value) => json!({ "kind": "error", "value": value.token() }),
    }
}

fn cells_projection(
    source: &[u8],
    fields: &BTreeMap<ByteSpan, String>,
    rows: &[Vec<Cell>],
) -> JsonValue {
    JsonValue::Array(
        rows.iter()
            .map(|row| {
                JsonValue::Array(
                    row.iter()
                        .map(|cell| {
                            let span = required_origin(cell.origin, "CSV cell");
                            let decoded = fields.get(&span).unwrap_or_else(|| {
                                panic!("CSV cell span {span:?} must exist in the CST")
                            });
                            json!({
                                "source": {
                                    "span": span_json(span),
                                    "raw": source_text(source, span),
                                },
                                "value": cell_value(cell, decoded),
                            })
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

fn csv_item(
    source: &[u8],
    document: &ParsedDocument,
    fields: &BTreeMap<ByteSpan, String>,
    kind: &str,
    id: Option<&str>,
    block: &marksheet_model::Block,
) -> JsonValue {
    let origin = required_origin(block.origin, kind);
    let location = document
        .source_map
        .csv_blocks()
        .iter()
        .find(|candidate| candidate.span == origin)
        .unwrap_or_else(|| panic!("{kind} origin {origin:?} must have a source-map entry"));
    let mut result = Map::new();
    result.insert("kind".to_owned(), json!(kind));
    if let Some(id) = id {
        result.insert("id".to_owned(), json!(id));
    }
    result.insert(
        "anchor".to_owned(),
        json!({ "column": block.anchor.column, "row": block.anchor.row }),
    );
    result.insert(
        "span".to_owned(),
        span_json(content_span(source, location.directive.line)),
    );
    result.insert("body_span".to_owned(), span_json(location.body));
    result.insert(
        "rows".to_owned(),
        cells_projection(source, fields, &block.cells),
    );
    JsonValue::Object(result)
}

fn line_value_after_equals(source: &[u8], origin: ByteSpan) -> String {
    source_text(source, content_span(source, origin))
        .rsplit_once('=')
        .expect("geometry directive must contain '='")
        .1
        .to_owned()
}

fn sheet_items(
    source: &[u8],
    document: &ParsedDocument,
    fields: &BTreeMap<ByteSpan, String>,
    items: &[SheetItem],
) -> JsonValue {
    let mut projected = Vec::new();
    for item in items {
        match item {
            SheetItem::Block(block) => {
                projected.push(csv_item(source, document, fields, "block", None, block));
            }
            SheetItem::Table(table) => projected.push(csv_item(
                source,
                document,
                fields,
                "table",
                Some(table.id.as_str()),
                &table.block,
            )),
            SheetItem::Fill(fill) => {
                let origin = required_origin(fill.origin, "fill");
                let location = document
                    .source_map
                    .fills()
                    .iter()
                    .find(|candidate| candidate.directive.line == origin)
                    .expect("fill must have a source-map entry");
                let target = location.target.map_or_else(
                    || fill_target_text(&fill.target),
                    |span| source_text(source, span).to_owned(),
                );
                projected.push(json!({
                    "kind": "fill",
                    "target": target,
                    "formula": fill.formula.as_str(),
                    "span": span_json(content_span(source, origin)),
                }));
            }
            SheetItem::Apply(apply) => {
                let origin = required_origin(apply.origin, "apply");
                let location = document
                    .source_map
                    .applies()
                    .iter()
                    .find(|candidate| candidate.directive.line == origin)
                    .expect("apply must have a source-map entry");
                let target = location.target.map_or_else(
                    || target_text(&apply.target),
                    |span| source_text(source, span).to_owned(),
                );
                projected.push(json!({
                    "kind": "apply",
                    "target": target,
                    "styles": apply.styles.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "span": span_json(content_span(source, origin)),
                }));
            }
            SheetItem::ColumnGeometry(geometry) => {
                let origin = required_origin(geometry.origin, "column geometry");
                projected.push(json!({
                    "kind": "column",
                    "range": { "start": geometry.columns.start, "end": geometry.columns.end },
                    "value": line_value_after_equals(source, origin),
                    "span": span_json(content_span(source, origin)),
                }));
            }
            SheetItem::RowGeometry(geometry) => {
                let origin = required_origin(geometry.origin, "row geometry");
                projected.push(json!({
                    "kind": "row",
                    "range": { "start": geometry.rows.start, "end": geometry.rows.end },
                    "value": line_value_after_equals(source, origin),
                    "span": span_json(content_span(source, origin)),
                }));
            }
            // The independent schema aggregates opaque instances into the
            // workbook-level `extensions` array while recording their scope.
            SheetItem::Extension(_) => {}
        }
    }
    JsonValue::Array(projected)
}

fn extension_projection(
    source: &[u8],
    document: &ParsedDocument,
    extension: &Extension,
    scope: &str,
) -> JsonValue {
    let origin = required_origin(extension.origin, "extension");
    let payload = required_origin(extension.payload_origin, "extension payload");
    let location = document
        .source_map
        .extensions()
        .iter()
        .find(|candidate| candidate.span == origin)
        .expect("extension must have a source-map entry");
    json!({
        "id": extension_id(&extension.capability),
        "name": extension.name,
        "scope": scope,
        "span": span_json(content_span(source, location.directive.line)),
        "payload": {
            "span": span_json(payload),
            "byte_length": payload.len(),
            "sha256": sha256_hex(source_slice(source, payload)),
        },
    })
}

fn workbook_projection(source: &[u8], document: &ParsedDocument, workbook: &Workbook) -> JsonValue {
    let fields = decoded_csv_fields(document);
    let book = workbook.book_origin.map_or(JsonValue::Null, |origin| {
        json!({
            "span": span_json(content_span(source, origin.span)),
            "properties": authored_book_properties(source, origin.span, workbook),
        })
    });

    let styles = workbook
        .styles
        .iter()
        .map(|style| {
            let span = content_span(source, required_origin(style.origin, "style"));
            json!({
                "id": style.id.as_str(),
                "properties": style_properties(&style.properties),
                "span": span_json(span),
            })
        })
        .collect::<Vec<_>>();

    let names = workbook
        .names
        .iter()
        .map(|name| {
            // Accessing the semantic variant makes an unresolved/unsupported
            // target impossible to silently enter the reference projection.
            match &name.target {
                NameTarget::Cell(_) | NameTarget::Range(_) | NameTarget::TableColumn { .. } => {}
            }
            json!({
                "id": name.id.as_str(),
                "target": name_target(source, document, name),
                "span": span_json(content_span(source, required_origin(name.origin, "name"))),
            })
        })
        .collect::<Vec<_>>();

    let capabilities = workbook
        .extensions
        .iter()
        .map(|declaration| {
            json!({
                "id": extension_id(&declaration.capability),
                "required": declaration.required,
                "span": span_json(content_span(source, required_origin(declaration.origin, "extension declaration"))),
            })
        })
        .collect::<Vec<_>>();

    let mut extensions = workbook
        .extension_instances
        .iter()
        .map(|extension| extension_projection(source, document, extension, "workbook"))
        .collect::<Vec<_>>();
    for sheet in &workbook.sheets {
        extensions.extend(sheet.items.iter().filter_map(|item| {
            let SheetItem::Extension(extension) = item else {
                return None;
            };
            Some(extension_projection(
                source,
                document,
                extension,
                sheet.id.as_str(),
            ))
        }));
    }
    extensions.sort_by_key(|extension| extension["span"][0].as_u64());

    let sheets = workbook
        .sheets
        .iter()
        .map(|sheet| {
            json!({
                "id": sheet.id.as_str(),
                "label": sheet.label,
                "span": span_json(content_span(source, required_origin(sheet.origin, "sheet"))),
                "items": sheet_items(source, document, &fields, &sheet.items),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "settings": {
            "locale": workbook.settings.locale,
            "timezone": workbook.settings.timezone,
            "formula_profile": workbook.settings.formula_profile,
        },
        "book": book,
        "styles": styles,
        "names": names,
        "capabilities": capabilities,
        "extensions": extensions,
        "sheets": sheets,
    })
}

fn line_content_at(source: &[u8], offset: u64) -> ByteSpan {
    let offset = usize::try_from(offset)
        .unwrap_or(source.len())
        .min(source.len());
    let start = source[..offset]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let newline = source[offset..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(source.len(), |index| offset + index);
    let end = if newline > start && source[newline.saturating_sub(1)] == b'\r' {
        newline - 1
    } else {
        newline
    };
    ByteSpan {
        start: start as u64,
        end: end as u64,
    }
}

fn duplicate_version_span(source: &[u8]) -> Option<ByteSpan> {
    let first_line = line_content_at(source, 0);
    let mut offset = usize::try_from(first_line.end).ok()?;
    if source.get(offset) == Some(&b'\r') {
        offset += 1;
    }
    if source.get(offset) == Some(&b'\n') {
        offset += 1;
    }
    while offset < source.len() {
        let line = line_content_at(source, offset as u64);
        if source_slice(source, line).starts_with(b"#!marksheet") {
            return Some(line);
        }
        let next = usize::try_from(line.end).ok()?;
        offset = next
            + usize::from(source.get(next) == Some(&b'\r'))
            + usize::from(
                source.get(next + usize::from(source.get(next) == Some(&b'\r'))) == Some(&b'\n'),
            );
        if offset <= next {
            break;
        }
    }
    None
}

fn diagnostic_span(source: &[u8], document: &ParsedDocument, index: usize) -> ByteSpan {
    let diagnostic = &document.diagnostics[index];
    let primary = diagnostic.primary.span;
    if diagnostic.code.as_str() == "MS1001"
        && diagnostic.message.contains("exactly one version header")
    {
        if let Some(span) = duplicate_version_span(source) {
            return span;
        }
    }
    match diagnostic.code.as_str() {
        "MS3101" | "MS3102" => document
            .workbook
            .as_ref()
            .and_then(|workbook| {
                workbook.extensions.iter().find_map(|declaration| {
                    let line = required_origin(declaration.origin, "extension declaration");
                    line.contains_span(primary)
                        .then(|| content_span(source, line))
                })
            })
            .unwrap_or(primary),
        "MS3103" => document
            .source_map
            .extensions()
            .iter()
            .find(|extension| extension.directive.line.contains_span(primary))
            .map_or(primary, |extension| {
                content_span(source, extension.directive.line)
            }),
        _ if diagnostic.code.as_str() == "MS2201"
            && diagnostic.message.starts_with("invalid ISO") =>
        {
            primary
        }
        _ => document
            .source_map
            .csv_blocks()
            .iter()
            .find(|block| block.span.contains_span(primary))
            .map_or_else(
                || line_content_at(source, primary.start),
                |block| content_span(source, block.directive.line),
            ),
    }
}

fn diagnostics_projection(source: &[u8], document: &ParsedDocument) -> Vec<JsonValue> {
    let mut diagnostics = document
        .diagnostics
        .iter()
        .enumerate()
        .map(|(index, diagnostic)| {
            let span = diagnostic_span(source, document, index);
            let severity = match diagnostic.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
            };
            json!({
                "code": diagnostic.code.as_str(),
                "severity": severity,
                "span": span_json(span),
            })
        })
        .collect::<Vec<_>>();
    if let Some(workbook) = &document.workbook {
        let declared = workbook
            .extensions
            .iter()
            .map(|declaration| extension_id(&declaration.capability))
            .collect::<BTreeSet<_>>();
        let instances = workbook.extension_instances.iter().chain(
            workbook
                .sheets
                .iter()
                .flat_map(|sheet| sheet.items.iter())
                .filter_map(|item| {
                    let SheetItem::Extension(extension) = item else {
                        return None;
                    };
                    Some(extension)
                }),
        );
        for extension in instances {
            if declared.contains(&extension_id(&extension.capability)) {
                continue;
            }
            let origin = required_origin(extension.origin, "undeclared extension");
            let span = document
                .source_map
                .extensions()
                .iter()
                .find(|location| location.span == origin)
                .map_or(origin, |location| {
                    content_span(source, location.directive.line)
                });
            diagnostics.push(json!({
                "code": "MS3103",
                "severity": "warning",
                "span": span_json(span),
            }));
        }
    }
    diagnostics.sort_by(|left, right| {
        let key = |value: &JsonValue| {
            (
                value["span"][0].as_u64().unwrap_or_default(),
                value["span"][1].as_u64().unwrap_or_default(),
                value["code"].as_str().unwrap_or_default().to_owned(),
                value["severity"].as_str().unwrap_or_default().to_owned(),
            )
        };
        key(left).cmp(&key(right))
    });
    diagnostics.dedup();
    let mut emitted_duplicate_declaration = false;
    diagnostics.retain(|diagnostic| {
        if diagnostic["code"] != "MS1301" {
            return true;
        }
        let keep = !emitted_duplicate_declaration;
        emitted_duplicate_declaration = true;
        keep
    });
    diagnostics
}

fn reference_projection(source: &[u8], available_extensions: &[String]) -> JsonValue {
    let document = parse_with_options(
        source,
        &ParseOptions {
            supported_extensions: available_extensions.to_vec(),
        },
    );
    let workbook = document
        .workbook
        .as_ref()
        .expect("checked valid fixture must lower to a workbook");
    let diagnostics = diagnostics_projection(source, &document);
    let required_unavailable = diagnostics
        .iter()
        .any(|diagnostic| diagnostic["code"] == "MS3101" && diagnostic["severity"] == "error");
    json!({
        "schema": SCHEMA,
        "source": {
            "byte_length": source.len(),
            "sha256": sha256_hex(source),
            "physical_lines": physical_lines(source),
        },
        "workbook": workbook_projection(source, &document, workbook),
        "diagnostics": diagnostics,
        "completeness": {
            "calculation_complete": !required_unavailable,
            "rendering_complete": !required_unavailable,
        },
    })
}

fn first_json_difference(actual: &JsonValue, expected: &JsonValue, path: &str) -> Option<String> {
    match (actual, expected) {
        (JsonValue::Object(actual), JsonValue::Object(expected)) => {
            let keys = actual
                .keys()
                .chain(expected.keys())
                .collect::<BTreeSet<_>>();
            for key in keys {
                let nested = format!("{path}/{key}");
                match (actual.get(key), expected.get(key)) {
                    (Some(actual), Some(expected)) => {
                        if let Some(difference) = first_json_difference(actual, expected, &nested) {
                            return Some(difference);
                        }
                    }
                    (Some(actual), None) => {
                        return Some(format!("{nested}: unexpected {actual}"));
                    }
                    (None, Some(expected)) => {
                        return Some(format!("{nested}: missing expected {expected}"));
                    }
                    (None, None) => unreachable!("key came from at least one object"),
                }
            }
            None
        }
        (JsonValue::Array(actual), JsonValue::Array(expected)) => {
            if actual.len() != expected.len() {
                return Some(format!(
                    "{path}: array length {} != expected {}",
                    actual.len(),
                    expected.len()
                ));
            }
            actual
                .iter()
                .zip(expected)
                .enumerate()
                .find_map(|(index, (actual, expected))| {
                    first_json_difference(actual, expected, &format!("{path}/{index}"))
                })
        }
        _ if actual == expected => None,
        _ => Some(format!("{path}: {actual} != expected {expected}")),
    }
}

#[test]
fn manifest_is_a_bijection_over_every_checked_source_and_projection() {
    let root = repository_root();
    let manifest = read_manifest(&root);
    validate_manifest_bijection(&root, &manifest);
}

#[test]
fn rust_reference_projection_matches_every_manifest_fixture_exactly() {
    let root = repository_root();
    let manifest = read_manifest(&root);
    validate_manifest_bijection(&root, &manifest);
    let mut failures = Vec::new();
    for fixture in &manifest.fixtures {
        let source_path = root.join(&fixture.source);
        let expected_path = root.join(PROJECTION_DIRECTORY).join(&fixture.projection);
        let source = fs::read(&source_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", source_path.display()));
        let expected: JsonValue =
            serde_json::from_slice(&fs::read(&expected_path).unwrap_or_else(|error| {
                panic!("could not read {}: {error}", expected_path.display())
            }))
            .unwrap_or_else(|error| panic!("invalid JSON in {}: {error}", expected_path.display()));
        let actual = reference_projection(&source, &manifest.available_extensions);
        if actual != expected {
            failures.push(format!(
                "{}: {}",
                fixture.id,
                first_json_difference(&actual, &expected, "$")
                    .expect("unequal JSON values have a first difference")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Rust projection diverged from independent projections:\n{}",
        failures.join("\n")
    );
}

// A compact, dependency-free SHA-256 implementation keeps this test portable
// while checking the schema's byte-identity claims. The known-answer test
// guards against the reference and fixture accidentally agreeing on bad input.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut hash = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().expect("four-byte word"));
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for index in 0..64 {
            let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(big_s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = big_s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (target, value) in hash.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *target = target.wrapping_add(value);
        }
    }

    let mut output = String::with_capacity(64);
    for word in hash {
        write!(output, "{word:08x}").expect("writing to String cannot fail");
    }
    output
}

#[test]
fn reference_sha256_has_known_answer() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
