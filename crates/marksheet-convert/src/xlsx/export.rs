#![allow(clippy::format_push_string)] // XML is assembled in one bounded schema-ordered buffer.
#![allow(clippy::too_many_lines)] // Part writers intentionally mirror OOXML element order.
#![allow(clippy::uninlined_format_args)] // Positional fields keep long XML templates readable.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Seek, SeekFrom, Write},
};

use marksheet_calc::formula::{
    A1Reference, Expr, ExprKind, ParseLimits, Reference, StructuredReference,
    TableRegion as FormulaTableRegion, parse as parse_formula,
};
use marksheet_model::{
    CellError, Color, Coordinate, FillTarget, HorizontalAlignment, NameTarget, NumberFormat,
    SheetItem, StyleProperties, TableId, Value, VerticalAlignment, Workbook, canonical_number,
};
use time::format_description::well_known::Rfc3339;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    Conversion, ConversionEvent, ConversionFailure, ConversionFeature, ConversionLimits,
    ConversionLocation, ConversionReport, ConversionResult, ConvertError, ConvertErrorCode,
    FormatDescriptor, FormulaDisposition, formula_profile::validate_formula_expression,
};

use super::xml::{escape_attribute, escape_text, is_xml_character};
use crate::project::{
    ProjectedSheet, ProjectedTable, ProjectedWorkbook, RowGeometryWorkBudget, XLSX_MAX_COLUMN,
    XLSX_MAX_ROW, effective_column_runs, effective_row_heights, project,
};

const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";
const MAIN_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

#[derive(Clone, Debug)]
struct FormulaSymbols {
    sheet_labels: BTreeMap<String, String>,
    table_names: BTreeMap<String, String>,
    name_names: BTreeMap<String, String>,
}

/// Converts Marksheet IR to a deterministic, minimal OOXML workbook.
///
/// # Errors
///
/// Rejects invalid IR, coordinates outside the XLSX grid, unsupported scalar
/// encodings that cannot be honestly approximated, and resource limits.
pub fn export_xlsx(workbook: &Workbook, limits: ConversionLimits) -> ConversionResult<Vec<u8>> {
    export_xlsx_inner(workbook, limits).map_err(|error| {
        ConversionFailure::new(
            error,
            FormatDescriptor::marksheet_ir(),
            FormatDescriptor::xlsx(),
            "source_workbook",
        )
    })
}

fn export_xlsx_inner(
    workbook: &Workbook,
    limits: ConversionLimits,
) -> Result<Conversion<Vec<u8>>, ConvertError> {
    let mut report =
        ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
    let projected = project(workbook, limits)?;
    let sheet_labels = export_sheet_labels(&projected, &mut report);
    let table_names = export_table_names(&projected, &mut report);
    let name_names = export_name_names(workbook, &mut report);
    let symbols = FormulaSymbols {
        sheet_labels: projected
            .sheets
            .iter()
            .zip(&sheet_labels)
            .map(|(sheet, label)| (sheet.id.to_string(), label.clone()))
            .collect(),
        table_names: table_names.clone(),
        name_names: name_names.clone(),
    };
    let styles = collect_styles(&projected, &mut report, limits)?;
    if styles.definitions.len() > limits.max_styles {
        return Err(resource(
            "resolved XLSX style count exceeds the configured limit",
        ));
    }

    let table_count: usize = projected
        .sheets
        .iter()
        .map(|sheet| sheet.tables.len())
        .sum();
    if table_count > limits.max_tables {
        return Err(resource("XLSX table count exceeds the configured limit"));
    }

    let mut parts = vec![
        ("[Content_Types].xml".to_owned(), content_types(&projected)),
        ("_rels/.rels".to_owned(), root_relationships()),
        (
            "xl/workbook.xml".to_owned(),
            workbook_xml(&projected, &sheet_labels, &table_names, &name_names)?,
        ),
        (
            "xl/_rels/workbook.xml.rels".to_owned(),
            workbook_relationships(projected.sheets.len()),
        ),
        ("xl/styles.xml".to_owned(), styles_xml(&styles.definitions)),
    ];

    let mut next_table_part = 1_usize;
    // Worksheet XML is built before the ZIP sink can enforce its byte cap.
    // Share one geometry-expansion budget across sheets so range-shaped row
    // declarations cannot allocate one large height map per worksheet first.
    let mut row_geometry_budget = RowGeometryWorkBudget::for_export(limits);
    for (sheet_index, sheet) in projected.sheets.iter().enumerate() {
        let table_parts: Vec<usize> =
            (next_table_part..next_table_part + sheet.tables.len()).collect();
        let sheet_path = format!("xl/worksheets/sheet{}.xml", sheet_index + 1);
        parts.push((
            sheet_path,
            worksheet_xml(
                sheet,
                &styles,
                &table_parts,
                &symbols,
                limits,
                &mut row_geometry_budget,
                &mut report,
            )?,
        ));
        if !table_parts.is_empty() {
            parts.push((
                format!("xl/worksheets/_rels/sheet{}.xml.rels", sheet_index + 1),
                worksheet_relationships(&table_parts),
            ));
        }
        for (table, part_index) in sheet.tables.iter().zip(table_parts) {
            let display_name = table_names.get(&table.id.to_string()).ok_or_else(|| {
                ConvertError::new(
                    ConvertErrorCode::Internal,
                    "projected table has no deterministic export name",
                )
            })?;
            parts.push((
                format!("xl/tables/table{part_index}.xml"),
                table_xml(
                    table,
                    display_name,
                    part_index,
                    &table_fill_formulas(&workbook.sheets[sheet_index], &table.id),
                    &symbols,
                    limits,
                )?,
            ));
        }
        next_table_part += sheet.tables.len();
    }

    record_exact_features(workbook, &projected, &mut report);
    record_omissions(workbook, &mut report);
    let bytes = write_zip(parts, limits)?;
    Ok(Conversion {
        value: bytes,
        report: report.finish(),
    })
}

fn content_types(workbook: &ProjectedWorkbook) -> Vec<u8> {
    let mut xml = format!(
        "{XML_HEADER}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>"
    );
    let mut table_index = 1;
    for (sheet_index, sheet) in workbook.sheets.iter().enumerate() {
        xml.push_str(&format!("<Override PartName=\"/xl/worksheets/sheet{}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>", sheet_index + 1));
        for _ in &sheet.tables {
            xml.push_str(&format!("<Override PartName=\"/xl/tables/table{table_index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml\"/>"));
            table_index += 1;
        }
    }
    xml.push_str("</Types>");
    xml.into_bytes()
}

fn root_relationships() -> Vec<u8> {
    format!("{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>").into_bytes()
}

fn workbook_relationships(sheet_count: usize) -> Vec<u8> {
    let mut xml = format!(
        "{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
    );
    for index in 0..sheet_count {
        xml.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>", index + 1, index + 1));
    }
    xml.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>", sheet_count + 1));
    xml.push_str("</Relationships>");
    xml.into_bytes()
}

fn workbook_xml(
    projected: &ProjectedWorkbook,
    labels: &[String],
    table_names: &BTreeMap<String, String>,
    name_names: &BTreeMap<String, String>,
) -> Result<Vec<u8>, ConvertError> {
    let mut xml = format!(
        "{XML_HEADER}<workbook xmlns=\"{MAIN_NS}\" xmlns:r=\"{REL_NS}\"><workbookPr date1904=\"0\"/><sheets>"
    );
    for (index, label) in labels.iter().enumerate() {
        xml.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
            escape_attribute(label),
            index + 1,
            index + 1
        ));
    }
    xml.push_str("</sheets>");
    if !projected.names.is_empty() {
        xml.push_str("<definedNames>");
        for name in &projected.names {
            let export_name = name_names.get(name.id.as_str()).ok_or_else(|| {
                ConvertError::new(
                    ConvertErrorCode::Internal,
                    "projected name has no deterministic export name",
                )
            })?;
            let target = name_target_formula(&name.target, projected, labels, table_names)?;
            xml.push_str(&format!(
                "<definedName name=\"{}\">{}</definedName>",
                escape_attribute(export_name),
                escape_text(&target)
            ));
        }
        xml.push_str("</definedNames>");
    }
    // Full calculation on open avoids stale or nondeterministic cached values.
    xml.push_str("<calcPr calcId=\"0\" fullCalcOnLoad=\"1\" forceFullCalc=\"1\"/></workbook>");
    Ok(xml.into_bytes())
}

fn name_target_formula(
    target: &NameTarget,
    workbook: &ProjectedWorkbook,
    labels: &[String],
    table_names: &BTreeMap<String, String>,
) -> Result<String, ConvertError> {
    match target {
        NameTarget::Cell(target) => {
            check_reference_coordinate(target.coordinate)?;
            let label = label_for_sheet(&target.sheet, workbook, labels)?;
            Ok(format!(
                "{}!{}",
                quote_sheet(label),
                absolute_coordinate(target.coordinate)
            ))
        }
        NameTarget::Range(target) => {
            check_reference_coordinate(target.range.start)?;
            check_reference_coordinate(target.range.end)?;
            let label = label_for_sheet(&target.sheet, workbook, labels)?;
            Ok(format!(
                "{}!{}",
                quote_sheet(label),
                absolute_range(target.range)
            ))
        }
        NameTarget::TableColumn { table, header } => {
            let table = table_names.get(table.as_str()).ok_or_else(|| {
                ConvertError::new(
                    ConvertErrorCode::InvalidWorkbook,
                    format!("name refers to unresolved table {table}"),
                )
            })?;
            Ok(format!("{table}[{}]", header.replace(']', "]]")))
        }
    }
}

fn label_for_sheet<'a>(
    id: &marksheet_model::SheetId,
    workbook: &'a ProjectedWorkbook,
    labels: &'a [String],
) -> Result<&'a str, ConvertError> {
    workbook
        .sheets
        .iter()
        .position(|sheet| &sheet.id == id)
        .and_then(|index| labels.get(index))
        .map(String::as_str)
        .ok_or_else(|| {
            ConvertError::new(
                ConvertErrorCode::InvalidWorkbook,
                format!("unresolved sheet {id}"),
            )
        })
}

fn worksheet_relationships(table_parts: &[usize]) -> Vec<u8> {
    let mut xml = format!(
        "{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
    );
    for (relationship, part) in table_parts.iter().enumerate() {
        xml.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/table\" Target=\"../tables/table{part}.xml\"/>", relationship + 1));
    }
    xml.push_str("</Relationships>");
    xml.into_bytes()
}

fn worksheet_xml(
    sheet: &ProjectedSheet,
    styles: &ExportStyles,
    table_parts: &[usize],
    symbols: &FormulaSymbols,
    limits: ConversionLimits,
    row_geometry_budget: &mut RowGeometryWorkBudget,
    report: &mut ConversionReport,
) -> Result<Vec<u8>, ConvertError> {
    let mut xml = format!("{XML_HEADER}<worksheet xmlns=\"{MAIN_NS}\" xmlns:r=\"{REL_NS}\">");
    let columns = effective_column_runs(&sheet.columns);
    if !columns.is_empty() {
        xml.push_str("<cols>");
        for (start, end, width) in columns {
            xml.push_str(&format!(
                "<col min=\"{start}\" max=\"{end}\" width=\"{}\" customWidth=\"1\"/>",
                number(width)?
            ));
        }
        xml.push_str("</cols>");
    }

    let row_heights = effective_row_heights(&sheet.rows, limits.max_cells, row_geometry_budget)?;
    let mut rows: BTreeSet<u64> = row_heights.keys().copied().collect();
    rows.extend(sheet.cells.keys().map(|coordinate| coordinate.row));
    rows.extend(sheet.styles.keys().map(|coordinate| coordinate.row));
    let mut coordinates_by_row = BTreeMap::<u64, BTreeSet<Coordinate>>::new();
    for coordinate in sheet.cells.keys().chain(sheet.styles.keys()) {
        coordinates_by_row
            .entry(coordinate.row)
            .or_default()
            .insert(*coordinate);
    }
    xml.push_str("<sheetData>");
    for row in rows {
        if let Some(height) = row_heights.get(&row) {
            xml.push_str(&format!(
                "<row r=\"{row}\" ht=\"{}\" customHeight=\"1\">",
                number(*height)?
            ));
        } else {
            xml.push_str(&format!("<row r=\"{row}\">"));
        }
        for coordinate in coordinates_by_row.get(&row).into_iter().flatten() {
            let style_index = styles
                .by_cell
                .get(&(sheet.id.to_string(), *coordinate))
                .copied()
                .unwrap_or(0);
            let value = sheet.cells.get(coordinate).map(|cell| &cell.value);
            let mut context = CellWriteContext {
                sheet: &sheet.id,
                symbols,
                limits,
                report,
            };
            cell_xml(&mut xml, *coordinate, value, style_index, &mut context)?;
        }
        xml.push_str("</row>");
    }
    xml.push_str("</sheetData>");
    if !table_parts.is_empty() {
        xml.push_str(&format!("<tableParts count=\"{}\">", table_parts.len()));
        for index in 0..table_parts.len() {
            xml.push_str(&format!("<tablePart r:id=\"rId{}\"/>", index + 1));
        }
        xml.push_str("</tableParts>");
    }
    xml.push_str("</worksheet>");
    Ok(xml.into_bytes())
}

struct CellWriteContext<'a> {
    sheet: &'a marksheet_model::SheetId,
    symbols: &'a FormulaSymbols,
    limits: ConversionLimits,
    report: &'a mut ConversionReport,
}

fn cell_xml(
    xml: &mut String,
    coordinate: Coordinate,
    value: Option<&Value>,
    style_index: usize,
    context: &mut CellWriteContext<'_>,
) -> Result<(), ConvertError> {
    let style = if style_index == 0 {
        String::new()
    } else {
        format!(" s=\"{style_index}\"")
    };
    match value {
        // A style-only `<c/>` carries presentation without inventing a
        // Marksheet scalar. Authored Blank uses an explicit empty `<v/>`, so
        // our importer can preserve the otherwise unobservable distinction.
        None => {
            xml.push_str(&format!("<c r=\"{coordinate}\"{style}/>"));
        }
        Some(Value::Blank) => {
            xml.push_str(&format!("<c r=\"{coordinate}\"{style}><v/></c>"));
        }
        Some(Value::Text(text)) => {
            if !text.chars().all(is_xml_character) {
                return Err(ConvertError::new(
                    ConvertErrorCode::UnsupportedPackage,
                    "text contains a control character that XML cannot represent",
                )
                .at(ConversionLocation::cell(context.sheet.clone(), coordinate)));
            }
            let preserve = if text.starts_with(char::is_whitespace)
                || text.ends_with(char::is_whitespace)
                || text.is_empty()
            {
                " xml:space=\"preserve\""
            } else {
                ""
            };
            xml.push_str(&format!(
                "<c r=\"{coordinate}\"{style} t=\"inlineStr\"><is><t{preserve}>{}</t></is></c>",
                escape_text(text)
            ));
        }
        Some(Value::Number(value)) => {
            xml.push_str(&format!(
                "<c r=\"{coordinate}\"{style}><v>{}</v></c>",
                canonical_number(*value).map_err(|error| invalid_workbook(error.to_string()))?
            ));
        }
        Some(Value::Boolean(value)) => {
            xml.push_str(&format!(
                "<c r=\"{coordinate}\"{style} t=\"b\"><v>{}</v></c>",
                u8::from(*value)
            ));
        }
        Some(Value::Date(value)) => {
            xml.push_str(&format!(
                "<c r=\"{coordinate}\"{style} t=\"d\"><v>{value}</v></c>"
            ));
        }
        Some(Value::DateTime(value)) => {
            let encoded = value
                .format(&Rfc3339)
                .map_err(|error| invalid_workbook(format!("invalid datetime: {error}")))?;
            xml.push_str(&format!(
                "<c r=\"{coordinate}\"{style} t=\"d\"><v>{}</v></c>",
                escape_text(&encoded)
            ));
        }
        Some(Value::Formula(formula)) => {
            let translated = translate_formula(formula.as_str(), context.symbols, context.limits)?;
            xml.push_str(&format!(
                "<c r=\"{coordinate}\"{style}><f>{}</f></c>",
                escape_text(&translated)
            ));
        }
        Some(Value::Error(error)) => {
            let token = if *error == CellError::Circular {
                context.report.approximate(
                    ConversionEvent::new(
                        ConversionFeature::Cell,
                        "#CIRC! has no native XLSX scalar and was represented as #VALUE!",
                    )
                    .at(ConversionLocation::cell(context.sheet.clone(), coordinate)),
                );
                CellError::Value.token()
            } else {
                error.token()
            };
            xml.push_str(&format!(
                "<c r=\"{coordinate}\"{style} t=\"e\"><v>{token}</v></c>"
            ));
        }
    }
    Ok(())
}

fn table_xml(
    table: &ProjectedTable,
    display_name: &str,
    index: usize,
    calculated: &BTreeMap<String, String>,
    symbols: &FormulaSymbols,
    limits: ConversionLimits,
) -> Result<Vec<u8>, ConvertError> {
    let mut xml = format!(
        "{XML_HEADER}<table xmlns=\"{MAIN_NS}\" id=\"{index}\" name=\"{}\" displayName=\"{}\" ref=\"{}\" headerRowCount=\"1\"><autoFilter ref=\"{}\"/><tableColumns count=\"{}\">",
        escape_attribute(display_name),
        escape_attribute(display_name),
        table.range,
        table.range,
        table.headers.len()
    );
    for (column, header) in table.headers.iter().enumerate() {
        if let Some(formula) = calculated.get(header) {
            xml.push_str(&format!(
                "<tableColumn id=\"{}\" name=\"{}\"><calculatedColumnFormula>{}</calculatedColumnFormula></tableColumn>",
                column + 1,
                escape_attribute(header),
                escape_text(&translate_formula(formula, symbols, limits)?)
            ));
        } else {
            xml.push_str(&format!(
                "<tableColumn id=\"{}\" name=\"{}\"/>",
                column + 1,
                escape_attribute(header)
            ));
        }
    }
    xml.push_str("</tableColumns></table>");
    Ok(xml.into_bytes())
}

#[derive(Clone, Debug)]
struct ExportStyles {
    definitions: Vec<StyleProperties>,
    by_cell: BTreeMap<(String, Coordinate), usize>,
}

fn collect_styles(
    workbook: &ProjectedWorkbook,
    _report: &mut ConversionReport,
    limits: ConversionLimits,
) -> Result<ExportStyles, ConvertError> {
    let mut definitions = vec![StyleProperties::default()];
    let mut by_cell = BTreeMap::new();
    for sheet in &workbook.sheets {
        let mut coordinates: BTreeSet<Coordinate> = sheet.styles.keys().copied().collect();
        coordinates.extend(sheet.cells.keys().copied());
        for coordinate in coordinates {
            let style = sheet.styles.get(&coordinate).cloned().unwrap_or_default();
            if style == StyleProperties::default() {
                continue;
            }
            if style
                .font_size
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
            {
                return Err(invalid_workbook(
                    "style font size must be finite and positive",
                ));
            }
            let index = definitions
                .iter()
                .position(|candidate| candidate == &style)
                .unwrap_or_else(|| {
                    definitions.push(style);
                    definitions.len() - 1
                });
            if definitions.len() > limits.max_styles {
                return Err(resource(
                    "resolved XLSX style count exceeds the configured limit",
                ));
            }
            by_cell.insert((sheet.id.to_string(), coordinate), index);
        }
    }
    Ok(ExportStyles {
        definitions,
        by_cell,
    })
}

fn styles_xml(styles: &[StyleProperties]) -> Vec<u8> {
    let mut xml = format!("{XML_HEADER}<styleSheet xmlns=\"{MAIN_NS}\">");
    let custom_formats: Vec<(usize, u32, String)> = styles
        .iter()
        .enumerate()
        .filter_map(|(index, style)| {
            custom_number_format(index, style).map(|(id, code)| (index, id, code))
        })
        .collect();
    if !custom_formats.is_empty() {
        xml.push_str(&format!("<numFmts count=\"{}\">", custom_formats.len()));
        for (_, id, code) in &custom_formats {
            xml.push_str(&format!(
                "<numFmt numFmtId=\"{id}\" formatCode=\"{}\"/>",
                escape_attribute(code)
            ));
        }
        xml.push_str("</numFmts>");
    }
    xml.push_str(&format!("<fonts count=\"{}\">", styles.len()));
    for style in styles {
        xml.push_str("<font>");
        if style.bold == Some(true) {
            xml.push_str("<b/>");
        }
        if style.italic == Some(true) {
            xml.push_str("<i/>");
        }
        if let Some(size) = style.font_size {
            xml.push_str(&format!("<sz val=\"{}\"/>", size));
        }
        if let Some(color) = &style.text_color {
            xml.push_str(&format!("<color rgb=\"{}\"/>", argb(color)));
        }
        xml.push_str("</font>");
    }
    xml.push_str("</fonts>");
    xml.push_str(&format!("<fills count=\"{}\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill>", styles.len() + 2));
    for style in styles {
        if let Some(color) = &style.fill {
            xml.push_str(&format!("<fill><patternFill patternType=\"solid\"><fgColor rgb=\"{}\"/><bgColor indexed=\"64\"/></patternFill></fill>", argb(color)));
        } else {
            xml.push_str("<fill><patternFill patternType=\"none\"/></fill>");
        }
    }
    xml.push_str("</fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>");
    xml.push_str(&format!("<cellXfs count=\"{}\">", styles.len()));
    for (index, style) in styles.iter().enumerate() {
        let num_fmt = custom_formats
            .iter()
            .find(|(style_index, _, _)| *style_index == index)
            .map_or_else(|| built_in_number_format(style), |(_, id, _)| *id);
        let fill_id = if style.fill.is_some() { index + 2 } else { 0 };
        xml.push_str(&format!("<xf numFmtId=\"{num_fmt}\" fontId=\"{index}\" fillId=\"{fill_id}\" borderId=\"0\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyNumberFormat=\"1\""));
        let alignment = alignment_xml(style);
        if alignment.is_empty() {
            xml.push_str("/>");
        } else {
            xml.push_str(" applyAlignment=\"1\">");
            xml.push_str(&alignment);
            xml.push_str("</xf>");
        }
    }
    xml.push_str("</cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>");
    xml.into_bytes()
}

fn custom_number_format(index: usize, style: &StyleProperties) -> Option<(u32, String)> {
    custom_number_format_code(style)
        .map(|code| (164 + u32::try_from(index).unwrap_or(u32::MAX), code))
}

/// Renders the `numFmt` format code this exporter emits for a style, or `None`
/// when the style is carried by a built-in `numFmtId` instead. The importer
/// reuses it to decide whether a source format survives the projection.
pub(super) fn custom_number_format_code(style: &StyleProperties) -> Option<String> {
    match style.number {
        Some(NumberFormat::Currency) => Some(format!(
            "[$-{}]#,##0{}",
            style.currency.as_deref().unwrap_or("USD"),
            fraction_pattern(style.decimals.unwrap_or(2))
        )),
        Some(NumberFormat::Decimal | NumberFormat::Percent) if style.decimals.is_some() => {
            Some(format!(
                "0{}{}",
                fraction_pattern(style.decimals.unwrap_or(0)),
                if style.number == Some(NumberFormat::Percent) {
                    "%"
                } else {
                    ""
                }
            ))
        }
        _ => None,
    }
}

fn fraction_pattern(decimals: u8) -> String {
    if decimals == 0 {
        String::new()
    } else {
        format!(".{}", "0".repeat(usize::from(decimals)))
    }
}

pub(super) fn built_in_number_format(style: &StyleProperties) -> u32 {
    match style.number {
        None | Some(NumberFormat::General) => 0,
        Some(NumberFormat::Integer) => 1,
        Some(NumberFormat::Decimal) => 2,
        Some(NumberFormat::Percent) => 10,
        Some(NumberFormat::Currency) => 4,
        Some(NumberFormat::Date) => 14,
        Some(NumberFormat::DateTime) => 22,
    }
}

fn alignment_xml(style: &StyleProperties) -> String {
    let horizontal = style.align.map(|value| match value {
        HorizontalAlignment::Left => "left",
        HorizontalAlignment::Center => "center",
        HorizontalAlignment::Right => "right",
        HorizontalAlignment::General => "general",
    });
    let vertical = style.valign.map(|value| match value {
        VerticalAlignment::Top => "top",
        VerticalAlignment::Middle => "center",
        VerticalAlignment::Bottom => "bottom",
    });
    if horizontal.is_none() && vertical.is_none() && style.wrap.is_none() {
        return String::new();
    }
    format!(
        "<alignment{}{}{} />",
        horizontal.map_or_else(String::new, |value| format!(" horizontal=\"{value}\"")),
        vertical.map_or_else(String::new, |value| format!(" vertical=\"{value}\"")),
        style.wrap.map_or_else(String::new, |value| format!(
            " wrapText=\"{}\"",
            u8::from(value)
        ))
    )
}

fn argb(color: &Color) -> String {
    let value = color.as_str().trim_start_matches('#');
    if value.len() == 6 {
        format!("FF{}", value.to_ascii_uppercase())
    } else {
        // Marksheet is RRGGBBAA; SpreadsheetML is AARRGGBB.
        format!("{}{}", &value[6..8], &value[..6]).to_ascii_uppercase()
    }
}

fn export_sheet_labels(workbook: &ProjectedWorkbook, report: &mut ConversionReport) -> Vec<String> {
    let mut used = BTreeSet::new();
    let mut derived_ids = BTreeSet::new();
    let mut labels = Vec::new();
    for (index, sheet) in workbook.sheets.iter().enumerate() {
        let mut label = sheet.label.clone();
        if !valid_sheet_label(&label) || used.contains(&label.to_ascii_lowercase()) {
            label = sanitized_sheet_label(&sheet.label, index + 1, &used);
            report.approximate(
                ConversionEvent::new(
                    ConversionFeature::Sheet,
                    format!("sheet label {:?} was exported as {label:?}", sheet.label),
                )
                .at(ConversionLocation::Sheet {
                    sheet: sheet.id.clone(),
                }),
            );
        }
        let derived = derived_sheet_id(&label, &mut derived_ids);
        if derived != sheet.id.as_str() {
            report.approximate(
                ConversionEvent::new(
                    ConversionFeature::Sheet,
                    format!(
                        "stable sheet ID {} is not representable; importing label {label:?} derives {derived}",
                        sheet.id
                    ),
                )
                .at(ConversionLocation::Sheet {
                    sheet: sheet.id.clone(),
                }),
            );
        }
        used.insert(label.to_ascii_lowercase());
        labels.push(label);
    }
    labels
}

fn derived_sheet_id(label: &str, used: &mut BTreeSet<String>) -> String {
    let mut base = String::new();
    for character in label.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_' {
            base.push(character);
        } else if !base.ends_with('_') {
            base.push('_');
        }
    }
    base = base.trim_matches('_').to_owned();
    if !base.starts_with(|character: char| character.is_ascii_lowercase()) {
        base = format!("x_{base}");
    }
    if base == "x_" || base.is_empty() {
        "sheet".clone_into(&mut base);
    }
    let mut candidate = base.clone();
    for suffix in 2_u64.. {
        if used.insert(candidate.clone()) {
            return candidate;
        }
        candidate = format!("{base}_{suffix}");
    }
    unreachable!("unbounded identifier suffix space")
}

fn export_table_names(
    workbook: &ProjectedWorkbook,
    report: &mut ConversionReport,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let mut used = BTreeSet::new();
    let mut ordinal = 1;
    for sheet in &workbook.sheets {
        for table in &sheet.tables {
            let source = table.id.to_string();
            let mut candidate = source.clone();
            if !valid_defined_name(&candidate) || used.contains(&candidate.to_ascii_lowercase()) {
                candidate = format!("ms_table_{ordinal}");
                report.approximate(
                    ConversionEvent::new(
                        ConversionFeature::Table,
                        format!("table {source} was exported as {candidate}"),
                    )
                    .at(ConversionLocation::table(table.id.clone())),
                );
            }
            used.insert(candidate.to_ascii_lowercase());
            result.insert(source, candidate);
            ordinal += 1;
        }
    }
    result
}

fn export_name_names(
    workbook: &Workbook,
    report: &mut ConversionReport,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let mut used = BTreeSet::new();
    for (ordinal, name) in workbook.names.iter().enumerate() {
        let source = name.id.to_string();
        let mut candidate = source.clone();
        if !valid_defined_name(&candidate) || used.contains(&candidate.to_ascii_lowercase()) {
            candidate = format!("ms_name_{}", ordinal + 1);
            report.approximate(
                ConversionEvent::new(
                    ConversionFeature::Name,
                    format!("name {source} was rewritten as {candidate}"),
                )
                .at(name.origin.map_or_else(
                    || ConversionLocation::source(format!("@name {}", name.id)),
                    |origin| ConversionLocation::source_span(origin.span),
                )),
            );
        }
        used.insert(candidate.to_ascii_lowercase());
        result.insert(source, candidate);
    }
    result
}

fn table_fill_formulas(
    sheet: &marksheet_model::Sheet,
    table: &TableId,
) -> BTreeMap<String, String> {
    sheet
        .items
        .iter()
        .filter_map(|item| {
            let SheetItem::Fill(fill) = item else {
                return None;
            };
            let FillTarget::TableColumn {
                table: target,
                header,
            } = &fill.target
            else {
                return None;
            };
            (target == table).then(|| (header.clone(), fill.formula.as_str().to_owned()))
        })
        .collect()
}

fn translate_formula(
    source: &str,
    symbols: &FormulaSymbols,
    limits: ConversionLimits,
) -> Result<String, ConvertError> {
    let parse_limits = ParseLimits {
        max_source_bytes: limits.max_string_bytes,
        max_tokens: limits.max_string_bytes.min(100_000),
        max_depth: limits.max_xml_depth.max(1),
        max_nodes: limits.max_string_bytes.min(100_000),
        max_function_arguments: limits.max_string_bytes.min(10_000),
    };
    let formula = parse_formula(source, &parse_limits).map_err(|error| {
        ConvertError::new(
            ConvertErrorCode::InvalidWorkbook,
            format!("formula is not valid portable-a1@1: {error}"),
        )
    })?;
    validate_formula_expression(&formula.expression).map_err(|error| {
        ConvertError::new(ConvertErrorCode::UnsupportedPackage, error.to_string())
    })?;
    let mut replacements = Vec::new();
    collect_reference_replacements(&formula.expression, symbols, &mut replacements)?;
    replacements.sort_by_key(|(start, _, _)| *start);
    for pair in replacements.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(ConvertError::new(
                ConvertErrorCode::Internal,
                "formula reference spans overlap",
            ));
        }
    }
    let mut translated = source.to_owned();
    for (start, end, replacement) in replacements.into_iter().rev() {
        if end > translated.len()
            || start > end
            || !translated.is_char_boundary(start)
            || !translated.is_char_boundary(end)
        {
            return Err(ConvertError::new(
                ConvertErrorCode::Internal,
                "formula reference span is outside its source",
            ));
        }
        translated.replace_range(start..end, &replacement);
    }
    translated
        .strip_prefix('=')
        .map(str::to_owned)
        .ok_or_else(|| {
            ConvertError::new(
                ConvertErrorCode::InvalidWorkbook,
                "formula source is missing its leading equals sign",
            )
        })
}

fn collect_reference_replacements(
    expression: &Expr,
    symbols: &FormulaSymbols,
    output: &mut Vec<(usize, usize, String)>,
) -> Result<(), ConvertError> {
    match &expression.kind {
        ExprKind::Reference { reference } => {
            let start = usize::try_from(expression.span.start).map_err(|_| {
                ConvertError::new(ConvertErrorCode::Internal, "formula span exceeds usize")
            })?;
            let end = usize::try_from(expression.span.end).map_err(|_| {
                ConvertError::new(ConvertErrorCode::Internal, "formula span exceeds usize")
            })?;
            output.push((start, end, render_excel_reference(reference, symbols)?));
        }
        ExprKind::Unary { operand, .. } => {
            collect_reference_replacements(operand, symbols, output)?;
        }
        ExprKind::Binary { left, right, .. } => {
            collect_reference_replacements(left, symbols, output)?;
            collect_reference_replacements(right, symbols, output)?;
        }
        ExprKind::Call { call } => {
            for argument in &call.arguments {
                collect_reference_replacements(argument, symbols, output)?;
            }
        }
        ExprKind::Literal { .. } => {}
    }
    Ok(())
}

fn render_excel_reference(
    reference: &Reference,
    symbols: &FormulaSymbols,
) -> Result<String, ConvertError> {
    match reference {
        Reference::Cell { sheet, address } => {
            check_reference_coordinate(address.coordinate)?;
            Ok(format!(
                "{}{}",
                sheet
                    .as_ref()
                    .map(|sheet| excel_sheet_prefix(sheet.as_str(), symbols))
                    .transpose()?
                    .unwrap_or_default(),
                render_a1(address)
            ))
        }
        Reference::Range(range) => {
            check_reference_coordinate(range.start.coordinate)?;
            check_reference_coordinate(range.end.coordinate)?;
            Ok(format!(
                "{}{}:{}",
                range
                    .sheet
                    .as_ref()
                    .map(|sheet| excel_sheet_prefix(sheet.as_str(), symbols))
                    .transpose()?
                    .unwrap_or_default(),
                render_a1(&range.start),
                render_a1(&range.end)
            ))
        }
        Reference::Name { name } => symbols.name_names.get(name.as_str()).cloned().map_or_else(
            || {
                // An undeclared name has no defined name to point at, but its
                // own spelling is a valid Excel name, and Excel resolves it to
                // `#NAME?` -- exactly the value SPEC section 13 requires.
                // Writing it through is therefore faithful, not lossy.
                if valid_defined_name(name.as_str()) {
                    Ok(name.as_str().to_owned())
                } else {
                    Err(invalid_workbook(format!(
                        "formula refers to unresolved name {name}"
                    )))
                }
            },
            Ok,
        ),
        Reference::Structured(reference) => match reference {
            StructuredReference::Column { table, header } => Ok(format!(
                "{}[{}]",
                mapped_table_name(table, symbols)?,
                header.replace(']', "]]"),
            )),
            StructuredReference::Region { table, region } => Ok(format!(
                "{}[{}]",
                mapped_table_name(table, symbols)?,
                match region {
                    FormulaTableRegion::Headers => "#Headers",
                    FormulaTableRegion::Data => "#Data",
                }
            )),
            StructuredReference::CurrentRow { table, header } => Ok(format!(
                "{}[@{}]",
                table
                    .as_ref()
                    .map(|table| mapped_table_name(table, symbols))
                    .transpose()?
                    .unwrap_or_default(),
                header.replace(']', "]]"),
            )),
        },
    }
}

fn mapped_table_name<'a>(
    table: &TableId,
    symbols: &'a FormulaSymbols,
) -> Result<&'a str, ConvertError> {
    symbols
        .table_names
        .get(table.as_str())
        .map(String::as_str)
        .ok_or_else(|| invalid_workbook(format!("formula refers to unresolved table {table}")))
}

fn excel_sheet_prefix(sheet: &str, symbols: &FormulaSymbols) -> Result<String, ConvertError> {
    symbols
        .sheet_labels
        .get(sheet)
        .map(|label| format!("{}!", quote_sheet(label)))
        .ok_or_else(|| invalid_workbook(format!("formula refers to unresolved sheet {sheet}")))
}

fn render_a1(reference: &A1Reference) -> String {
    format!(
        "{}{}{}{}",
        if reference.column_absolute { "$" } else { "" },
        reference.coordinate.column_name(),
        if reference.row_absolute { "$" } else { "" },
        reference.coordinate.row
    )
}

fn check_reference_coordinate(coordinate: Coordinate) -> Result<(), ConvertError> {
    if coordinate.column > XLSX_MAX_COLUMN || coordinate.row > XLSX_MAX_ROW {
        return Err(ConvertError::new(
            ConvertErrorCode::UnsupportedPackage,
            format!("reference {coordinate} exceeds the XLSX grid"),
        ));
    }
    Ok(())
}

fn valid_sheet_label(label: &str) -> bool {
    !label.is_empty()
        && label.chars().count() <= 31
        && !label
            .chars()
            .any(|character| "[]:*?/\\".contains(character))
        && label.chars().all(is_xml_character)
        && !label.starts_with('\'')
        && !label.ends_with('\'')
}

fn sanitized_sheet_label(label: &str, ordinal: usize, used: &BTreeSet<String>) -> String {
    let mut base: String = label
        .chars()
        .filter(|character| {
            !"[]:*?/\\".contains(*character) && *character != '\'' && is_xml_character(*character)
        })
        .take(24)
        .collect();
    if base.is_empty() {
        "Sheet".clone_into(&mut base);
    }
    for suffix in ordinal.. {
        let candidate = format!("{base}_{suffix}");
        if candidate.chars().count() <= 31 && !used.contains(&candidate.to_ascii_lowercase()) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix space")
}

fn valid_defined_name(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.'))
        // Shares the importer's and serializer's single definition, so a name
        // that survived import is not renamed on the way back out.
        && !marksheet_model::resembles_cell_address(value)
        && !matches!(value.to_ascii_lowercase().as_str(), "r" | "c")
}

fn quote_sheet(label: &str) -> String {
    format!("'{}'", label.replace('\'', "''"))
}

fn absolute_coordinate(coordinate: Coordinate) -> String {
    format!("${}${}", coordinate.column_name(), coordinate.row)
}

fn absolute_range(range: marksheet_model::Range) -> String {
    if range.start == range.end {
        absolute_coordinate(range.start)
    } else {
        format!(
            "{}:{}",
            absolute_coordinate(range.start),
            absolute_coordinate(range.end)
        )
    }
}

fn number(value: f64) -> Result<String, ConvertError> {
    canonical_number(value).map_err(|error| invalid_workbook(error.to_string()))
}

fn record_exact_features(
    source: &Workbook,
    projected: &ProjectedWorkbook,
    report: &mut ConversionReport,
) {
    report.exact_event(ConversionEvent::new(
        ConversionFeature::Sheet,
        "sheet order and labels were represented in workbook.xml",
    ));
    report.exact_event(ConversionEvent::new(
        ConversionFeature::Cell,
        "sparse scalar cells were represented without a dense used range",
    ));
    if projected.sheets.iter().any(|sheet| {
        sheet
            .cells
            .values()
            .any(|cell| matches!(cell.value, Value::Formula(_)))
    }) {
        report.exact_event(
            ConversionEvent::new(
                ConversionFeature::Formula,
                "portable formulas were translated to OOXML formula elements",
            )
            .formula(FormulaDisposition::Translated),
        );
    }
    for table in projected.sheets.iter().flat_map(|sheet| &sheet.tables) {
        report.exact_event(
            ConversionEvent::new(
                ConversionFeature::Table,
                "table rectangle and ordered headers were represented as a native XLSX table",
            )
            .at(ConversionLocation::table(table.id.clone())),
        );
    }
    for name in &source.names {
        if valid_defined_name(name.id.as_str()) {
            report.exact_event(
                ConversionEvent::new(
                    ConversionFeature::Name,
                    "single-target workbook name was represented as a defined name",
                )
                .at(name.origin.map_or_else(
                    || ConversionLocation::source(format!("@name {}", name.id)),
                    |origin| ConversionLocation::source_span(origin.span),
                )),
            );
        }
    }
    if !source.styles.is_empty() {
        report.exact_event(ConversionEvent::new(
            ConversionFeature::Style,
            "resolved core style properties were represented as cell formats",
        ));
    }
    if source.sheets.iter().any(|sheet| {
        sheet.items.iter().any(|item| {
            matches!(
                item,
                SheetItem::ColumnGeometry(_) | SheetItem::RowGeometry(_)
            )
        })
    }) {
        report.exact_event(ConversionEvent::new(
            ConversionFeature::ColumnWidth,
            "effective row heights and column width runs were represented",
        ));
    }
}

fn record_omissions(source: &Workbook, report: &mut ConversionReport) {
    for declaration in &source.extensions {
        let directive = if declaration.required {
            "@require"
        } else {
            "@use"
        };
        report.omit(
            ConversionEvent::new(
                ConversionFeature::Other(format!(
                    "extension.{}",
                    declaration.capability.id.as_str()
                )),
                format!(
                    "{directive} {}@{} declaration has no OOXML representation",
                    declaration.capability.id.as_str(),
                    declaration.capability.major
                ),
            )
            .at(declaration.origin.map_or_else(
                || {
                    ConversionLocation::source(format!(
                        "{directive} {}@{}",
                        declaration.capability.id.as_str(),
                        declaration.capability.major
                    ))
                },
                |origin| ConversionLocation::source_span(origin.span),
            )),
        );
    }
    for sheet in &source.sheets {
        for item in &sheet.items {
            let SheetItem::Fill(fill) = item else {
                continue;
            };
            let FillTarget::Range(range) = fill.target else {
                continue;
            };
            report.approximate(
                ConversionEvent::new(
                    ConversionFeature::Formula,
                    "coordinate @fill was expanded into destination-specific cell formulas",
                )
                .formula(FormulaDisposition::Translated)
                .at(ConversionLocation::range(sheet.id.clone(), range)),
            );
        }
    }
    for extension in source
        .extension_instances
        .iter()
        .chain(source.sheets.iter().flat_map(|sheet| {
            sheet.items.iter().filter_map(|item| match item {
                SheetItem::Extension(extension) => Some(extension),
                _ => None,
            })
        }))
    {
        report.omit(
            ConversionEvent::new(
                ConversionFeature::Other(format!("extension.{}", extension.capability.id.as_str())),
                format!(
                    "{} are outside the initial XLSX profile",
                    extension.capability.id.as_str()
                ),
            )
            .at(ConversionLocation::source(format!(
                "@extension {}@{} \"{}\"",
                extension.capability.id.as_str(),
                extension.capability.major,
                extension.name
            ))),
        );
    }
    if source.settings != marksheet_model::WorkbookSettings::default() {
        report.omit(ConversionEvent::new(ConversionFeature::WorkbookSettings, "non-default workbook locale, timezone, or formula profile has no native OOXML representation"));
    }
}

fn write_zip(
    parts: Vec<(String, Vec<u8>)>,
    limits: ConversionLimits,
) -> Result<Vec<u8>, ConvertError> {
    let total = parts.iter().try_fold(0_u64, |total, (_, bytes)| {
        total
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| resource("OOXML part size overflow"))
    })?;
    if total > limits.max_zip_total_uncompressed_bytes {
        return Err(resource(
            "generated OOXML parts exceed the uncompressed limit",
        ));
    }
    for (name, bytes) in &parts {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limits.max_zip_entry_uncompressed_bytes
        {
            return Err(resource(format!(
                "generated part {name} exceeds the per-entry limit"
            )));
        }
    }
    let cursor = BoundedCursor::new(limits.max_output_bytes);
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::DEFAULT
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .unix_permissions(0o644)
        .large_file(false);
    for (name, bytes) in parts {
        writer.start_file(name, options).map_err(zip_error)?;
        writer.write_all(&bytes).map_err(|error| {
            ConvertError::new(
                ConvertErrorCode::Internal,
                format!("cannot write XLSX part: {error}"),
            )
        })?;
    }
    writer.finish().map_err(zip_error)?.into_result()
}

struct BoundedCursor {
    data: Vec<u8>,
    position: u64,
    virtual_len: u64,
    limit: u64,
    overflowed: bool,
}

impl BoundedCursor {
    fn new(limit: u64) -> Self {
        Self {
            data: Vec::new(),
            position: 0,
            virtual_len: 0,
            limit,
            overflowed: false,
        }
    }

    fn into_result(self) -> Result<Vec<u8>, ConvertError> {
        if self.overflowed {
            Err(ConvertError::new(
                ConvertErrorCode::OutputLimit,
                "generated XLSX exceeds the configured output limit",
            ))
        } else {
            Ok(self.data)
        }
    }
}

impl Write for BoundedCursor {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let end = self.position.saturating_add(byte_count);
        if end > self.limit {
            self.overflowed = true;
        }
        let stored_end = end.min(self.limit);
        if self.position < stored_end {
            let start = usize::try_from(self.position).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "XLSX output position exceeds usize",
                )
            })?;
            let finish = usize::try_from(stored_end).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "XLSX output position exceeds usize",
                )
            })?;
            if self.data.len() < finish {
                self.data.resize(finish, 0);
            }
            self.data[start..finish].copy_from_slice(&bytes[..finish - start]);
        }
        self.position = end;
        self.virtual_len = self.virtual_len.max(end);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for BoundedCursor {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => i128::from(self.virtual_len) + i128::from(value),
        };
        let next = u64::try_from(next)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid XLSX output seek"))?;
        if next > self.limit {
            self.overflowed = true;
        }
        self.position = next;
        Ok(next)
    }
}

#[allow(clippy::needless_pass_by_value)] // `map_err` supplies an owned error.
fn zip_error(error: zip::result::ZipError) -> ConvertError {
    ConvertError::new(
        ConvertErrorCode::Internal,
        format!("cannot construct XLSX ZIP: {error}"),
    )
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
    use marksheet_model::{
        Apply, ApplyTarget, Block, Cell, ExtensionDeclaration, ExtensionId, Fill, FormulaSource,
        Name, NameId, Range, RowGeometry, RowRange, Sheet, SheetCoordinate, SheetId, Style,
        StyleId, Table, TableId,
    };

    fn workbook() -> Workbook {
        Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![vec![
                            Cell::new(Value::Text("hello".to_owned())),
                            Cell::new(Value::Number(2.0)),
                        ]],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        }
    }

    #[test]
    fn export_is_byte_deterministic() {
        let first = export_xlsx(&workbook(), ConversionLimits::default()).unwrap();
        let second = export_xlsx(&workbook(), ConversionLimits::default()).unwrap();
        assert_eq!(first.value, second.value);
        assert!(first.report.is_lossless());
    }

    #[test]
    fn coordinate_fill_is_reported_as_an_approximation() {
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![
                    SheetItem::Block(
                        Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![
                                vec![Cell::new(Value::Number(2.0)), Cell::new(Value::Blank)],
                                vec![Cell::new(Value::Number(3.0)), Cell::new(Value::Blank)],
                            ],
                        )
                        .unwrap(),
                    ),
                    SheetItem::Fill(Fill {
                        target: FillTarget::Range(Range::parse("B1:B2").unwrap()),
                        formula: FormulaSource::new("=A1*2").unwrap(),
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };

        let conversion = export_xlsx(&source, ConversionLimits::default()).unwrap();
        assert!(!conversion.report.is_lossless());
        assert!(conversion.report.outcomes().iter().any(|event| {
            event.outcome == crate::FeatureOutcome::Approximated
                && event.feature == "portable_formulas"
        }));
    }

    #[test]
    fn rejects_formula_and_name_references_outside_the_xlsx_grid() {
        let mut formula_workbook = workbook();
        if let SheetItem::Block(block) = &mut formula_workbook.sheets[0].items[0] {
            block.cells[0][0].value =
                Value::Formula(FormulaSource::new("=XFE1").expect("valid portable formula"));
        }
        assert_eq!(
            export_xlsx(&formula_workbook, ConversionLimits::default())
                .unwrap_err()
                .code,
            ConvertErrorCode::UnsupportedPackage
        );

        if let SheetItem::Block(block) = &mut formula_workbook.sheets[0].items[0] {
            block.cells[0][0].value =
                Value::Formula(FormulaSource::new("=XLOOKUP(A1,A1:A2,B1:B2)").unwrap());
        }
        assert_eq!(
            export_xlsx(&formula_workbook, ConversionLimits::default())
                .unwrap_err()
                .code,
            ConvertErrorCode::UnsupportedPackage
        );

        if let SheetItem::Block(block) = &mut formula_workbook.sheets[0].items[0] {
            block.cells[0][0].value = Value::Formula(FormulaSource::new("=IF(TRUE,1)").unwrap());
        }
        let failure = export_xlsx(&formula_workbook, ConversionLimits::default())
            .expect_err("known function with evaluator-incompatible arity must be rejected");
        assert_eq!(failure.code, ConvertErrorCode::UnsupportedPackage);
        assert!(failure.message.contains("exactly 3 arguments"));

        let mut name_workbook = workbook();
        name_workbook.names.push(Name {
            id: NameId::parse("too_far").unwrap(),
            target: NameTarget::Cell(SheetCoordinate {
                sheet: SheetId::parse("data").unwrap(),
                coordinate: Coordinate::parse("XFE1").unwrap(),
            }),
            origin: None,
        });
        assert_eq!(
            export_xlsx(&name_workbook, ConversionLimits::default())
                .unwrap_err()
                .code,
            ConvertErrorCode::UnsupportedPackage
        );
    }

    #[test]
    fn unrepresentable_sheet_identity_and_extension_declaration_are_lossy() {
        let mut source = workbook();
        source.sheets[0].id = SheetId::parse("s").unwrap();
        source.sheets[0].label = "Budget".to_owned();
        source.extensions.push(ExtensionDeclaration {
            capability: ExtensionId::parse("charts@1").unwrap(),
            required: false,
            origin: None,
        });

        let conversion = export_xlsx(&source, ConversionLimits::default()).unwrap();
        assert!(!conversion.report.is_lossless());
        assert!(conversion.report.outcomes().iter().any(|event| {
            event.feature == "sheets" && event.outcome == crate::FeatureOutcome::Approximated
        }));
        assert!(conversion.report.outcomes().iter().any(|event| {
            event.feature == "extension.charts" && event.outcome == crate::FeatureOutcome::Omitted
        }));
    }

    #[test]
    fn output_limit_stops_the_zip_sink_and_carries_ms4101() {
        let limits = ConversionLimits {
            max_output_bytes: 1,
            ..ConversionLimits::default()
        };
        let failure = export_xlsx(&workbook(), limits).unwrap_err();
        assert_eq!(failure.error.code, ConvertErrorCode::OutputLimit);
        assert_eq!(failure.report.fidelity(), crate::Fidelity::Unsupported);
        assert!(
            failure
                .report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "MS4101")
        );
    }

    #[test]
    fn aggregate_style_only_coordinates_obey_the_cell_limit() {
        let style_id = StyleId::parse("emphasis").unwrap();
        let source = Workbook {
            styles: vec![Style {
                id: style_id.clone(),
                properties: StyleProperties {
                    bold: Some(true),
                    ..StyleProperties::default()
                },
                origin: None,
            }],
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![
                    SheetItem::Apply(Apply {
                        target: ApplyTarget::Range(Range::single(Coordinate::parse("A1").unwrap())),
                        styles: vec![style_id.clone()],
                        origin: None,
                    }),
                    SheetItem::Apply(Apply {
                        target: ApplyTarget::Range(Range::single(Coordinate::parse("B1").unwrap())),
                        styles: vec![style_id],
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };
        let limits = ConversionLimits {
            max_cells: 1,
            ..ConversionLimits::default()
        };
        assert_eq!(
            export_xlsx(&source, limits).unwrap_err().error.code,
            ConvertErrorCode::ResourceLimit
        );
    }

    #[test]
    fn row_geometry_expansion_is_bounded_across_sheets_before_xml_allocation() {
        let source = Workbook {
            sheets: vec![
                Sheet {
                    id: SheetId::parse("first").unwrap(),
                    label: "First".to_owned(),
                    items: vec![SheetItem::RowGeometry(RowGeometry {
                        rows: RowRange::new(1, 2).unwrap(),
                        height: 18.0,
                        origin: None,
                    })],
                    origin: None,
                },
                Sheet {
                    id: SheetId::parse("second").unwrap(),
                    label: "Second".to_owned(),
                    items: vec![SheetItem::RowGeometry(RowGeometry {
                        rows: RowRange::new(1, 2).unwrap(),
                        height: 18.0,
                        origin: None,
                    })],
                    origin: None,
                },
            ],
            ..Workbook::default()
        };
        let limits = ConversionLimits {
            max_cells: 2,
            ..ConversionLimits::default()
        };

        let failure = export_xlsx(&source, limits)
            .expect_err("two independently valid geometry ranges must share one work budget");
        assert_eq!(failure.error.code, ConvertErrorCode::ResourceLimit);
        assert!(failure.error.message.contains("aggregate row geometry"));
    }

    #[test]
    fn row_geometry_respects_low_output_construction_budget() {
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::RowGeometry(RowGeometry {
                    rows: RowRange::new(1, 2).unwrap(),
                    height: 18.0,
                    origin: None,
                })],
                origin: None,
            }],
            ..Workbook::default()
        };
        let limits = ConversionLimits {
            max_output_bytes: 64,
            ..ConversionLimits::default()
        };

        let failure = export_xlsx(&source, limits).expect_err(
            "geometry must be rejected before building an oversized worksheet XML part",
        );
        assert_eq!(failure.error.code, ConvertErrorCode::ResourceLimit);
        assert!(failure.error.message.contains("row geometry expansion"));
    }

    #[test]
    fn table_limit_is_workbook_global() {
        let table = |sheet: &str, table: &str| Sheet {
            id: SheetId::parse(sheet).unwrap(),
            label: sheet.to_owned(),
            items: vec![SheetItem::Table(Table {
                id: TableId::parse(table).unwrap(),
                block: Block::new(
                    Coordinate::parse("A1").unwrap(),
                    vec![vec![Cell::new(Value::Text("header".to_owned()))]],
                )
                .unwrap(),
                origin: None,
            })],
            origin: None,
        };
        let source = Workbook {
            sheets: vec![table("first", "one"), table("second", "two")],
            ..Workbook::default()
        };
        let limits = ConversionLimits {
            max_tables: 1,
            ..ConversionLimits::default()
        };

        let failure = export_xlsx(&source, limits)
            .expect_err("table limits apply to the whole workbook, not each sheet");
        assert_eq!(failure.error.code, ConvertErrorCode::ResourceLimit);
        assert!(failure.error.message.contains("workbook table count"));
    }

    #[test]
    fn characters_xml_cannot_represent_are_reported_rather_than_written() {
        let mut label_workbook = workbook();
        label_workbook.sheets[0].label = "Bell\u{7}Label".to_owned();
        let conversion = export_xlsx(&label_workbook, ConversionLimits::default())
            .expect("an unrepresentable label is sanitized, not silently emitted");
        assert!(conversion.report.outcomes().iter().any(|event| {
            event.outcome == crate::FeatureOutcome::Approximated
                && event.feature == "sheets"
                && event
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.contains("was exported as"))
        }));
        let imported = crate::import_xlsx(&conversion.value, ConversionLimits::default())
            .expect("the sanitized package is still well-formed XML");
        assert!(!imported.value.sheets[0].label.contains('\u{7}'));

        let mut text_workbook = workbook();
        if let SheetItem::Block(block) = &mut text_workbook.sheets[0].items[0] {
            block.cells[0][0].value = Value::Text("Bell\u{7}Text".to_owned());
        }
        let failure = export_xlsx(&text_workbook, ConversionLimits::default())
            .expect_err("XML has no spelling for a C0 control other than tab, LF, and CR");
        assert_eq!(failure.error.code, ConvertErrorCode::UnsupportedPackage);
        assert!(failure.error.message.contains("control character"));
    }
}
