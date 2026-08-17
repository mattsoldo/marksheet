//! Canonical source serialization for source-less semantic workbooks.

use marksheet_calc::formula::{ParseLimits, format_formula, parse as parse_formula};
use marksheet_model::{
    ApplyTarget, Block, ByteSpan, Cell, ColumnGeometry, Coordinate, Diagnostic, Extension,
    ExtensionId, FillTarget, FormulaSource, HorizontalAlignment, LabeledSpan, NameTarget,
    NumberFormat, Range, RowGeometry, Severity, SheetItem, StyleProperties, TableRegion, Value,
    VerticalAlignment, Workbook, canonical_number,
};
use time::format_description::well_known::Rfc3339;

use crate::{ParseOptions, parse_with_options};

/// Serializes a semantic workbook to deterministic Draft 0.1 Marksheet source.
///
/// Unlike [`crate::canonicalize`], this entry point does not need a CST and is
/// therefore suitable for imported or programmatically constructed workbooks.
/// The complete semantic model is checked by reparsing the emitted source. A
/// malformed or unrepresentable manually constructed IR is rejected rather
/// than being silently normalized into a different workbook.
///
/// # Errors
///
/// Returns stable Marksheet diagnostics when the workbook contains invalid
/// coordinates, non-finite values, malformed formulas, invalid extension
/// payload boundaries, unresolved references, overlapping blocks, or another
/// semantic state that Draft 0.1 source cannot represent.
pub fn serialize_workbook(workbook: &Workbook) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let mut serializer = Serializer::default();
    serializer.write_workbook(workbook);
    if !serializer.diagnostics.is_empty() {
        return Err(serializer.diagnostics);
    }

    while serializer.output.ends_with("\n\n") {
        serializer.output.pop();
    }
    if !serializer.output.ends_with('\n') {
        serializer.output.push('\n');
    }
    let output = serializer.output.into_bytes();

    // Treat every capability present in this IR as host-supported while
    // checking source representability. Runtime capability policy belongs to
    // the host; it must not make serialization of a required declaration fail.
    let mut supported_extensions = workbook
        .extensions
        .iter()
        .map(|declaration| extension_id_text(&declaration.capability))
        .chain(
            workbook
                .extension_instances
                .iter()
                .map(|extension| extension_id_text(&extension.capability)),
        )
        .chain(workbook.sheets.iter().flat_map(|sheet| {
            sheet.items.iter().filter_map(|item| match item {
                SheetItem::Extension(extension) => Some(extension_id_text(&extension.capability)),
                _ => None,
            })
        }))
        .collect::<Vec<_>>();
    supported_extensions.sort();
    supported_extensions.dedup();
    let document = parse_with_options(
        &output,
        &ParseOptions {
            supported_extensions,
        },
    );
    let errors = document
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .cloned()
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(errors);
    }
    let Some(mut reparsed) = document.workbook else {
        return Err(vec![serialization_error(
            "MS2201",
            "serialized workbook did not produce a semantic model",
        )]);
    };

    let mut expected = workbook.clone();
    canonicalize_ir(&mut expected)?;
    strip_origins(&mut expected);
    strip_origins(&mut reparsed);
    if expected != reparsed {
        return Err(vec![serialization_error(
            "MS2201",
            "workbook cannot be represented as Draft 0.1 source without changing its semantics",
        )]);
    }

    Ok(output)
}

#[derive(Default)]
struct Serializer {
    output: String,
    diagnostics: Vec<Diagnostic>,
}

impl Serializer {
    fn write_workbook(&mut self, workbook: &Workbook) {
        self.output.push_str("#!marksheet 0.1\n");
        self.output.push_str("@book locale=");
        self.output
            .push_str(&json_string(&workbook.settings.locale));
        self.output.push_str(" timezone=");
        self.output
            .push_str(&json_string(&workbook.settings.timezone));
        self.output.push_str(" formula-profile=");
        self.output
            .push_str(&json_string(&workbook.settings.formula_profile));
        self.output.push('\n');

        for style in &workbook.styles {
            self.output.push_str("@style ");
            self.output.push_str(style.id.as_str());
            self.write_style_properties(&style.properties);
            self.output.push('\n');
        }
        for name in &workbook.names {
            self.output.push_str("@name ");
            self.output.push_str(name.id.as_str());
            self.output.push_str(" = ");
            let target = self.name_target(&name.target);
            self.output.push_str(&target);
            self.output.push('\n');
        }
        for declaration in &workbook.extensions {
            self.output.push_str(if declaration.required {
                "@require "
            } else {
                "@use "
            });
            let capability = self.extension_id(&declaration.capability);
            self.output.push_str(&capability);
            self.output.push('\n');
        }
        for extension in &workbook.extension_instances {
            self.write_extension(extension);
        }

        for sheet in &workbook.sheets {
            self.ensure_section_break();
            self.output.push_str("@sheet ");
            self.output.push_str(sheet.id.as_str());
            self.output.push(' ');
            self.output.push_str(&json_string(&sheet.label));
            self.output.push('\n');
            for item in &sheet.items {
                self.write_sheet_item(item);
            }
        }
    }

    fn ensure_section_break(&mut self) {
        while self.output.ends_with("\n\n") {
            self.output.pop();
        }
        if !self.output.is_empty() {
            if !self.output.ends_with('\n') {
                self.output.push('\n');
            }
            self.output.push('\n');
        }
    }

    fn write_style_properties(&mut self, properties: &StyleProperties) {
        if let Some(value) = properties.bold {
            self.property("bold", if value { "true" } else { "false" });
        }
        if let Some(value) = properties.italic {
            self.property("italic", if value { "true" } else { "false" });
        }
        if let Some(value) = properties.wrap {
            self.property("wrap", if value { "true" } else { "false" });
        }
        if let Some(value) = &properties.text_color {
            self.property("text-color", &json_string(value.as_str()));
        }
        if let Some(value) = &properties.fill {
            self.property("fill", &json_string(value.as_str()));
        }
        if let Some(value) = properties.font_size {
            let number = self.number(value, "style font size");
            self.property("font-size", &number);
        }
        if let Some(value) = properties.align {
            self.property(
                "align",
                match value {
                    HorizontalAlignment::Left => "left",
                    HorizontalAlignment::Center => "center",
                    HorizontalAlignment::Right => "right",
                    HorizontalAlignment::General => "general",
                },
            );
        }
        if let Some(value) = properties.valign {
            self.property(
                "valign",
                match value {
                    VerticalAlignment::Top => "top",
                    VerticalAlignment::Middle => "middle",
                    VerticalAlignment::Bottom => "bottom",
                },
            );
        }
        if let Some(value) = properties.number {
            self.property(
                "number",
                match value {
                    NumberFormat::General => "general",
                    NumberFormat::Integer => "integer",
                    NumberFormat::Decimal => "decimal",
                    NumberFormat::Percent => "percent",
                    NumberFormat::Currency => "currency",
                    NumberFormat::Date => "date",
                    NumberFormat::DateTime => "datetime",
                },
            );
        }
        if let Some(value) = properties.decimals {
            self.property("decimals", &value.to_string());
        }
        if let Some(value) = &properties.currency {
            self.property("currency", &json_string(value));
        }
    }

    fn property(&mut self, key: &str, value: &str) {
        self.output.push(' ');
        self.output.push_str(key);
        self.output.push('=');
        self.output.push_str(value);
    }

    fn write_sheet_item(&mut self, item: &SheetItem) {
        match item {
            SheetItem::Block(block) => self.write_block("block", None, block),
            SheetItem::Table(table) => {
                self.write_block("table", Some(table.id.as_str()), &table.block);
            }
            SheetItem::Fill(fill) => {
                self.output.push_str("@fill ");
                let target = self.fill_target(&fill.target);
                self.output.push_str(&target);
                self.output.push(' ');
                let formula = self.formula(&fill.formula, "fill formula");
                self.output.push_str(&formula);
                self.output.push('\n');
            }
            SheetItem::Apply(apply) => {
                self.output.push_str("@apply ");
                let target = self.apply_target(&apply.target);
                self.output.push_str(&target);
                for style in &apply.styles {
                    self.output.push(' ');
                    self.output.push_str(style.as_str());
                }
                self.output.push('\n');
            }
            SheetItem::ColumnGeometry(geometry) => self.write_column_geometry(geometry),
            SheetItem::RowGeometry(geometry) => self.write_row_geometry(geometry),
            SheetItem::Extension(extension) => self.write_extension(extension),
        }
    }

    fn write_block(&mut self, kind: &str, table: Option<&str>, block: &Block) {
        self.output.push('@');
        self.output.push_str(kind);
        self.output.push(' ');
        if let Some(table) = table {
            self.output.push_str(table);
            self.output.push(' ');
        }
        let anchor = self.coordinate(block.anchor, "block anchor");
        self.output.push_str(&anchor);
        self.output.push_str(" csv\n");

        let Some(width) = block.cells.first().map(Vec::len) else {
            self.diagnostics.push(serialization_error(
                "MS1204",
                "block requires at least one row and field",
            ));
            self.output.push_str("\n@end\n");
            return;
        };
        if width == 0 || block.cells.iter().any(|row| row.len() != width) {
            self.diagnostics.push(serialization_error(
                "MS1204",
                "block rows must have equal positive field counts",
            ));
        }
        for row in &block.cells {
            for (index, cell) in row.iter().enumerate() {
                if index != 0 {
                    self.output.push(',');
                }
                let scalar = self.value(cell);
                let force = row.len() == 1 && scalar == "@end";
                self.output.push_str(&csv_quote(&scalar, force));
            }
            self.output.push('\n');
        }
        self.output.push_str("@end\n");
    }

    fn value(&mut self, cell: &Cell) -> String {
        match &cell.value {
            Value::Blank => String::new(),
            Value::Text(text) => {
                if text.contains('\r') {
                    self.diagnostics.push(serialization_error(
                        "MS2201",
                        "cell text cannot contain a carriage return in canonical source",
                    ));
                }
                let safe_unforced = matches!(
                    Value::parse_strict(text),
                    Ok(Value::Text(ref parsed)) if parsed == text
                ) && !text.starts_with('\'');
                if safe_unforced {
                    text.clone()
                } else {
                    format!("'{text}")
                }
            }
            Value::Number(number) => self.number(*number, "cell number"),
            Value::Boolean(boolean) => boolean.to_string(),
            Value::Date(date) => {
                let year = date.year();
                if !(0..=9999).contains(&year) {
                    self.diagnostics.push(serialization_error(
                        "MS2201",
                        "cell date must have a four-digit non-negative year",
                    ));
                }
                format!(
                    "{year:04}-{month:02}-{day:02}",
                    month = u8::from(date.month()),
                    day = date.day()
                )
            }
            Value::DateTime(datetime) => datetime.format(&Rfc3339).unwrap_or_else(|error| {
                self.diagnostics.push(serialization_error(
                    "MS2201",
                    format!("cell datetime is not representable as RFC 3339: {error}"),
                ));
                "1970-01-01T00:00:00Z".to_owned()
            }),
            Value::Formula(formula) => self.formula(formula, "cell formula"),
            Value::Error(error) => error.to_string(),
        }
    }

    fn number(&mut self, value: f64, description: &str) -> String {
        canonical_number(value).unwrap_or_else(|error| {
            self.diagnostics.push(serialization_error(
                "MS2201",
                format!("{description} is invalid: {error}"),
            ));
            "0".to_owned()
        })
    }

    fn formula(&mut self, formula: &FormulaSource, description: &str) -> String {
        canonical_formula(formula).unwrap_or_else(|message| {
            self.diagnostics.push(serialization_error(
                "MS2202",
                format!("{description} is invalid: {message}"),
            ));
            "=0".to_owned()
        })
    }

    fn name_target(&mut self, target: &NameTarget) -> String {
        match target {
            NameTarget::Cell(target) => {
                let coordinate = self.coordinate(target.coordinate, "named cell coordinate");
                format!("{}!{coordinate}", target.sheet)
            }
            NameTarget::Range(target) => {
                let range = self.range(target.range, "named range");
                // Named cells and named ranges are intentionally distinct in
                // the semantic model even when a range has one cell.
                let range = if target.range.start == target.range.end {
                    format!("{range}:{range}")
                } else {
                    range
                };
                format!("{}!{range}", target.sheet)
            }
            NameTarget::TableColumn { table, header } => {
                self.structured_target(table.as_str(), header, "named table column")
            }
        }
    }

    fn fill_target(&mut self, target: &FillTarget) -> String {
        match target {
            FillTarget::Range(range) => self.range(*range, "fill range"),
            FillTarget::TableColumn { table, header } => {
                self.structured_target(table.as_str(), header, "fill table column")
            }
        }
    }

    fn apply_target(&mut self, target: &ApplyTarget) -> String {
        match target {
            ApplyTarget::Range(range) => self.range(*range, "style application range"),
            ApplyTarget::Table { table, region } => {
                let header = match region {
                    TableRegion::Headers => "#Headers",
                    TableRegion::Data => "#Data",
                    TableRegion::Column { header } => header,
                };
                self.structured_target(table.as_str(), header, "style application target")
            }
        }
    }

    fn structured_target(&mut self, id: &str, header: &str, description: &str) -> String {
        if header.is_empty() || header.contains(['\n', '\r']) {
            self.diagnostics.push(serialization_error(
                "MS2102",
                format!("{description} has an empty or multiline header"),
            ));
        }
        format!("{id}[{}]", header.replace(']', "]]"))
    }

    fn coordinate(&mut self, coordinate: Coordinate, description: &str) -> String {
        if coordinate.column == 0 || coordinate.row == 0 {
            self.diagnostics.push(serialization_error(
                "MS1202",
                format!("{description} must use positive one-based axes"),
            ));
            return "A1".to_owned();
        }
        coordinate.to_string()
    }

    fn range(&mut self, range: Range, description: &str) -> String {
        let start = self.coordinate(range.start, description);
        let end = self.coordinate(range.end, description);
        if range.start.column > range.end.column || range.start.row > range.end.row {
            self.diagnostics.push(serialization_error(
                "MS1202",
                format!("{description} must be normalized from top-left to bottom-right"),
            ));
        }
        if range.start == range.end {
            start
        } else {
            format!("{start}:{end}")
        }
    }

    fn write_column_geometry(&mut self, geometry: &ColumnGeometry) {
        let columns = geometry.columns;
        if columns.start == 0 || columns.end == 0 || columns.start > columns.end {
            self.diagnostics.push(serialization_error(
                "MS1202",
                "column geometry range must be normalized and one-based",
            ));
        }
        let start = Coordinate {
            column: columns.start.max(1),
            row: 1,
        }
        .column_name();
        let end = Coordinate {
            column: columns.end.max(1),
            row: 1,
        }
        .column_name();
        self.output.push_str("@column ");
        self.output.push_str(&start);
        if start != end {
            self.output.push(':');
            self.output.push_str(&end);
        }
        self.output.push_str(" width=");
        let width = self.number(geometry.width, "column width");
        self.output.push_str(&width);
        self.output.push('\n');
    }

    fn write_row_geometry(&mut self, geometry: &RowGeometry) {
        let rows = geometry.rows;
        if rows.start == 0 || rows.end == 0 || rows.start > rows.end {
            self.diagnostics.push(serialization_error(
                "MS1202",
                "row geometry range must be normalized and one-based",
            ));
        }
        self.output.push_str("@row ");
        self.output.push_str(&rows.start.max(1).to_string());
        if rows.start != rows.end {
            self.output.push(':');
            self.output.push_str(&rows.end.max(1).to_string());
        }
        self.output.push_str(" height=");
        let height = self.number(geometry.height, "row height");
        self.output.push_str(&height);
        self.output.push('\n');
    }

    fn extension_id(&mut self, capability: &ExtensionId) -> String {
        if capability.major == 0 {
            self.diagnostics.push(serialization_error(
                "MS1201",
                "extension major version must be positive",
            ));
        }
        extension_id_text(capability)
    }

    fn write_extension(&mut self, extension: &Extension) {
        self.output.push_str("@extension ");
        let capability = self.extension_id(&extension.capability);
        self.output.push_str(&capability);
        self.output.push(' ');
        self.output.push_str(&json_string(&extension.name));
        self.output.push('\n');
        match canonical_extension_payload(&extension.payload) {
            Ok(payload) => self.output.push_str(&payload),
            Err(message) => self
                .diagnostics
                .push(serialization_error("MS1101", message)),
        }
        self.output.push_str("@end\n");
    }
}

fn canonical_formula(formula: &FormulaSource) -> Result<String, String> {
    let parsed = parse_formula(formula.as_str(), &ParseLimits::default())
        .map_err(|error| error.to_string())?;
    format_formula(&parsed).map_err(|error| error.to_string())
}

fn canonical_extension_payload(payload: &str) -> Result<String, String> {
    if !payload.is_empty() && !payload.ends_with(['\n', '\r']) {
        return Err("non-empty extension payload must end at a physical line boundary".to_owned());
    }
    let mut normalized = String::with_capacity(payload.len());
    let mut characters = payload.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            characters.next_if_eq(&'\n');
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    if normalized.split_terminator('\n').any(|line| line == "@end") {
        return Err("extension payload contains an unescaped @end terminator line".to_owned());
    }
    Ok(normalized)
}

fn canonicalize_ir(workbook: &mut Workbook) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    for extension in &mut workbook.extension_instances {
        match canonical_extension_payload(&extension.payload) {
            Ok(payload) => extension.payload = payload,
            Err(message) => diagnostics.push(serialization_error("MS1101", message)),
        }
    }
    for sheet in &mut workbook.sheets {
        for item in &mut sheet.items {
            match item {
                SheetItem::Block(block) => {
                    canonicalize_block_formulas(block, &mut diagnostics);
                }
                SheetItem::Table(table) => {
                    canonicalize_block_formulas(&mut table.block, &mut diagnostics);
                }
                SheetItem::Fill(fill) => match canonical_formula(&fill.formula) {
                    Ok(formula) => {
                        fill.formula = FormulaSource::new(formula)
                            .expect("canonical formula retains its leading marker");
                    }
                    Err(message) => diagnostics.push(serialization_error("MS2202", message)),
                },
                SheetItem::Extension(extension) => {
                    match canonical_extension_payload(&extension.payload) {
                        Ok(payload) => extension.payload = payload,
                        Err(message) => diagnostics.push(serialization_error("MS1101", message)),
                    }
                }
                SheetItem::Apply(_) | SheetItem::ColumnGeometry(_) | SheetItem::RowGeometry(_) => {}
            }
        }
    }
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

fn canonicalize_block_formulas(block: &mut Block, diagnostics: &mut Vec<Diagnostic>) {
    for cell in block.cells.iter_mut().flatten() {
        let Value::Formula(formula) = &cell.value else {
            continue;
        };
        match canonical_formula(formula) {
            Ok(source) => {
                cell.value = Value::Formula(
                    FormulaSource::new(source)
                        .expect("canonical formula retains its leading marker"),
                );
            }
            Err(message) => diagnostics.push(serialization_error("MS2202", message)),
        }
    }
}

fn strip_origins(workbook: &mut Workbook) {
    workbook.origin = None;
    workbook.book_origin = None;
    for style in &mut workbook.styles {
        style.origin = None;
    }
    for name in &mut workbook.names {
        name.origin = None;
    }
    for declaration in &mut workbook.extensions {
        declaration.origin = None;
    }
    for extension in &mut workbook.extension_instances {
        extension.origin = None;
        extension.payload_origin = None;
    }
    for sheet in &mut workbook.sheets {
        sheet.origin = None;
        for item in &mut sheet.items {
            match item {
                SheetItem::Block(block) => strip_block_origins(block),
                SheetItem::Table(table) => {
                    table.origin = None;
                    strip_block_origins(&mut table.block);
                }
                SheetItem::Fill(fill) => fill.origin = None,
                SheetItem::Apply(apply) => apply.origin = None,
                SheetItem::ColumnGeometry(geometry) => geometry.origin = None,
                SheetItem::RowGeometry(geometry) => geometry.origin = None,
                SheetItem::Extension(extension) => {
                    extension.origin = None;
                    extension.payload_origin = None;
                }
            }
        }
    }
}

fn strip_block_origins(block: &mut Block) {
    block.origin = None;
    for cell in block.cells.iter_mut().flatten() {
        cell.origin = None;
    }
}

fn extension_id_text(capability: &ExtensionId) -> String {
    format!("{}@{}", capability.id, capability.major)
}

fn json_string(value: &str) -> String {
    // Serializing a borrowed UTF-8 string to JSON has no fallible data model
    // cases; retain a non-panicking boundary for source-less imported text.
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn csv_quote(value: &str, force: bool) -> String {
    if force
        || value
            .bytes()
            .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn serialization_error(code: &'static str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: marksheet_model::DiagnosticCode::new(code)
            .expect("serializer diagnostics use registered Marksheet codes"),
        severity: Severity::Error,
        message: message.into(),
        primary: LabeledSpan {
            span: ByteSpan::empty(0),
            label: None,
        },
        related: Vec::new(),
        context: None,
        suggestion: None,
    }
}

#[cfg(test)]
mod tests {
    use marksheet_model::{
        Apply, Block, Cell, Color, ColumnGeometry, ColumnRange, Coordinate, Extension,
        ExtensionDeclaration, ExtensionId, Fill, FormulaSource, Name, NameId, Range, RowGeometry,
        RowRange, Sheet, SheetCoordinate, SheetId, SheetItem, SheetRange, Style, StyleId,
        StyleProperties, Table, TableId, TableRegion, Value, Workbook, WorkbookSettings,
    };
    use time::{Date, Month, OffsetDateTime};

    use super::*;

    fn cell(value: Value) -> Cell {
        Cell::new(value)
    }

    fn coordinate(column: u64, row: u64) -> Coordinate {
        Coordinate::new(column, row).expect("valid test coordinate")
    }

    #[test]
    fn valid_conformance_fixtures_round_trip_from_semantic_ir() {
        for source in [
            include_bytes!("../../../tests/conformance/valid/all_core.ms").as_slice(),
            include_bytes!("../../../tests/conformance/valid/csv_edge_cases.ms").as_slice(),
            include_bytes!("../../../tests/conformance/valid/sparse_blocks.ms").as_slice(),
        ] {
            let options = ParseOptions {
                supported_extensions: vec!["archive@1".to_owned()],
            };
            let document = parse_with_options(source, &options);
            assert!(!document.has_errors(), "{:?}", document.diagnostics);
            let workbook = document.workbook.expect("fixture has semantic IR");

            let once = serialize_workbook(&workbook).expect("semantic IR serializes");
            let reparsed = parse_with_options(&once, &options);
            let twice = serialize_workbook(
                &reparsed
                    .workbook
                    .clone()
                    .expect("serialized source reparses"),
            )
            .expect("reparsed IR serializes");

            assert_eq!(once, twice);
            assert_eq!(crate::canonicalize(&reparsed).unwrap(), once);
            assert!(once.ends_with(b"\n"));
            assert!(!once.ends_with(b"\n\n"));
            assert!(!once.windows(2).any(|window| window == b"\r\n"));
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn imported_workbook_serializes_every_semantic_section() {
        let table_id = TableId::parse("sales").unwrap();
        let style_id = StyleId::parse("money").unwrap();
        let extension_id = ExtensionId::parse("archive@1").unwrap();
        let sheet_id = SheetId::parse("inputs").unwrap();
        let table = Table {
            id: table_id.clone(),
            block: Block::new(
                coordinate(1, 1),
                vec![
                    vec![
                        cell(Value::Text("Item".to_owned())),
                        cell(Value::Text("Amount".to_owned())),
                        cell(Value::Text("Result] net".to_owned())),
                    ],
                    vec![
                        cell(Value::Text("Widget".to_owned())),
                        cell(Value::Number(12.5)),
                        cell(Value::Blank),
                    ],
                ],
            )
            .unwrap(),
            origin: None,
        };
        let values = Block::new(
            coordinate(6, 1),
            vec![
                vec![
                    cell(Value::Blank),
                    cell(Value::Text(String::new())),
                    cell(Value::Boolean(true)),
                    cell(Value::Date(
                        Date::from_calendar_date(2026, Month::August, 16).unwrap(),
                    )),
                    cell(Value::DateTime(
                        OffsetDateTime::parse("2026-08-16T14:30:00Z", &Rfc3339).unwrap(),
                    )),
                    cell(Value::Error(marksheet_model::CellError::NotAvailable)),
                    cell(Value::Formula(
                        FormulaSource::new("= sum ( a1 , 1 )").unwrap(),
                    )),
                ],
                vec![
                    cell(Value::Blank),
                    cell(Value::Text("true".to_owned())),
                    cell(Value::Text("2026-99-99".to_owned())),
                    cell(Value::Text("comma, quote \" and\nline".to_owned())),
                    cell(Value::Number(-0.0)),
                    cell(Value::Boolean(false)),
                    cell(Value::Text("plain".to_owned())),
                ],
            ],
        )
        .unwrap();
        let one_field = Block::new(
            coordinate(1, 1),
            vec![vec![cell(Value::Text("@end".to_owned()))]],
        )
        .unwrap();
        let workbook = Workbook {
            settings: WorkbookSettings {
                locale: "en-GB".to_owned(),
                timezone: "Europe/London".to_owned(),
                formula_profile: "portable-a1@1".to_owned(),
            },
            styles: vec![Style {
                id: style_id.clone(),
                properties: StyleProperties {
                    bold: Some(true),
                    italic: Some(false),
                    wrap: Some(true),
                    text_color: Some(Color::parse("#112233").unwrap()),
                    fill: Some(Color::parse("#AABBCCDD").unwrap()),
                    font_size: Some(10.5),
                    align: Some(HorizontalAlignment::Right),
                    valign: Some(VerticalAlignment::Middle),
                    number: Some(NumberFormat::Currency),
                    decimals: Some(2),
                    currency: Some("GBP".to_owned()),
                },
                origin: None,
            }],
            names: vec![
                Name {
                    id: NameId::parse("first_item").unwrap(),
                    target: NameTarget::Cell(SheetCoordinate {
                        sheet: sheet_id.clone(),
                        coordinate: coordinate(1, 2),
                    }),
                    origin: None,
                },
                Name {
                    id: NameId::parse("input_range").unwrap(),
                    target: NameTarget::Range(SheetRange {
                        sheet: sheet_id.clone(),
                        range: Range::new(coordinate(1, 1), coordinate(2, 2)),
                    }),
                    origin: None,
                },
                Name {
                    id: NameId::parse("single_range").unwrap(),
                    target: NameTarget::Range(SheetRange {
                        sheet: sheet_id.clone(),
                        range: Range::single(coordinate(2, 2)),
                    }),
                    origin: None,
                },
                Name {
                    id: NameId::parse("amounts").unwrap(),
                    target: NameTarget::TableColumn {
                        table: table_id.clone(),
                        header: "Amount".to_owned(),
                    },
                    origin: None,
                },
            ],
            extensions: vec![ExtensionDeclaration {
                capability: extension_id.clone(),
                required: false,
                origin: None,
            }],
            extension_instances: vec![Extension {
                capability: extension_id.clone(),
                name: "workbook".to_owned(),
                payload: "owner=finance\r\nstable=true\r\n".to_owned(),
                origin: None,
                payload_origin: None,
            }],
            sheets: vec![
                Sheet {
                    id: sheet_id,
                    label: "Imported inputs".to_owned(),
                    items: vec![
                        SheetItem::Table(table),
                        SheetItem::Fill(Fill {
                            target: FillTarget::TableColumn {
                                table: table_id.clone(),
                                header: "Result] net".to_owned(),
                            },
                            formula: FormulaSource::new("=[@Amount] * 2").unwrap(),
                            origin: None,
                        }),
                        SheetItem::Apply(Apply {
                            target: ApplyTarget::Table {
                                table: table_id.clone(),
                                region: TableRegion::Headers,
                            },
                            styles: vec![style_id.clone()],
                            origin: None,
                        }),
                        SheetItem::Apply(Apply {
                            target: ApplyTarget::Table {
                                table: table_id.clone(),
                                region: TableRegion::Data,
                            },
                            styles: vec![style_id.clone()],
                            origin: None,
                        }),
                        SheetItem::Apply(Apply {
                            target: ApplyTarget::Table {
                                table: table_id,
                                region: TableRegion::Column {
                                    header: "Amount".to_owned(),
                                },
                            },
                            styles: vec![style_id.clone()],
                            origin: None,
                        }),
                        SheetItem::Block(values),
                        SheetItem::Fill(Fill {
                            target: FillTarget::Range(Range::single(coordinate(6, 2))),
                            formula: FormulaSource::new("=1").unwrap(),
                            origin: None,
                        }),
                        SheetItem::Apply(Apply {
                            target: ApplyTarget::Range(Range::new(
                                coordinate(6, 1),
                                coordinate(12, 1),
                            )),
                            styles: vec![style_id],
                            origin: None,
                        }),
                        SheetItem::ColumnGeometry(ColumnGeometry {
                            columns: ColumnRange::new(1, 4).unwrap(),
                            width: 12.5,
                            origin: None,
                        }),
                        SheetItem::RowGeometry(RowGeometry {
                            rows: RowRange::new(1, 2).unwrap(),
                            height: 18.0,
                            origin: None,
                        }),
                        SheetItem::Extension(Extension {
                            capability: extension_id,
                            name: "sheet".to_owned(),
                            payload: "source=xlsx\r\n".to_owned(),
                            origin: None,
                            payload_origin: None,
                        }),
                    ],
                    origin: None,
                },
                Sheet {
                    id: SheetId::parse("summary").unwrap(),
                    label: "Summary".to_owned(),
                    items: vec![SheetItem::Block(one_field)],
                    origin: None,
                },
            ],
            book_origin: None,
            origin: None,
        };

        let source = serialize_workbook(&workbook).expect("imported workbook serializes");
        let text = String::from_utf8(source.clone()).unwrap();
        assert!(text.contains("@book locale=\"en-GB\" timezone=\"Europe/London\""));
        assert!(text.contains("@fill sales[Result]] net] =[@Amount]*2\n"));
        assert!(text.contains("@name input_range = inputs!A1:B2\n"));
        assert!(text.contains("@name single_range = inputs!B2:B2\n"));
        assert!(text.contains("\"@end\"\n@end\n"));
        assert!(text.contains("owner=finance\nstable=true\n@end\n"));
        assert!(text.contains("=SUM(A1,1)"));
        assert!(!text.contains('\r'));
        assert!(
            !parse_with_options(
                &source,
                &ParseOptions {
                    supported_extensions: vec!["archive@1".to_owned()],
                }
            )
            .has_errors()
        );
    }

    #[test]
    fn malformed_manual_ir_returns_diagnostics_instead_of_panicking() {
        let mut workbook = Workbook::default();
        assert_eq!(
            serialize_workbook(&workbook).unwrap_err()[0].code.as_str(),
            "MS1101"
        );

        workbook.sheets.push(Sheet {
            id: SheetId::parse("data").unwrap(),
            label: "Data".to_owned(),
            items: vec![SheetItem::Block(
                Block::new(coordinate(1, 1), vec![vec![cell(Value::Number(f64::NAN))]]).unwrap(),
            )],
            origin: None,
        });
        assert_eq!(
            serialize_workbook(&workbook).unwrap_err()[0].code.as_str(),
            "MS2201"
        );

        workbook.sheets[0].items = vec![SheetItem::Apply(Apply {
            target: ApplyTarget::Range(Range {
                start: coordinate(2, 2),
                end: coordinate(1, 1),
            }),
            styles: Vec::new(),
            origin: None,
        })];
        assert_eq!(
            serialize_workbook(&workbook).unwrap_err()[0].code.as_str(),
            "MS1202"
        );

        workbook.sheets[0].items = vec![SheetItem::Extension(Extension {
            capability: ExtensionId::parse("opaque@1").unwrap(),
            name: "bad".to_owned(),
            payload: "not newline terminated".to_owned(),
            origin: None,
            payload_origin: None,
        })];
        assert_eq!(
            serialize_workbook(&workbook).unwrap_err()[0].code.as_str(),
            "MS1101"
        );
    }
}
