#![allow(clippy::too_many_lines)] // The streaming CSV state machine is clearer kept contiguous.

use std::collections::BTreeSet;

use marksheet_calc::{
    PreparedWorkbook,
    formula::{FormulaTemplate, ParseLimits, format_formula, parse},
    prepare::{PrepareLimits, PreparedSheet},
};
use marksheet_model::{
    Block, Cell, Coordinate, FormulaSource, Range, Sheet, SheetId, SheetItem, Table, TableId,
    Value, Workbook, canonical_number,
};
use time::format_description::well_known::Rfc3339;

use crate::{
    Conversion, ConversionEvent, ConversionFailure, ConversionFeature, ConversionLimits,
    ConversionLocation, ConversionReport, ConversionResult, ConvertError, ConvertErrorCode,
    FormatDescriptor, FormulaDisposition, FormulaEvent,
    formula_profile::validate_formula_expression,
};

/// CSV export cannot infer a used range. Callers must name one finite source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CsvExportSelection {
    Range { sheet: SheetId, range: Range },
    Table { table: TableId },
}

/// CSV import always creates one sheet and one explicitly shaped item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CsvImportSelection {
    /// The CSV dimensions must exactly match this destination range.
    Range {
        sheet: SheetId,
        label: String,
        range: Range,
    },
    /// The first CSV row is the table header.
    Table {
        sheet: SheetId,
        label: String,
        table: TableId,
        anchor: Coordinate,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CsvState {
    Start,
    Unquoted,
    Quoted,
    AfterQuote,
}

/// Exports exactly the requested table or rectangle using Marksheet scalar
/// spellings and RFC 4180 quoting. Output always uses LF and a terminal LF.
///
/// # Errors
///
/// Returns an error for an unresolved selection, invalid workbook, malformed
/// formula fill, or resource limit. No partial byte vector is returned.
pub fn export_csv(
    workbook: &Workbook,
    selection: &CsvExportSelection,
    limits: ConversionLimits,
) -> ConversionResult<Vec<u8>> {
    export_csv_inner(workbook, selection, limits).map_err(|error| {
        let feature = if error.code == ConvertErrorCode::InvalidSelection {
            "csv_selection"
        } else {
            "source_workbook"
        };
        ConversionFailure::new(
            error,
            FormatDescriptor::marksheet_ir(),
            FormatDescriptor::csv(),
            feature,
        )
    })
}

fn export_csv_inner(
    workbook: &Workbook,
    selection: &CsvExportSelection,
    limits: ConversionLimits,
) -> Result<Conversion<Vec<u8>>, ConvertError> {
    if workbook.sheets.len() > limits.max_sheets {
        return Err(resource("sheet count exceeds the configured limit"));
    }
    let prepared = PreparedWorkbook::build(
        workbook,
        PrepareLimits {
            max_range_cells: limits.max_cells,
            max_virtual_cells: limits.max_cells,
        },
    )
    .map_err(|error| invalid_workbook(format!("workbook cannot be projected: {error}")))?;

    let (sheet, range, table_selection) = match selection {
        CsvExportSelection::Range { sheet, range } => (
            prepared.sheet(sheet).ok_or_else(|| {
                ConvertError::new(
                    ConvertErrorCode::InvalidSelection,
                    format!("unknown sheet {sheet}"),
                )
            })?,
            *range,
            None,
        ),
        CsvExportSelection::Table { table } => {
            let table_index = prepared.table(table).ok_or_else(|| {
                ConvertError::new(
                    ConvertErrorCode::InvalidSelection,
                    format!("unknown table {table}"),
                )
            })?;
            let owner = prepared
                .sheet(&table_index.sheet)
                .ok_or_else(|| invalid_workbook("prepared table has no owning sheet".to_owned()))?;
            (owner, table_index.footprint, Some(table.clone()))
        }
    };

    let width = range
        .width()
        .map_err(|error| invalid_selection(error.to_string()))?;
    let height = range
        .height()
        .map_err(|error| invalid_selection(error.to_string()))?;
    let area = width
        .checked_mul(height)
        .ok_or_else(|| resource("CSV selection area overflows u64"))?;
    if area > limits.max_cells {
        return Err(resource("CSV selection exceeds the configured cell limit"));
    }
    if matches!(selection, CsvExportSelection::Range { .. })
        && !range_has_selected_cell(sheet, range)
    {
        return Err(invalid_selection(format!(
            "selected range {range} contains no authored or fill-derived cell"
        )));
    }

    let mut report =
        ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::csv());
    match selection {
        CsvExportSelection::Range { sheet, range } => report.exact_event(
            ConversionEvent::new(
                ConversionFeature::Other("selected_range".to_owned()),
                "the explicitly selected rectangle was exported",
            )
            .at(ConversionLocation::range(sheet.clone(), *range)),
        ),
        CsvExportSelection::Table { table } => report.exact_event(
            ConversionEvent::new(
                ConversionFeature::Other("selected_table".to_owned()),
                "the explicitly selected table rectangle was exported",
            )
            .at(ConversionLocation::table(table.clone())),
        ),
    }
    let mut output = Vec::new();
    for row in range.start.row..=range.end.row {
        for column in range.start.column..=range.end.column {
            if column != range.start.column {
                push_bounded(&mut output, b",", limits)?;
            }
            let coordinate = Coordinate { column, row };
            let value = selected_value(sheet, coordinate, limits)?;
            if let Some(value) = value {
                if let Value::Formula(formula) = &value {
                    validate_csv_export_formula(formula.as_str(), &sheet.id, coordinate, limits)?;
                }
                let spelling = scalar_spelling(&value)?;
                let encoded = quote_csv(&spelling);
                push_bounded(&mut output, encoded.as_bytes(), limits)?;
                if let Value::Formula(formula) = value {
                    report.formula(FormulaEvent {
                        disposition: FormulaDisposition::Preserved,
                        source: Some(formula.as_str().to_owned()),
                        destination: Some(formula.as_str().to_owned()),
                        locations: vec![ConversionLocation::cell(sheet.id.clone(), coordinate)],
                    });
                }
            }
        }
        push_bounded(&mut output, b"\n", limits)?;
    }

    let omission_location = if let Some(table) = table_selection {
        ConversionLocation::table(table)
    } else {
        ConversionLocation::range(sheet.id.clone(), range)
    };
    record_csv_omissions(workbook, &mut report, omission_location);
    Ok(Conversion {
        value: output,
        report: report.finish(),
    })
}

/// Imports strict UTF-8 RFC 4180 CSV into one explicitly selected range or
/// table. The operation is atomic and performs no file I/O.
///
/// # Errors
///
/// Returns an error for malformed/ragged CSV, invalid scalar spellings,
/// selection dimension mismatch, invalid table headers, or resource limits.
pub fn import_csv(
    bytes: &[u8],
    selection: &CsvImportSelection,
    limits: ConversionLimits,
) -> ConversionResult<Workbook> {
    import_csv_inner(bytes, selection, limits).map_err(|error| {
        ConversionFailure::new(
            error,
            FormatDescriptor::csv(),
            FormatDescriptor::marksheet_ir(),
            "source_csv",
        )
    })
}

fn import_csv_inner(
    bytes: &[u8],
    selection: &CsvImportSelection,
    limits: ConversionLimits,
) -> Result<Conversion<Workbook>, ConvertError> {
    limits.check_input(bytes.len())?;
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(invalid_csv(
            "UTF-8 BOM is not accepted by the strict CSV profile",
        ));
    }
    let rows = parse_csv(bytes, limits)?;
    let height = u64::try_from(rows.len()).map_err(|_| resource("CSV row count overflow"))?;
    let width = u64::try_from(rows[0].len()).map_err(|_| resource("CSV width overflow"))?;
    let anchor = match selection {
        CsvImportSelection::Range { range, .. } => range.start,
        CsvImportSelection::Table { anchor, .. } => *anchor,
    };
    let formulas = rows
        .iter()
        .enumerate()
        .flat_map(|(row, values)| {
            values
                .iter()
                .enumerate()
                .filter_map(move |(column, value)| match value {
                    Value::Formula(formula) => Some((row, column, formula.as_str().to_owned())),
                    _ => None,
                })
        })
        .collect::<Vec<_>>();

    validate_csv_formulas(
        &formulas,
        anchor,
        &sheet_id_for_selection(selection),
        limits,
    )?;

    let (sheet_id, label, item) = match selection {
        CsvImportSelection::Range {
            sheet,
            label,
            range,
        } => {
            let expected_width = range
                .width()
                .map_err(|error| invalid_selection(error.to_string()))?;
            let expected_height = range
                .height()
                .map_err(|error| invalid_selection(error.to_string()))?;
            if width != expected_width || height != expected_height {
                return Err(invalid_selection(format!(
                    "CSV is {width}x{height}, but selected range {range} is {expected_width}x{expected_height}"
                )));
            }
            let block = Block::new(range.start, into_cells(rows))
                .map_err(|error| invalid_csv(error.to_string()))?;
            (sheet.clone(), label.clone(), SheetItem::Block(block))
        }
        CsvImportSelection::Table {
            sheet,
            label,
            table,
            anchor,
        } => {
            validate_headers(&rows)?;
            let block = Block::new(*anchor, into_cells(rows))
                .map_err(|error| invalid_csv(error.to_string()))?;
            block
                .footprint()
                .map_err(|error| invalid_selection(error.to_string()))?;
            (
                sheet.clone(),
                label.clone(),
                SheetItem::Table(Table {
                    id: table.clone(),
                    block,
                    origin: None,
                }),
            )
        }
    };

    let workbook = Workbook {
        sheets: vec![Sheet {
            id: sheet_id.clone(),
            label,
            items: vec![item],
            origin: None,
        }],
        ..Workbook::default()
    };
    let mut report =
        ConversionReport::new(FormatDescriptor::csv(), FormatDescriptor::marksheet_ir());
    let target_location = match selection {
        CsvImportSelection::Range { range, .. } => {
            ConversionLocation::range(sheet_id.clone(), *range)
        }
        CsvImportSelection::Table { table, .. } => {
            ConversionLocation::table_on_sheet(sheet_id.clone(), table.clone())
        }
    };
    report.exact_event(
        ConversionEvent::new(
            ConversionFeature::Other("csv_target".to_owned()),
            "CSV was placed into the explicitly selected sheet target",
        )
        .at(target_location),
    );
    report.exact_event(ConversionEvent::new(
        ConversionFeature::Cell,
        format!("imported {width} columns and {height} rows without inference"),
    ));
    if matches!(selection, CsvImportSelection::Table { .. }) {
        report.exact_event(ConversionEvent::new(
            ConversionFeature::Table,
            "the explicit target table and header row were created",
        ));
    }
    for (row, column, formula) in formulas {
        let coordinate = anchor
            .offset(
                u64::try_from(column).unwrap_or(u64::MAX),
                u64::try_from(row).unwrap_or(u64::MAX),
            )
            .map_err(|error| invalid_selection(error.to_string()))?;
        report.formula(FormulaEvent {
            disposition: FormulaDisposition::Preserved,
            source: Some(formula.clone()),
            destination: Some(formula),
            locations: vec![ConversionLocation::cell(sheet_id.clone(), coordinate)],
        });
    }
    Ok(Conversion {
        value: workbook,
        report: report.finish(),
    })
}

fn selected_value(
    sheet: &PreparedSheet,
    coordinate: Coordinate,
    limits: ConversionLimits,
) -> Result<Option<Value>, ConvertError> {
    if let Some(virtual_cell) = sheet.virtual_cell(coordinate) {
        let parse_limits = formula_parse_limits(limits);
        let parsed = parse(virtual_cell.formula.as_str(), &parse_limits)
            .map_err(|error| invalid_workbook(format!("invalid fill formula: {error}")))?;
        let adjusted = FormulaTemplate::new(virtual_cell.fill_anchor, parsed)
            .bind(coordinate)
            .map_err(|error| invalid_workbook(format!("fill adjustment failed: {error}")))?;
        let source = format_formula(&adjusted)
            .map_err(|error| invalid_workbook(format!("fill formatting failed: {error}")))?;
        return FormulaSource::new(source)
            .map(Value::Formula)
            .map(Some)
            .map_err(|error| invalid_workbook(error.to_string()));
    }
    Ok(sheet
        .authored_cell(coordinate)
        .map(|authored| authored.cell.value.clone()))
}

pub(crate) fn scalar_spelling(value: &Value) -> Result<String, ConvertError> {
    match value {
        Value::Blank => Ok(String::new()),
        Value::Text(text) => {
            let already_text =
                matches!(Value::parse_strict(text), Ok(Value::Text(parsed)) if parsed == *text);
            Ok(if already_text {
                text.clone()
            } else {
                format!("'{text}")
            })
        }
        Value::Number(number) => canonical_number(*number)
            .map_err(|error| invalid_workbook(format!("invalid number: {error}"))),
        Value::Boolean(value) => Ok(value.to_string()),
        Value::Date(value) => Ok(value.to_string()),
        Value::DateTime(value) => value
            .format(&Rfc3339)
            .map_err(|error| invalid_workbook(format!("invalid datetime: {error}"))),
        Value::Formula(formula) => Ok(formula.as_str().to_owned()),
        Value::Error(error) => Ok(error.token().to_owned()),
    }
}

pub(crate) fn quote_csv(value: &str) -> String {
    if value
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\r' | b'\n'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn parse_csv(bytes: &[u8], limits: ConversionLimits) -> Result<Vec<Vec<Value>>, ConvertError> {
    let source = std::str::from_utf8(bytes).map_err(|_| invalid_csv("CSV must be valid UTF-8"))?;
    if source.is_empty() {
        return Err(invalid_csv("CSV must contain at least one record"));
    }

    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut state = CsvState::Start;
    let mut chars = source.chars().peekable();
    let mut cells = 0_u64;
    let mut ended_record = false;

    while let Some(character) = chars.next() {
        ended_record = false;
        match state {
            CsvState::Start => match character {
                '"' => state = CsvState::Quoted,
                ',' => finish_field(&mut row, &mut field, limits, &mut cells)?,
                '\n' => {
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    finish_row(&mut rows, &mut row)?;
                    ended_record = true;
                }
                '\r' if chars.peek() == Some(&'\n') => {
                    chars.next();
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    finish_row(&mut rows, &mut row)?;
                    ended_record = true;
                }
                '\r' => {
                    return Err(invalid_csv(
                        "a carriage return must be followed by line feed",
                    ));
                }
                _ => {
                    field.push(character);
                    check_field(&field, limits)?;
                    state = CsvState::Unquoted;
                }
            },
            CsvState::Unquoted => match character {
                ',' => {
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    state = CsvState::Start;
                }
                '\n' => {
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    finish_row(&mut rows, &mut row)?;
                    state = CsvState::Start;
                    ended_record = true;
                }
                '\r' if chars.peek() == Some(&'\n') => {
                    chars.next();
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    finish_row(&mut rows, &mut row)?;
                    state = CsvState::Start;
                    ended_record = true;
                }
                '\r' => {
                    return Err(invalid_csv(
                        "a carriage return must be followed by line feed",
                    ));
                }
                '"' => return Err(invalid_csv("a quote inside an unquoted field is invalid")),
                _ => {
                    field.push(character);
                    check_field(&field, limits)?;
                }
            },
            CsvState::Quoted => {
                match character {
                    '"' => state = CsvState::AfterQuote,
                    // RFC 4180 permits CRLF inside a quoted field. The CSV
                    // transport spelling is normalized to Marksheet's LF
                    // text convention; CRLF outside quotes remains a record
                    // delimiter in the states above.
                    '\r' if chars.peek() == Some(&'\n') => {
                        chars.next();
                        field.push('\n');
                        check_field(&field, limits)?;
                    }
                    '\r' => {
                        return Err(invalid_csv(
                            "a carriage return must be followed by line feed",
                        ));
                    }
                    _ => {
                        field.push(character);
                        check_field(&field, limits)?;
                    }
                }
            }
            CsvState::AfterQuote => match character {
                '"' => {
                    field.push('"');
                    check_field(&field, limits)?;
                    state = CsvState::Quoted;
                }
                ',' => {
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    state = CsvState::Start;
                }
                '\n' => {
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    finish_row(&mut rows, &mut row)?;
                    state = CsvState::Start;
                    ended_record = true;
                }
                '\r' if chars.peek() == Some(&'\n') => {
                    chars.next();
                    finish_field(&mut row, &mut field, limits, &mut cells)?;
                    finish_row(&mut rows, &mut row)?;
                    state = CsvState::Start;
                    ended_record = true;
                }
                '\r' => {
                    return Err(invalid_csv(
                        "a carriage return must be followed by line feed",
                    ));
                }
                _ => return Err(invalid_csv("characters after a closing quote are invalid")),
            },
        }
    }
    if state == CsvState::Quoted {
        return Err(invalid_csv("quoted field is not terminated"));
    }
    if !ended_record {
        finish_field(&mut row, &mut field, limits, &mut cells)?;
        finish_row(&mut rows, &mut row)?;
    }
    if rows.is_empty() {
        return Err(invalid_csv("CSV must contain at least one record"));
    }
    let width = rows[0].len();
    if rows.iter().any(|candidate| candidate.len() != width) {
        return Err(invalid_csv("CSV records must have equal field counts"));
    }
    Ok(rows)
}

fn range_has_selected_cell(sheet: &PreparedSheet, range: Range) -> bool {
    sheet
        .authored_cells
        .keys()
        .chain(sheet.virtual_cells.keys())
        .any(|coordinate| range.contains(*coordinate))
}

fn sheet_id_for_selection(selection: &CsvImportSelection) -> SheetId {
    match selection {
        CsvImportSelection::Range { sheet, .. } | CsvImportSelection::Table { sheet, .. } => {
            sheet.clone()
        }
    }
}

fn validate_csv_formulas(
    formulas: &[(usize, usize, String)],
    anchor: Coordinate,
    sheet: &SheetId,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let formula_count =
        u64::try_from(formulas.len()).map_err(|_| resource("CSV formula count overflow"))?;
    if formula_count > limits.max_formulas {
        return Err(resource("CSV formula count exceeds the configured limit"));
    }
    let parse_limits = formula_parse_limits(limits);
    for (row, column, source) in formulas {
        let coordinate = anchor
            .offset(
                u64::try_from(*column).map_err(|_| resource("CSV column index overflow"))?,
                u64::try_from(*row).map_err(|_| resource("CSV row index overflow"))?,
            )
            .map_err(|error| invalid_selection(error.to_string()))?;
        let parsed = parse(source, &parse_limits).map_err(|error| {
            invalid_csv(format!("formula is not valid portable-a1@1: {error}"))
                .at(ConversionLocation::cell(sheet.clone(), coordinate))
        })?;
        validate_formula_expression(&parsed.expression).map_err(|error| {
            invalid_csv(error.to_string()).at(ConversionLocation::cell(sheet.clone(), coordinate))
        })?;
    }
    Ok(())
}

fn validate_csv_export_formula(
    source: &str,
    sheet: &SheetId,
    coordinate: Coordinate,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let parsed = parse(source, &formula_parse_limits(limits)).map_err(|error| {
        invalid_workbook(format!("formula is not valid portable-a1@1: {error}"))
            .at(ConversionLocation::cell(sheet.clone(), coordinate))
    })?;
    validate_formula_expression(&parsed.expression).map_err(|error| {
        invalid_workbook(error.to_string()).at(ConversionLocation::cell(sheet.clone(), coordinate))
    })
}

fn formula_parse_limits(limits: ConversionLimits) -> ParseLimits {
    ParseLimits {
        max_source_bytes: limits.max_string_bytes,
        max_tokens: limits.max_string_bytes.min(100_000),
        max_depth: limits.max_xml_depth.max(1),
        max_nodes: limits.max_string_bytes.min(100_000),
        max_function_arguments: limits.max_string_bytes.min(10_000),
    }
}

fn finish_field(
    row: &mut Vec<Value>,
    field: &mut String,
    limits: ConversionLimits,
    cells: &mut u64,
) -> Result<(), ConvertError> {
    check_field(field, limits)?;
    *cells = cells
        .checked_add(1)
        .ok_or_else(|| resource("CSV cell count overflow"))?;
    if *cells > limits.max_cells {
        return Err(resource("CSV cell count exceeds the configured limit"));
    }
    let value = Value::parse_strict(field).map_err(|error| invalid_csv(error.to_string()))?;
    row.push(value);
    field.clear();
    Ok(())
}

fn finish_row(rows: &mut Vec<Vec<Value>>, row: &mut Vec<Value>) -> Result<(), ConvertError> {
    if row.is_empty() {
        return Err(invalid_csv("CSV record has no fields"));
    }
    rows.push(std::mem::take(row));
    Ok(())
}

fn check_field(field: &str, limits: ConversionLimits) -> Result<(), ConvertError> {
    if field.len() > limits.max_string_bytes {
        return Err(resource(
            "decoded CSV field exceeds the configured string limit",
        ));
    }
    Ok(())
}

fn validate_headers(rows: &[Vec<Value>]) -> Result<(), ConvertError> {
    let mut headers = BTreeSet::new();
    for value in &rows[0] {
        let Value::Text(header) = value else {
            return Err(invalid_csv("every table header must be text"));
        };
        if header.is_empty() {
            return Err(invalid_csv("table headers must not be empty"));
        }
        if !headers.insert(header) {
            return Err(invalid_csv("table headers must be unique"));
        }
    }
    Ok(())
}

fn into_cells(rows: Vec<Vec<Value>>) -> Vec<Vec<Cell>> {
    rows.into_iter()
        .map(|row| row.into_iter().map(Cell::new).collect())
        .collect()
}

fn record_csv_omissions(
    _workbook: &Workbook,
    report: &mut ConversionReport,
    location: ConversionLocation,
) {
    report.omit(
        ConversionEvent::new(
            ConversionFeature::Other("workbook_features_outside_selection".to_owned()),
            "CSV contains only selected scalar fields; workbook structure and presentation are omitted",
        )
        .at(location),
    );
}

fn push_bounded(
    output: &mut Vec<u8>,
    bytes: &[u8],
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let new_len = output
        .len()
        .checked_add(bytes.len())
        .ok_or_else(|| ConvertError::new(ConvertErrorCode::OutputLimit, "output size overflow"))?;
    if u64::try_from(new_len).unwrap_or(u64::MAX) > limits.max_output_bytes {
        return Err(ConvertError::new(
            ConvertErrorCode::OutputLimit,
            "CSV output exceeds the configured limit",
        ));
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn invalid_csv(message: impl Into<String>) -> ConvertError {
    ConvertError::new(ConvertErrorCode::InvalidCsv, message)
}

fn invalid_selection(message: impl Into<String>) -> ConvertError {
    ConvertError::new(ConvertErrorCode::InvalidSelection, message)
}

fn invalid_workbook(message: impl Into<String>) -> ConvertError {
    ConvertError::new(ConvertErrorCode::InvalidWorkbook, message)
}

fn resource(message: impl Into<String>) -> ConvertError {
    ConvertError::new(ConvertErrorCode::ResourceLimit, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::{CellError, Sheet};
    use time::{Date, Month, OffsetDateTime};

    fn sheet_id() -> SheetId {
        SheetId::parse("data").unwrap()
    }

    fn range_selection(range: &str) -> CsvImportSelection {
        CsvImportSelection::Range {
            sheet: sheet_id(),
            label: "Data".to_owned(),
            range: Range::parse(range).unwrap(),
        }
    }

    #[test]
    fn strict_parser_handles_quotes_newlines_and_crlf() {
        let imported = import_csv(
            b"name,note\r\nAda,\"one, \"\"two\"\"\r\nthree\"\r\n",
            &range_selection("A1:B2"),
            ConversionLimits::default(),
        )
        .unwrap();
        let SheetItem::Block(block) = &imported.value.sheets[0].items[0] else {
            panic!("expected block")
        };
        assert_eq!(
            block.cells[1][1].value,
            Value::Text("one, \"two\"\nthree".to_owned())
        );
    }

    #[test]
    fn rejects_non_portable_formula_with_its_destination_location() {
        let error = import_csv(
            b"name,value\nAda,=A1#\n",
            &range_selection("A1:B2"),
            ConversionLimits::default(),
        )
        .expect_err("dynamic-array suffix is outside portable-a1@1");

        assert_eq!(error.code, ConvertErrorCode::InvalidCsv);
        assert!(error.message.contains("portable-a1@1"));
        assert_eq!(
            error.location,
            Some(ConversionLocation::cell(
                sheet_id(),
                Coordinate::parse("B2").unwrap(),
            ))
        );
    }

    #[test]
    fn rejects_csv_formula_count_over_the_configured_limit() {
        let error = import_csv(
            b"=A1\n",
            &range_selection("A1"),
            ConversionLimits {
                max_formulas: 0,
                ..ConversionLimits::default()
            },
        )
        .expect_err("formula-bearing cells are bounded independently");

        assert_eq!(error.code, ConvertErrorCode::ResourceLimit);
    }

    #[test]
    fn rejects_formula_arity_mismatch_on_import_and_export() {
        let imported = import_csv(
            b"\"=IF(TRUE,1)\"\n",
            &range_selection("A1"),
            ConversionLimits::default(),
        )
        .expect_err("CSV import must enforce the evaluator function signature");
        assert_eq!(imported.code, ConvertErrorCode::InvalidCsv);
        assert!(
            imported.message.contains("exactly 3 arguments"),
            "{}",
            imported.message
        );

        let workbook = Workbook {
            sheets: vec![Sheet {
                id: sheet_id(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![vec![Cell::new(Value::Formula(
                            FormulaSource::new("=IF(TRUE,1)").unwrap(),
                        ))]],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_csv(
            &workbook,
            &CsvExportSelection::Range {
                sheet: sheet_id(),
                range: Range::parse("A1").unwrap(),
            },
            ConversionLimits::default(),
        )
        .expect_err("CSV export must enforce the evaluator function signature");
        assert_eq!(exported.code, ConvertErrorCode::InvalidWorkbook);
        assert!(
            exported.message.contains("exactly 3 arguments"),
            "{}",
            exported.message
        );
    }

    #[test]
    fn rejects_malformed_and_ragged_csv_atomically() {
        for bytes in [b"a,\"unterminated".as_slice(), b"a,b\n1\n", b"a\rb\n"] {
            assert!(
                import_csv(
                    bytes,
                    &range_selection("A1:B2"),
                    ConversionLimits::default()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn scalar_spelling_round_trips_all_scalar_kinds() {
        let date = Date::from_calendar_date(2026, Month::August, 16).unwrap();
        let datetime = OffsetDateTime::parse("2026-08-16T12:34:56+02:00", &Rfc3339).unwrap();
        let values = vec![
            Value::Blank,
            Value::Text(String::new()),
            Value::Text("true".to_owned()),
            Value::Text("'leading".to_owned()),
            Value::Number(-0.0),
            Value::Boolean(true),
            Value::Date(date),
            Value::DateTime(datetime),
            Value::Formula(FormulaSource::new("=SUM(A1,2)").unwrap()),
            Value::Error(CellError::Reference),
        ];
        for value in values {
            let spelling = scalar_spelling(&value).unwrap();
            assert_eq!(Value::parse_strict(&spelling).unwrap(), value, "{spelling}");
        }
    }

    #[test]
    fn explicit_range_preserves_absent_edges() {
        let workbook = Workbook {
            sheets: vec![Sheet {
                id: sheet_id(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("B2").unwrap(),
                        vec![vec![Cell::new(Value::Number(1.0))]],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        };
        let converted = export_csv(
            &workbook,
            &CsvExportSelection::Range {
                sheet: sheet_id(),
                range: Range::parse("A1:C3").unwrap(),
            },
            ConversionLimits::default(),
        )
        .unwrap();
        assert_eq!(converted.value, b",,\n,1,\n,,\n");
        assert!(!converted.report.is_lossless());
    }

    #[test]
    fn explicit_range_without_a_cell_is_rejected() {
        let workbook = Workbook {
            sheets: vec![Sheet {
                id: sheet_id(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![vec![Cell::new(Value::Number(1.0))]],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        };

        let error = export_csv(
            &workbook,
            &CsvExportSelection::Range {
                sheet: sheet_id(),
                range: Range::parse("Z99").unwrap(),
            },
            ConversionLimits::default(),
        )
        .expect_err("an unrelated rectangle is not an exportable selection");
        assert_eq!(error.code, ConvertErrorCode::InvalidSelection);
    }

    #[test]
    fn explicit_table_selection_remains_valid() {
        let table = TableId::parse("items").unwrap();
        let workbook = Workbook {
            sheets: vec![Sheet {
                id: sheet_id(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Table(Table {
                    id: table.clone(),
                    block: Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![vec![Cell::new(Value::Text("name".to_owned()))]],
                    )
                    .unwrap(),
                    origin: None,
                })],
                origin: None,
            }],
            ..Workbook::default()
        };

        let converted = export_csv(
            &workbook,
            &CsvExportSelection::Table { table },
            ConversionLimits::default(),
        )
        .expect("a selected table has an explicit, exportable footprint");
        assert_eq!(converted.value, b"name\n");
    }

    #[test]
    fn table_headers_are_explicit_not_inferred() {
        let selection = CsvImportSelection::Table {
            sheet: sheet_id(),
            label: "Data".to_owned(),
            table: TableId::parse("items").unwrap(),
            anchor: Coordinate::parse("A1").unwrap(),
        };
        assert!(import_csv(b"name,name\na,b\n", &selection, ConversionLimits::default()).is_err());
        assert!(
            import_csv(
                b"name,value\na,1\n",
                &selection,
                ConversionLimits::default()
            )
            .is_ok()
        );
    }
}
