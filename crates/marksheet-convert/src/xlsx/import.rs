#![allow(clippy::too_many_lines)] // Streaming state machines mirror workbook, style, and sheet XML.

use std::collections::{BTreeMap, BTreeSet};

use marksheet_calc::formula::{Expr, ExprKind, ParseLimits, parse as parse_formula};
use marksheet_model::{
    Apply, ApplyTarget, Block, Cell, CellError, Color, ColumnGeometry, ColumnRange, Coordinate,
    Fill, FillTarget, FormulaSource, HorizontalAlignment, Name, NameId, NameTarget, NumberFormat,
    Range, RowGeometry, RowRange, Sheet, SheetCoordinate, SheetId, SheetItem, SheetRange, Style,
    StyleId, StyleProperties, Table, TableId, Value, VerticalAlignment, Workbook,
};
use quick_xml::{Reader, escape::unescape, events::Event, name::ResolveResult, reader::NsReader};
use time::{Date, Duration, Month, PrimitiveDateTime, Time};

use crate::{
    Conversion, ConversionEvent, ConversionFailure, ConversionFeature, ConversionLimits,
    ConversionLocation, ConversionReport, ConversionResult, ConvertError, ConvertErrorCode,
    FormatDescriptor, FormulaDisposition,
    formula_profile::{FormulaProfileError, validate_formula_expression},
};

use super::{
    package::{Package, relationships_part},
    xml::{
        attribute, invalid, is_xml_character, local_name, required_attribute, resource,
        xlsx_location,
    },
};

#[derive(Clone, Debug)]
struct WorkbookSheet {
    label: String,
    relationship: String,
}

#[derive(Clone, Debug)]
struct DefinedName {
    name: String,
    expression: String,
}

#[derive(Clone, Debug, Default)]
struct WorkbookInfo {
    sheets: Vec<WorkbookSheet>,
    names: Vec<DefinedName>,
    date_1904: bool,
    omitted_features: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct ImportedCell {
    value: Value,
    style: usize,
}

#[derive(Clone, Debug, Default)]
struct WorksheetData {
    cells: BTreeMap<Coordinate, ImportedCell>,
    /// Style-only OOXML cell records do not become authored Marksheet blanks.
    style_only: BTreeMap<Coordinate, usize>,
    columns: Vec<ColumnGeometry>,
    rows: Vec<RowGeometry>,
    table_relationships: Vec<String>,
    omitted_features: BTreeSet<String>,
    formula_count: u64,
}

#[derive(Clone, Debug)]
struct ImportedTable {
    display_name: String,
    range: Range,
    headers: Vec<String>,
    calculated: BTreeMap<String, String>,
    omitted_features: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct FillBuilder {
    color: Option<Color>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum StyleSection {
    None,
    NumFmts,
    Fonts,
    Fills,
    CellStyleXfs,
    CellXfs,
    CellStyles,
}

const SPREADSHEETML_NS: &[u8] = b"http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const STRICT_SPREADSHEETML_NS: &[u8] = b"http://purl.oclc.org/ooxml/spreadsheetml/main";
const OFFICE_RELATIONSHIPS_NS: &[u8] =
    b"http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const STRICT_OFFICE_RELATIONSHIPS_NS: &[u8] =
    b"http://purl.oclc.org/ooxml/officeDocument/relationships";
const XML_NS: &[u8] = b"http://www.w3.org/XML/1998/namespace";

/// Imports supported OOXML into source-independent Marksheet IR.
///
/// The ZIP package and every XML part are fully validated against configured
/// budgets before semantic data is exposed. Unsupported safe parts (for
/// example charts and macros) are ignored with explicit lossy outcomes;
/// external relationships, encryption, and unsafe paths are rejected.
///
/// # Errors
///
/// Returns an error without a partial workbook for malformed packages,
/// external relationships, identifier/range conflicts, and exceeded limits.
pub fn import_xlsx(bytes: &[u8], limits: ConversionLimits) -> ConversionResult<Workbook> {
    import_xlsx_inner(bytes, limits).map_err(|error| {
        let feature = if error.code == ConvertErrorCode::ResourceLimit {
            if error.message.contains("uncompressed") {
                "resource_limit.uncompressed_bytes"
            } else {
                "resource_limit"
            }
        } else {
            "source_xlsx"
        };
        ConversionFailure::new(
            error,
            FormatDescriptor::xlsx(),
            FormatDescriptor::marksheet_ir(),
            feature,
        )
    })
}

fn import_xlsx_inner(
    bytes: &[u8],
    limits: ConversionLimits,
) -> Result<Conversion<Workbook>, ConvertError> {
    let package = Package::open(bytes, limits)?;
    let mut workbook_info = parse_workbook(package.xml_part("xl/workbook.xml", limits)?, limits)?;
    if workbook_info.sheets.is_empty() {
        return Err(invalid("xl/workbook.xml", "workbook contains no sheets"));
    }
    if workbook_info.sheets.len() > limits.max_sheets {
        return Err(resource(
            "xl/workbook.xml",
            "sheet count exceeds the configured limit",
        ));
    }
    reject_case_insensitive_duplicates(
        workbook_info
            .sheets
            .iter()
            .map(|sheet| sheet.label.as_str()),
        "xl/workbook.xml",
        "sheet label",
    )?;

    let mut consumed_parts = BTreeSet::from([
        "[Content_Types].xml".to_owned(),
        "_rels/.rels".to_owned(),
        "xl/workbook.xml".to_owned(),
        "xl/_rels/workbook.xml.rels".to_owned(),
    ]);

    let workbook_relationships =
        package.relationships("xl/_rels/workbook.xml.rels", "xl/workbook.xml", limits)?;
    let relationship_map: BTreeMap<_, _> = workbook_relationships
        .iter()
        .map(|relationship| (relationship.id.as_str(), relationship))
        .collect();
    let styles_part = workbook_relationships
        .iter()
        .find(|relationship| relationship.kind.ends_with("/styles"))
        .map(|relationship| relationship.target.as_str());
    let shared_part = workbook_relationships
        .iter()
        .find(|relationship| relationship.kind.ends_with("/sharedStrings"))
        .map(|relationship| relationship.target.as_str());
    let mut report =
        ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
    record_consumed_part_omissions(
        "xl/workbook.xml",
        std::mem::take(&mut workbook_info.omitted_features),
        &mut report,
    );
    let styles_part_name = styles_part.unwrap_or("xl/styles.xml");
    let style_definitions = if let Some(part) = styles_part {
        consumed_parts.insert(part.to_owned());
        let parsed = parse_styles(package.xml_part(part, limits)?, part, limits)?;
        record_consumed_part_omissions(part, parsed.unsupported, &mut report);
        record_dropped_number_formats(part, &parsed.dropped_number_formats, &mut report);
        parsed.definitions
    } else {
        vec![StyleProperties::default()]
    };
    if style_definitions.len() > limits.max_styles {
        return Err(resource(
            styles_part_name,
            "style count exceeds the configured limit",
        ));
    }
    // OOXML formats every record that names no other cell format with
    // `cellXfs[0]`, and this importer already reads it when interpreting cell
    // values. Marksheet has no workbook-wide default style, so a non-default
    // entry is materialized on each imported record rather than dropped.
    let default_format_is_significant = style_definitions
        .first()
        .is_some_and(|properties| *properties != StyleProperties::default());
    if default_format_is_significant {
        report.approximate(
            ConversionEvent::new(
                ConversionFeature::Style,
                "the XLSX default cell format was applied to every imported cell record because Marksheet has no workbook-wide default style",
            )
            .at(xlsx_location(styles_part_name, Some("cellXfs[0]"))),
        );
    }
    let shared_strings = if let Some(part) = shared_part {
        consumed_parts.insert(part.to_owned());
        let (strings, unsupported) =
            parse_shared_strings(package.xml_part(part, limits)?, part, limits)?;
        record_consumed_part_omissions(part, unsupported, &mut report);
        strings
    } else {
        Vec::new()
    };

    let sheet_ids = derive_sheet_ids(&workbook_info.sheets, &mut report);
    let sheet_label_ids: BTreeMap<String, SheetId> = workbook_info
        .sheets
        .iter()
        .zip(&sheet_ids)
        .map(|(sheet, id)| (sheet.label.to_lowercase(), id.clone()))
        .collect();
    // Defined names are numbered before any sheet is read because formula
    // bodies have to be translated to the Marksheet spelling of the name they
    // reach, and worksheets are parsed first. `import_names` reuses this
    // assignment, so the two views of a name can never disagree.
    let name_ids = assign_name_identifiers(&workbook_info.names)?;
    let formula_names = FormulaNames {
        sheets: &sheet_label_ids,
        names: &name_ids,
    };
    let mut table_ids = BTreeSet::new();
    let mut table_name_map = BTreeMap::<String, TableId>::new();
    let mut table_headers = BTreeMap::<TableId, BTreeSet<String>>::new();
    let mut fill_outcomes = FillOutcomes::new();
    let mut sheets = Vec::with_capacity(workbook_info.sheets.len());
    let mut total_cells = 0_u64;
    let mut total_formulas = 0_u64;
    let mut total_tables = 0_usize;

    for (sheet_index, source_sheet) in workbook_info.sheets.iter().enumerate() {
        let relationship = relationship_map
            .get(source_sheet.relationship.as_str())
            .ok_or_else(|| invalid("xl/workbook.xml", "sheet relationship is unresolved"))?;
        if !relationship.kind.ends_with("/worksheet") {
            return Err(invalid(
                "xl/workbook.xml",
                "sheet relationship does not target a worksheet",
            ));
        }
        let sheet_part = relationship.target.as_str();
        consumed_parts.insert(sheet_part.to_owned());
        let context = WorksheetParseContext {
            shared_strings: &shared_strings,
            styles: &style_definitions,
            sheet_id: &sheet_ids[sheet_index],
            formula_names,
            date_1904: workbook_info.date_1904,
            limits,
        };
        let mut worksheet = parse_worksheet(
            package.xml_part(sheet_part, limits)?,
            sheet_part,
            &context,
            &mut report,
        )?;
        total_formulas = total_formulas
            .checked_add(worksheet.formula_count)
            .ok_or_else(|| resource(sheet_part, "workbook formula count overflow"))?;
        if total_formulas > limits.max_formulas {
            return Err(resource(
                sheet_part,
                "workbook formula count exceeds the configured limit",
            ));
        }
        let worksheet_records = worksheet
            .cells
            .len()
            .checked_add(worksheet.style_only.len())
            .ok_or_else(|| resource(sheet_part, "worksheet cell record count overflow"))?;
        total_cells = total_cells
            .checked_add(u64::try_from(worksheet_records).unwrap_or(u64::MAX))
            .ok_or_else(|| resource(sheet_part, "workbook cell count overflow"))?;
        if total_cells > limits.max_cells {
            return Err(resource(
                sheet_part,
                "workbook cell count exceeds the configured limit",
            ));
        }

        let sheet_relationships = if worksheet.table_relationships.is_empty() {
            Vec::new()
        } else {
            let rels_part = relationships_part(sheet_part);
            consumed_parts.insert(rels_part.clone());
            package.relationships(&rels_part, sheet_part, limits)?
        };
        let sheet_relationship_map: BTreeMap<_, _> = sheet_relationships
            .iter()
            .map(|relationship| (relationship.id.as_str(), relationship))
            .collect();
        let mut imported_tables = Vec::new();
        for relationship_id in &worksheet.table_relationships {
            let relationship = sheet_relationship_map
                .get(relationship_id.as_str())
                .ok_or_else(|| invalid(sheet_part, "table relationship is unresolved"))?;
            if !relationship.kind.ends_with("/table") {
                return Err(invalid(
                    sheet_part,
                    "tablePart relationship does not target a table",
                ));
            }
            let table = parse_table(
                package.xml_part(&relationship.target, limits)?,
                &relationship.target,
                limits,
            )?;
            consumed_parts.insert(relationship.target.clone());
            imported_tables.push((table, relationship.target.clone()));
        }
        total_tables = total_tables
            .checked_add(imported_tables.len())
            .ok_or_else(|| resource(sheet_part, "workbook table count overflow"))?;
        if total_tables > limits.max_tables {
            return Err(resource(
                sheet_part,
                "workbook table count exceeds the configured limit",
            ));
        }

        let mut table_cells = BTreeSet::new();
        let mut items = Vec::new();
        for (mut table, part) in imported_tables {
            record_consumed_part_omissions(
                &part,
                std::mem::take(&mut table.omitted_features),
                &mut report,
            );
            let folded_table_name = table.display_name.to_lowercase();
            if table_name_map.contains_key(&folded_table_name) {
                return Err(invalid(
                    &part,
                    "duplicate case-insensitive table display name",
                ));
            }
            for (offset, expected) in table.headers.iter().enumerate() {
                let column = table
                    .range
                    .start
                    .column
                    .checked_add(u64::try_from(offset).unwrap_or(u64::MAX))
                    .ok_or_else(|| resource(&part, "table header coordinate overflow"))?;
                let coordinate = Coordinate {
                    column,
                    row: table.range.start.row,
                };
                let Some(ImportedCell {
                    value: Value::Text(actual),
                    ..
                }) = worksheet.cells.get(&coordinate)
                else {
                    return Err(invalid(&part, "table header row must contain text cells"));
                };
                if actual != expected {
                    return Err(invalid(
                        &part,
                        "table XML header does not match its worksheet cell",
                    ));
                }
            }
            let table_id =
                unique_identifier::<TableId>(&table.display_name, "xlsx_table", &mut table_ids)?;
            if table_id.as_str() != table.display_name {
                report.approximate(
                    ConversionEvent::new(
                        ConversionFeature::Table,
                        format!(
                            "XLSX table {:?} was assigned Marksheet ID {table_id}",
                            table.display_name
                        ),
                    )
                    .at(xlsx_location(&part, Some(&table.display_name))),
                );
            }
            table_name_map.insert(folded_table_name, table_id.clone());
            table_headers.insert(table_id.clone(), table.headers.iter().cloned().collect());
            let table_width = table
                .range
                .width()
                .map_err(|error| invalid(&part, &error.to_string()))?;
            let table_height = table
                .range
                .height()
                .map_err(|error| invalid(&part, &error.to_string()))?;
            let table_cell_count = table_width
                .checked_mul(table_height)
                .ok_or_else(|| resource(&part, "table cell count overflow"))?;
            total_cells = total_cells
                .checked_add(table_cell_count)
                .ok_or_else(|| resource(&part, "workbook cell count overflow"))?;
            if total_cells > limits.max_cells {
                return Err(resource(
                    &part,
                    "dense table materialization exceeds the workbook cell limit",
                ));
            }
            let mut cells = rectangular_cells(&worksheet.cells, table.range, limits)?;
            let mut fills = Vec::new();
            for (header, formula) in &table.calculated {
                total_formulas = total_formulas
                    .checked_add(1)
                    .ok_or_else(|| resource(&part, "workbook formula count overflow"))?;
                if total_formulas > limits.max_formulas {
                    return Err(resource(
                        &part,
                        "workbook formula count exceeds the configured limit",
                    ));
                }
                let column_index = table
                    .headers
                    .iter()
                    .position(|candidate| candidate == header)
                    .ok_or_else(|| {
                        invalid(&part, "calculated column has no matching table header")
                    })?;
                if cells.len() == 1 {
                    report.omit(
                        ConversionEvent::new(
                            ConversionFeature::Formula,
                            "header-only calculated column has no Marksheet fill destination",
                        )
                        .at(xlsx_location(&part, Some(header))),
                    );
                    continue;
                }
                let translated = translate_excel_formula(formula, formula_names);
                // A calculated-column body Marksheet cannot parse is unsupported
                // content in one column, not a broken package: the column keeps
                // the values Excel cached and only the @fill is dropped, exactly
                // as for a body that parses but leaves the portable profile.
                let parsed = match parse_portable_formula(&translated, limits) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        report.approximate(
                            ConversionEvent::new(
                                ConversionFeature::Formula,
                                format!(
                                    "calculated-column formula is outside portable-a1@1 syntax ({error}) and was not converted to @fill"
                                ),
                            )
                            .formula(FormulaDisposition::Replaced)
                            .at(xlsx_location(&part, Some(header))),
                        );
                        continue;
                    }
                };
                if !formula_references_in_xlsx_grid(&parsed.expression) {
                    return Err(invalid(
                        &part,
                        "calculated-column formula reference exceeds the XLSX grid",
                    ));
                }
                if let Err(error) = validate_formula_expression(&parsed.expression) {
                    report.approximate(
                        ConversionEvent::new(
                            ConversionFeature::Formula,
                            format!(
                                "calculated-column formula is outside the portable profile ({error}) and was not converted to @fill"
                            ),
                        )
                        .formula(FormulaDisposition::Replaced)
                        .at(xlsx_location(&part, Some(header))),
                    );
                    continue;
                }
                let formula = FormulaSource::new(translated).map_err(|error| {
                    invalid(
                        &part,
                        &format!("invalid calculated-column formula: {error}"),
                    )
                })?;
                for row in cells.iter_mut().skip(1) {
                    row[column_index] = Cell::new(Value::Blank);
                }
                fills.push(Fill {
                    target: FillTarget::TableColumn {
                        table: table_id.clone(),
                        header: header.clone(),
                    },
                    formula,
                    origin: None,
                });
                let outcome_location = xlsx_location(&part, Some(header));
                let column_offset = u64::try_from(column_index)
                    .map_err(|error| invalid(&part, &error.to_string()))?;
                let body_top = table
                    .range
                    .start
                    .offset(column_offset, 1)
                    .map_err(|error| invalid(&part, &error.to_string()))?;
                let body_bottom = Coordinate::new(body_top.column, table.range.end.row)
                    .map_err(|error| invalid(&part, &error.to_string()))?;
                fill_outcomes.insert(
                    (table_id.clone(), header.clone()),
                    FillOutcome {
                        location: outcome_location.clone(),
                        sheet: sheet_ids[sheet_index].clone(),
                        body: Range::new(body_top, body_bottom),
                    },
                );
                report.exact_event(
                    ConversionEvent::new(
                        ConversionFeature::Formula,
                        "table calculated-column formula was translated to an exact @fill",
                    )
                    .formula(FormulaDisposition::Translated)
                    .at(outcome_location),
                );
            }
            for row in table.range.start.row..=table.range.end.row {
                for column in table.range.start.column..=table.range.end.column {
                    table_cells.insert(Coordinate { column, row });
                }
            }
            items.push(SheetItem::Table(Table {
                id: table_id.clone(),
                block: Block::new(table.range.start, cells)
                    .map_err(|error| invalid(&part, &error.to_string()))?,
                origin: None,
            }));
            items.extend(fills.into_iter().map(SheetItem::Fill));
            report.exact_event(
                ConversionEvent::new(
                    ConversionFeature::Table,
                    "native XLSX table rectangle and ordered headers were imported",
                )
                .at(ConversionLocation::table_on_sheet(
                    sheet_ids[sheet_index].clone(),
                    table_id.clone(),
                )),
            );
        }

        for (coordinate, imported) in &worksheet.cells {
            if !table_cells.contains(coordinate) {
                items.push(SheetItem::Block(
                    Block::new(*coordinate, vec![vec![Cell::new(imported.value.clone())]])
                        .map_err(|error| invalid(sheet_part, &error.to_string()))?,
                ));
            }
        }
        for (coordinate, imported) in &worksheet.cells {
            if imported.style > 0 || default_format_is_significant {
                let style_id = style_id(imported.style)?;
                items.push(SheetItem::Apply(Apply {
                    target: ApplyTarget::Range(Range::single(*coordinate)),
                    styles: vec![style_id],
                    origin: None,
                }));
            }
        }
        for (coordinate, style) in &worksheet.style_only {
            let style_id = style_id(*style)?;
            items.push(SheetItem::Apply(Apply {
                target: ApplyTarget::Range(Range::single(*coordinate)),
                styles: vec![style_id],
                origin: None,
            }));
        }
        items.extend(worksheet.columns.drain(..).map(SheetItem::ColumnGeometry));
        items.extend(worksheet.rows.drain(..).map(SheetItem::RowGeometry));
        for feature in worksheet.omitted_features {
            report.omit(
                ConversionEvent::new(
                    ConversionFeature::Other(feature),
                    "worksheet feature is outside the initial Marksheet conversion profile",
                )
                .at(xlsx_location(sheet_part, None)),
            );
        }
        sheets.push(Sheet {
            id: sheet_ids[sheet_index].clone(),
            label: source_sheet.label.clone(),
            items,
            origin: None,
        });
    }

    let styles = style_definitions
        .into_iter()
        .enumerate()
        .skip(usize::from(!default_format_is_significant))
        .map(|(index, properties)| {
            Ok(Style {
                id: style_id(index)?,
                properties,
                origin: None,
            })
        })
        .collect::<Result<Vec<_>, ConvertError>>()?;
    let ImportedNames { names, omitted } = import_names(
        &workbook_info.names,
        &name_ids,
        &workbook_info.sheets,
        &sheet_ids,
        &table_name_map,
        &table_headers,
        &mut report,
    )?;
    if !omitted.is_empty() {
        replace_formulas_referencing_omitted_names(
            &mut sheets,
            &omitted,
            &fill_outcomes,
            limits,
            &mut report,
        )?;
    }
    let workbook = Workbook {
        styles,
        names,
        sheets,
        ..Workbook::default()
    };

    report.exact_event(ConversionEvent::new(
        ConversionFeature::Sheet,
        "worksheet order and labels were imported",
    ));
    if workbook_info.date_1904 {
        report.exact_event(ConversionEvent::new(
            ConversionFeature::WorkbookSettings,
            "1904 date-system serials were translated to absolute dates and datetimes",
        ));
    }
    report.exact_event(ConversionEvent::new(
        ConversionFeature::Cell,
        "supported sparse XLSX scalar cells were imported",
    ));
    if !workbook.styles.is_empty() {
        report.exact_event(ConversionEvent::new(
            ConversionFeature::Style,
            "supported cell format properties were imported",
        ));
    }
    if workbook.sheets.iter().any(|sheet| {
        sheet.items.iter().any(|item| {
            matches!(
                item,
                SheetItem::ColumnGeometry(_) | SheetItem::RowGeometry(_)
            )
        })
    }) {
        report.exact_event(ConversionEvent::new(
            ConversionFeature::ColumnWidth,
            "column widths and row heights were imported",
        ));
    }
    record_unconsumed_package_content(&package, &consumed_parts, limits, &mut report)?;
    Ok(Conversion {
        value: workbook,
        report: report.finish(),
    })
}

fn parse_workbook(bytes: &[u8], limits: ConversionLimits) -> Result<WorkbookInfo, ConvertError> {
    let part = "xl/workbook.xml";
    validate_consumed_part_namespaces(bytes, part)?;
    let mut reader = Reader::from_reader(bytes);
    let mut info = WorkbookInfo {
        omitted_features: scan_workbook_unsupported(bytes, part)?,
        ..WorkbookInfo::default()
    };
    let mut current_name: Option<String> = None;
    let mut name_text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element))
                if local_name(element.name().as_ref()) == b"workbookPr" =>
            {
                info.date_1904 = attribute(&reader, &element, b"date1904", part)?
                    .map(|value| parse_bool(&value, part))
                    .transpose()?
                    .unwrap_or(false);
            }
            Ok(Event::Start(element) | Event::Empty(element))
                if local_name(element.name().as_ref()) == b"sheet" =>
            {
                if info.sheets.len() >= limits.max_sheets {
                    return Err(resource(part, "sheet count exceeds the configured limit"));
                }
                let label = required_attribute(&reader, &element, b"name", part)?;
                if label.len() > limits.max_string_bytes {
                    return Err(resource(part, "sheet label exceeds the string limit"));
                }
                info.sheets.push(WorkbookSheet {
                    label,
                    relationship: required_attribute(&reader, &element, b"id", part)?,
                });
            }
            Ok(Event::Start(element)) if local_name(element.name().as_ref()) == b"definedName" => {
                if attribute(&reader, &element, b"localSheetId", part)?.is_some() {
                    return Err(ConvertError::new(
                        ConvertErrorCode::UnsupportedPackage,
                        "sheet-local defined names are outside the initial profile",
                    )
                    .at(xlsx_location(part, None)));
                }
                current_name = Some(required_attribute(&reader, &element, b"name", part)?);
                name_text.clear();
            }
            Ok(Event::Text(text)) if current_name.is_some() => {
                append_text(&mut name_text, &text, part, limits)?;
            }
            Ok(Event::GeneralRef(reference)) if current_name.is_some() => {
                append_reference(&mut name_text, &reference, part, limits)?;
            }
            Ok(Event::End(element)) if local_name(element.name().as_ref()) == b"definedName" => {
                let name = current_name
                    .take()
                    .ok_or_else(|| invalid(part, "definedName closes without opening"))?;
                info.names.push(DefinedName {
                    name,
                    expression: name_text.clone(),
                });
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(invalid(part, &format!("malformed workbook XML: {error}"))),
        }
    }
    Ok(info)
}

fn scan_workbook_unsupported(bytes: &[u8], part: &str) -> Result<BTreeSet<String>, ConvertError> {
    let mut reader = Reader::from_reader(bytes);
    let mut unsupported = BTreeSet::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let allowed_attributes: &[&[u8]] = match name {
                    b"workbookPr" => &[b"date1904"],
                    b"sheet" => &[b"name", b"sheetId", b"id", b"state"],
                    b"definedName" => &[b"name", b"localSheetId"],
                    b"calcPr" => &[b"calcId", b"fullCalcOnLoad", b"forceFullCalc"],
                    _ => &[],
                };
                match name {
                    b"workbookViews" | b"workbookView" => {
                        unsupported.insert("workbook_views".to_owned());
                    }
                    b"workbook" | b"workbookPr" | b"sheets" | b"sheet" | b"definedNames"
                    | b"definedName" | b"calcPr" => {}
                    _ => {
                        unsupported.insert("unknown_workbook_content".to_owned());
                    }
                }
                if name == b"sheet"
                    && attribute(&reader, &element, b"state", part)?
                        .is_some_and(|state| state != "visible")
                {
                    unsupported.insert("sheet_visibility".to_owned());
                }
                if name == b"calcPr" {
                    let calc_id = attribute(&reader, &element, b"calcId", part)?;
                    let full_calc = attribute(&reader, &element, b"fullCalcOnLoad", part)?
                        .map(|value| parse_bool(&value, part))
                        .transpose()?;
                    let force_full_calc = attribute(&reader, &element, b"forceFullCalc", part)?
                        .map(|value| parse_bool(&value, part))
                        .transpose()?;
                    if calc_id.as_deref() != Some("0")
                        || full_calc != Some(true)
                        || force_full_calc != Some(true)
                    {
                        unsupported.insert("workbook_calculation_properties".to_owned());
                    }
                }
                if element_has_unsupported_attributes(&element, allowed_attributes, part)? {
                    unsupported.insert("unsupported_workbook_attributes".to_owned());
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(invalid(
                    part,
                    &format!("malformed workbook XML during feature inventory: {error}"),
                ));
            }
        }
    }
    Ok(unsupported)
}

fn parse_shared_strings(
    bytes: &[u8],
    part: &str,
    limits: ConversionLimits,
) -> Result<(Vec<String>, BTreeSet<String>), ConvertError> {
    validate_consumed_part_namespaces(bytes, part)?;
    let mut reader = Reader::from_reader(bytes);
    let mut strings = Vec::new();
    let mut unsupported = scan_shared_string_unsupported(bytes, part)?;
    let mut current: Option<String> = None;
    let mut inside_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if local_name(element.name().as_ref()) == b"si" => {
                current = Some(String::new());
            }
            Ok(Event::Start(element)) if local_name(element.name().as_ref()) == b"t" => {
                inside_text = true;
            }
            Ok(Event::Start(element) | Event::Empty(element))
                if matches!(local_name(element.name().as_ref()), b"r" | b"rPr") =>
            {
                unsupported.insert("xlsx_shared_string_rich_text".to_owned());
            }
            Ok(Event::Start(element) | Event::Empty(element))
                if matches!(local_name(element.name().as_ref()), b"rPh" | b"phoneticPr") =>
            {
                unsupported.insert("xlsx_shared_string_phonetics".to_owned());
            }
            Ok(Event::Text(text)) if inside_text => {
                if let Some(current) = &mut current {
                    append_text(current, &text, part, limits)?;
                }
            }
            Ok(Event::GeneralRef(reference)) if inside_text => {
                if let Some(current) = &mut current {
                    append_reference(current, &reference, part, limits)?;
                }
            }
            Ok(Event::End(element)) if local_name(element.name().as_ref()) == b"t" => {
                inside_text = false;
            }
            Ok(Event::End(element)) if local_name(element.name().as_ref()) == b"si" => {
                strings.push(
                    current
                        .take()
                        .ok_or_else(|| invalid(part, "shared string closes without opening"))?,
                );
                if strings.len() > limits.max_shared_strings {
                    return Err(resource(
                        part,
                        "shared string count exceeds the configured limit",
                    ));
                }
            }
            Ok(Event::DocType(_)) => {
                return Err(ConvertError::new(
                    ConvertErrorCode::UnsupportedPackage,
                    "DOCTYPE is not accepted in shared strings",
                ));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(invalid(
                    part,
                    &format!("malformed shared strings XML: {error}"),
                ));
            }
        }
    }
    Ok((strings, unsupported))
}

fn scan_shared_string_unsupported(
    bytes: &[u8],
    part: &str,
) -> Result<BTreeSet<String>, ConvertError> {
    let mut reader = Reader::from_reader(bytes);
    let mut unsupported = BTreeSet::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let allowed_attributes: &[&[u8]] = match name {
                    b"sst" => &[b"count", b"uniqueCount"],
                    b"t" => &[b"space"],
                    _ => &[],
                };
                match name {
                    b"r" | b"rPr" | b"b" | b"i" | b"u" | b"strike" | b"sz" | b"color"
                    | b"rFont" | b"family" | b"scheme" | b"vertAlign" => {
                        unsupported.insert("xlsx_shared_string_rich_text".to_owned());
                    }
                    b"rPh" | b"phoneticPr" => {
                        unsupported.insert("xlsx_shared_string_phonetics".to_owned());
                    }
                    b"sst" | b"si" | b"t" => {}
                    _ => {
                        unsupported.insert("xlsx_shared_string_unknown_content".to_owned());
                    }
                }
                if element_has_unsupported_attributes(&element, allowed_attributes, part)? {
                    unsupported.insert("xlsx_shared_string_unsupported_attributes".to_owned());
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(invalid(
                    part,
                    &format!("malformed shared strings XML during feature inventory: {error}"),
                ));
            }
        }
    }
    Ok(unsupported)
}

fn record_consumed_part_omissions(
    part: &str,
    unsupported: BTreeSet<String>,
    report: &mut ConversionReport,
) {
    for feature in unsupported {
        report.omit(
            ConversionEvent::new(
                ConversionFeature::Other(feature),
                "OOXML subfeature in a consumed part is not represented in Marksheet",
            )
            .at(xlsx_location(part, None)),
        );
    }
}

/// Emits one approximated `core_styles` outcome per source cell format whose
/// number format the Marksheet style model cannot reproduce.
fn record_dropped_number_formats(
    part: &str,
    dropped: &[DroppedNumberFormat],
    report: &mut ConversionReport,
) {
    for format in dropped {
        let detail = match &format.code {
            Some(code) => format!(
                "XLSX number format {} ({code:?}) has no exact Marksheet equivalent",
                format.id
            ),
            None => format!(
                "built-in XLSX number format {} has no exact Marksheet equivalent",
                format.id
            ),
        };
        report.approximate(ConversionEvent::new(ConversionFeature::Style, detail).at(
            xlsx_location(part, Some(&format!("cellXfs[{}]", format.index))),
        ));
    }
}

fn record_unsupported_style_element(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    section: StyleSection,
    unsupported: &mut BTreeSet<String>,
) -> Result<(), ConvertError> {
    let qualified_name = element.name();
    let name = local_name(qualified_name.as_ref());
    let allowed_attributes: &[&[u8]] = match name {
        b"numFmts" | b"fonts" | b"fills" | b"borders" | b"cellStyleXfs" | b"cellXfs"
        | b"cellStyles" | b"dxfs" => &[b"count"],
        b"numFmt" => &[b"numFmtId", b"formatCode"],
        b"b" | b"i" | b"u" | b"strike" | b"outline" | b"shadow" | b"condense" | b"extend" => {
            &[b"val"]
        }
        b"sz" | b"name" | b"vertAlign" | b"charset" | b"family" | b"scheme" => &[b"val"],
        b"color" | b"fgColor" | b"bgColor" => &[b"rgb", b"theme", b"tint", b"indexed", b"auto"],
        b"patternFill" => &[b"patternType"],
        b"gradientFill" => &[b"type", b"degree", b"left", b"right", b"top", b"bottom"],
        b"xf" => &[
            b"numFmtId",
            b"fontId",
            b"fillId",
            b"borderId",
            b"xfId",
            b"applyFont",
            b"applyFill",
            b"applyNumberFormat",
            b"applyAlignment",
            b"applyProtection",
        ],
        b"alignment" => &[
            b"horizontal",
            b"vertical",
            b"wrapText",
            b"textRotation",
            b"shrinkToFit",
            b"indent",
            b"relativeIndent",
            b"justifyLastLine",
            b"readingOrder",
        ],
        b"cellStyle" => &[b"name", b"xfId", b"builtinId", b"hidden", b"customBuiltin"],
        b"tableStyles" => &[b"count", b"defaultTableStyle", b"defaultPivotStyle"],
        _ => &[],
    };
    match name {
        b"tableStyles"
            if parse_usize_attribute(reader, element, b"count", part)?
                .is_some_and(|count| count > 0)
                || element_has_any_attribute(
                    reader,
                    element,
                    part,
                    &[b"defaultTableStyle", b"defaultPivotStyle"],
                )? =>
        {
            unsupported.insert("xlsx_style_table_defaults".to_owned());
        }
        b"tableStyle" | b"tableStyleElement" => {
            unsupported.insert("xlsx_style_table_defaults".to_owned());
        }
        b"dxfs"
            if parse_usize_attribute(reader, element, b"count", part)?
                .is_some_and(|count| count > 0) =>
        {
            unsupported.insert("xlsx_style_differential_formats".to_owned());
        }
        b"dxf" => {
            unsupported.insert("xlsx_style_differential_formats".to_owned());
        }
        b"styleSheet" | b"numFmts" | b"numFmt" | b"fonts" | b"font" | b"b" | b"i" | b"u"
        | b"strike" | b"outline" | b"shadow" | b"condense" | b"extend" | b"sz" | b"name"
        | b"vertAlign" | b"charset" | b"family" | b"scheme" | b"color" | b"fills" | b"fill"
        | b"patternFill" | b"gradientFill" | b"fgColor" | b"bgColor" | b"borders" | b"border"
        | b"left" | b"right" | b"top" | b"bottom" | b"diagonal" | b"cellStyleXfs" | b"cellXfs"
        | b"xf" | b"alignment" | b"protection" | b"cellStyles" | b"cellStyle" | b"tableStyles"
        | b"dxfs" => {}
        _ => {
            unsupported.insert("xlsx_style_unknown_content".to_owned());
        }
    }
    if element_has_unsupported_attributes(element, allowed_attributes, part)? {
        unsupported.insert("xlsx_style_unsupported_attributes".to_owned());
    }
    if section == StyleSection::Fonts
        && matches!(
            name,
            b"name"
                | b"u"
                | b"strike"
                | b"outline"
                | b"shadow"
                | b"condense"
                | b"extend"
                | b"vertAlign"
                | b"charset"
                | b"family"
                | b"scheme"
        )
    {
        unsupported.insert("xlsx_style_font_metadata".to_owned());
    }
    if section == StyleSection::Fills && name == b"gradientFill" {
        unsupported.insert("xlsx_style_gradient_fill".to_owned());
    }
    if section == StyleSection::Fills
        && name == b"patternFill"
        && attribute(reader, element, b"patternType", part)?
            .is_some_and(|pattern| !matches!(pattern.as_str(), "none" | "gray125" | "solid"))
    {
        unsupported.insert("xlsx_style_pattern_fill".to_owned());
    }
    let exported_background_sentinel = name == b"bgColor"
        && attribute(reader, element, b"indexed", part)?.as_deref() == Some("64")
        && !element_has_any_attribute(reader, element, part, &[b"theme", b"tint", b"auto"])?;
    if section == StyleSection::Fills && name == b"bgColor" {
        let is_exported_sentinel = exported_background_sentinel;
        if !is_exported_sentinel {
            unsupported.insert("xlsx_style_pattern_fill".to_owned());
        }
    }
    if matches!(name, b"color" | b"fgColor" | b"bgColor")
        && style_color_has_unsupported_attributes(reader, element, part)?
        && !exported_background_sentinel
    {
        unsupported.insert("xlsx_style_theme_or_indexed_color".to_owned());
        if section == StyleSection::None {
            unsupported.insert("xlsx_style_borders".to_owned());
        }
    }
    if section == StyleSection::CellXfs && name == b"alignment" {
        for unsupported_attribute in [
            b"textRotation".as_slice(),
            b"shrinkToFit".as_slice(),
            b"indent".as_slice(),
            b"relativeIndent".as_slice(),
            b"justifyLastLine".as_slice(),
            b"readingOrder".as_slice(),
        ] {
            if attribute(reader, element, unsupported_attribute, part)?.is_some() {
                unsupported.insert("xlsx_style_advanced_alignment".to_owned());
                break;
            }
        }
    }
    if section == StyleSection::CellXfs && name == b"protection" {
        unsupported.insert("xlsx_style_protection".to_owned());
    }
    if section == StyleSection::CellXfs
        && name == b"xf"
        && parse_usize_attribute(reader, element, b"borderId", part)?.is_some_and(|id| id > 0)
    {
        unsupported.insert("xlsx_style_borders".to_owned());
    }
    if name == b"cellStyleXfs" && parse_usize_attribute(reader, element, b"count", part)? != Some(1)
    {
        unsupported.insert("xlsx_style_base_formats".to_owned());
    }
    if section == StyleSection::CellStyleXfs && name == b"xf" {
        let canonical = attributes_equal(
            reader,
            element,
            part,
            &[
                (b"numFmtId", "0"),
                (b"fontId", "0"),
                (b"fillId", "0"),
                (b"borderId", "0"),
            ],
        )? && !element_has_any_attribute(
            reader,
            element,
            part,
            &[
                b"xfId",
                b"applyFont",
                b"applyFill",
                b"applyNumberFormat",
                b"applyAlignment",
                b"applyProtection",
            ],
        )?;
        if !canonical {
            unsupported.insert("xlsx_style_base_formats".to_owned());
        }
    }
    if section == StyleSection::CellStyleXfs && matches!(name, b"alignment" | b"protection") {
        unsupported.insert("xlsx_style_base_formats".to_owned());
    }
    if name == b"cellStyles" && parse_usize_attribute(reader, element, b"count", part)? != Some(1) {
        unsupported.insert("xlsx_style_named_styles".to_owned());
    }
    if section == StyleSection::CellStyles && name == b"cellStyle" {
        let canonical = attribute(reader, element, b"name", part)?.as_deref() == Some("Normal")
            && attribute(reader, element, b"xfId", part)?.as_deref() == Some("0")
            && attribute(reader, element, b"builtinId", part)?.as_deref() == Some("0")
            && !element_has_any_attribute(reader, element, part, &[b"hidden", b"customBuiltin"])?;
        if !canonical {
            unsupported.insert("xlsx_style_named_styles".to_owned());
        }
    }
    if section == StyleSection::CellXfs && name == b"xf" {
        let required_true = attributes_are_true(
            reader,
            element,
            part,
            &[b"applyFont", b"applyFill", b"applyNumberFormat"],
        )?;
        let canonical_ids = attribute(reader, element, b"xfId", part)?.as_deref() == Some("0")
            && attribute(reader, element, b"borderId", part)?.as_deref() == Some("0");
        let alignment_flag = attribute(reader, element, b"applyAlignment", part)?;
        let supported_alignment_flag = alignment_flag
            .as_deref()
            .is_none_or(|value| matches!(value, "1" | "true"));
        if !required_true
            || !canonical_ids
            || !supported_alignment_flag
            || attribute(reader, element, b"applyProtection", part)?.is_some()
        {
            unsupported.insert("xlsx_style_apply_flags".to_owned());
        }
    }
    let border_metadata = name == b"border"
        && element_has_any_attribute(
            reader,
            element,
            part,
            &[b"diagonalUp", b"diagonalDown", b"outline"],
        )?;
    let styled_edge = matches!(name, b"left" | b"right" | b"top" | b"bottom" | b"diagonal")
        && attribute(reader, element, b"style", part)?.is_some();
    if section == StyleSection::None && (border_metadata || styled_edge) {
        unsupported.insert("xlsx_style_borders".to_owned());
    }
    Ok(())
}

fn element_has_any_attribute(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    names: &[&[u8]],
) -> Result<bool, ConvertError> {
    for name in names {
        if attribute(reader, element, name, part)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn attributes_equal(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    expected: &[(&[u8], &str)],
) -> Result<bool, ConvertError> {
    for (name, expected_value) in expected {
        if attribute(reader, element, name, part)?.as_deref() != Some(*expected_value) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn attributes_are_true(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    names: &[&[u8]],
) -> Result<bool, ConvertError> {
    for name in names {
        let Some(value) = attribute(reader, element, name, part)? else {
            return Ok(false);
        };
        if !parse_bool(&value, part)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn style_color_has_unsupported_attributes(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
) -> Result<bool, ConvertError> {
    for name in [
        b"theme".as_slice(),
        b"tint".as_slice(),
        b"indexed".as_slice(),
        b"auto".as_slice(),
    ] {
        if attribute(reader, element, name, part)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A `cellXfs` number format that the Marksheet style model cannot carry back
/// out unchanged, reported once per source cell format.
#[derive(Clone, Debug)]
struct DroppedNumberFormat {
    index: usize,
    id: u32,
    code: Option<String>,
}

#[derive(Clone, Debug)]
struct ParsedStyles {
    definitions: Vec<StyleProperties>,
    unsupported: BTreeSet<String>,
    dropped_number_formats: Vec<DroppedNumberFormat>,
}

fn parse_styles(
    bytes: &[u8],
    part: &str,
    limits: ConversionLimits,
) -> Result<ParsedStyles, ConvertError> {
    validate_consumed_part_namespaces(bytes, part)?;
    let mut reader = Reader::from_reader(bytes);
    let mut section = StyleSection::None;
    let mut num_formats = BTreeMap::<u32, String>::new();
    let mut fonts = Vec::<StyleProperties>::new();
    let mut fills = Vec::<Option<Color>>::new();
    let mut xfs = Vec::<StyleProperties>::new();
    let mut dropped_number_formats = Vec::<DroppedNumberFormat>::new();
    let mut current_xf_number_format: Option<u32> = None;
    let mut unsupported = BTreeSet::new();
    let mut base_xfs = 0_usize;
    let mut expected_num_formats = None;
    let mut expected_fonts = None;
    let mut expected_fills = None;
    let mut expected_base_xfs = None;
    let mut expected_xfs = None;
    let mut expected_cell_styles = None;
    let mut cell_styles = 0_usize;
    let mut current_font: Option<StyleProperties> = None;
    let mut current_fill: Option<FillBuilder> = None;
    let mut current_xf: Option<StyleProperties> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                record_unsupported_style_element(
                    &reader,
                    &element,
                    part,
                    section,
                    &mut unsupported,
                )?;
                match local_name(element.name().as_ref()) {
                    b"numFmts" => {
                        section = StyleSection::NumFmts;
                        expected_num_formats =
                            declared_style_count(&reader, &element, part, limits)?;
                    }
                    b"fonts" => {
                        section = StyleSection::Fonts;
                        expected_fonts = declared_style_count(&reader, &element, part, limits)?;
                    }
                    b"fills" => {
                        section = StyleSection::Fills;
                        expected_fills = declared_style_count(&reader, &element, part, limits)?;
                    }
                    b"cellStyleXfs" => {
                        section = StyleSection::CellStyleXfs;
                        expected_base_xfs = declared_style_count(&reader, &element, part, limits)?;
                    }
                    b"cellXfs" => {
                        section = StyleSection::CellXfs;
                        expected_xfs = declared_style_count(&reader, &element, part, limits)?;
                    }
                    b"cellStyles" => {
                        section = StyleSection::CellStyles;
                        expected_cell_styles =
                            declared_style_count(&reader, &element, part, limits)?;
                    }
                    b"font" if section == StyleSection::Fonts => {
                        current_font = Some(StyleProperties::default());
                    }
                    b"fill" if section == StyleSection::Fills => {
                        current_fill = Some(FillBuilder::default());
                    }
                    b"xf" if section == StyleSection::CellStyleXfs => {
                        base_xfs = base_xfs
                            .checked_add(1)
                            .ok_or_else(|| resource(part, "base style count overflow"))?;
                    }
                    b"xf" if section == StyleSection::CellXfs => {
                        let (style, number_format_loss) = xf_style(
                            &reader,
                            &element,
                            part,
                            &fonts,
                            &fills,
                            &num_formats,
                            base_xfs,
                        )?;
                        current_xf = Some(style);
                        current_xf_number_format = number_format_loss;
                    }
                    b"alignment" if section == StyleSection::CellXfs => {
                        if let Some(style) = &mut current_xf {
                            read_alignment(&reader, &element, part, style)?;
                        }
                    }
                    b"cellStyle" if section == StyleSection::CellStyles => {
                        cell_styles = cell_styles
                            .checked_add(1)
                            .ok_or_else(|| resource(part, "named style count overflow"))?;
                    }
                    _ => update_style_component(
                        &reader,
                        &element,
                        part,
                        section,
                        &mut current_font,
                        &mut current_fill,
                        &mut num_formats,
                    )?,
                }
            }
            Ok(Event::Empty(element)) => {
                record_unsupported_style_element(
                    &reader,
                    &element,
                    part,
                    section,
                    &mut unsupported,
                )?;
                let qualified_name = element.name();
                let element_name = local_name(qualified_name.as_ref());
                if matches!(
                    element_name,
                    b"numFmts" | b"fonts" | b"fills" | b"cellStyleXfs" | b"cellXfs" | b"cellStyles"
                ) {
                    let table = std::str::from_utf8(element_name)
                        .map_err(|_| invalid(part, "style table name is not UTF-8"))?;
                    let expected = declared_style_count(&reader, &element, part, limits)?;
                    validate_declared_style_count(expected, 0, part, table)?;
                } else if element_name == b"font" && section == StyleSection::Fonts {
                    fonts.push(StyleProperties::default());
                } else if element_name == b"fill" && section == StyleSection::Fills {
                    fills.push(None);
                } else if element_name == b"xf" && section == StyleSection::CellStyleXfs {
                    base_xfs = base_xfs
                        .checked_add(1)
                        .ok_or_else(|| resource(part, "base style count overflow"))?;
                } else if element_name == b"xf" && section == StyleSection::CellXfs {
                    let (style, number_format_loss) = xf_style(
                        &reader,
                        &element,
                        part,
                        &fonts,
                        &fills,
                        &num_formats,
                        base_xfs,
                    )?;
                    record_dropped_number_format(
                        &mut dropped_number_formats,
                        xfs.len(),
                        number_format_loss,
                        &num_formats,
                    );
                    xfs.push(style);
                } else if element_name == b"alignment" && section == StyleSection::CellXfs {
                    if let Some(style) = &mut current_xf {
                        read_alignment(&reader, &element, part, style)?;
                    }
                } else if element_name == b"cellStyle" && section == StyleSection::CellStyles {
                    cell_styles = cell_styles
                        .checked_add(1)
                        .ok_or_else(|| resource(part, "named style count overflow"))?;
                } else {
                    update_style_component(
                        &reader,
                        &element,
                        part,
                        section,
                        &mut current_font,
                        &mut current_fill,
                        &mut num_formats,
                    )?;
                }
            }
            Ok(Event::End(element)) => match local_name(element.name().as_ref()) {
                b"font" if section == StyleSection::Fonts => fonts.push(
                    current_font
                        .take()
                        .ok_or_else(|| invalid(part, "font closes without opening"))?,
                ),
                b"fill" if section == StyleSection::Fills => fills.push(
                    current_fill
                        .take()
                        .ok_or_else(|| invalid(part, "fill closes without opening"))?
                        .color,
                ),
                b"xf" if section == StyleSection::CellXfs => {
                    let style = current_xf
                        .take()
                        .ok_or_else(|| invalid(part, "cell format closes without opening"))?;
                    record_dropped_number_format(
                        &mut dropped_number_formats,
                        xfs.len(),
                        current_xf_number_format.take(),
                        &num_formats,
                    );
                    xfs.push(style);
                }
                b"numFmts" | b"fonts" | b"fills" | b"cellStyleXfs" | b"cellXfs" | b"cellStyles" => {
                    section = StyleSection::None;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(invalid(part, &format!("malformed styles XML: {error}"))),
        }
        if num_formats
            .len()
            .max(fonts.len())
            .max(fills.len())
            .max(base_xfs)
            .max(xfs.len())
            .max(cell_styles)
            > limits.max_styles
        {
            return Err(resource(part, "style table exceeds the configured limit"));
        }
    }
    validate_declared_style_count(expected_num_formats, num_formats.len(), part, "numFmts")?;
    validate_declared_style_count(expected_fonts, fonts.len(), part, "fonts")?;
    validate_declared_style_count(expected_fills, fills.len(), part, "fills")?;
    validate_declared_style_count(expected_base_xfs, base_xfs, part, "cellStyleXfs")?;
    validate_declared_style_count(expected_xfs, xfs.len(), part, "cellXfs")?;
    validate_declared_style_count(expected_cell_styles, cell_styles, part, "cellStyles")?;
    if xfs.is_empty() {
        return Err(invalid(part, "styles part contains no cell formats"));
    }
    Ok(ParsedStyles {
        definitions: xfs,
        unsupported,
        dropped_number_formats,
    })
}

fn record_dropped_number_format(
    dropped: &mut Vec<DroppedNumberFormat>,
    index: usize,
    id: Option<u32>,
    num_formats: &BTreeMap<u32, String>,
) {
    if let Some(id) = id {
        dropped.push(DroppedNumberFormat {
            index,
            id,
            code: num_formats.get(&id).cloned(),
        });
    }
}

fn declared_style_count(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    limits: ConversionLimits,
) -> Result<Option<usize>, ConvertError> {
    let count = parse_usize_attribute(reader, element, b"count", part)?;
    if count.is_some_and(|count| count > limits.max_styles) {
        return Err(resource(
            part,
            "declared style table count exceeds the configured limit",
        ));
    }
    Ok(count)
}

fn validate_declared_style_count(
    expected: Option<usize>,
    actual: usize,
    part: &str,
    table: &str,
) -> Result<(), ConvertError> {
    if expected.is_some_and(|expected| expected != actual) {
        return Err(invalid(
            part,
            &format!("declared {table} count does not match its entries"),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn update_style_component(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    section: StyleSection,
    current_font: &mut Option<StyleProperties>,
    current_fill: &mut Option<FillBuilder>,
    num_formats: &mut BTreeMap<u32, String>,
) -> Result<(), ConvertError> {
    match section {
        StyleSection::NumFmts if local_name(element.name().as_ref()) == b"numFmt" => {
            let id = parse_u32_attribute(reader, element, b"numFmtId", part)?
                .ok_or_else(|| invalid(part, "custom number format has no ID"))?;
            let code = required_attribute(reader, element, b"formatCode", part)?;
            if num_formats.insert(id, code).is_some() {
                return Err(invalid(part, "duplicate custom number format ID"));
            }
        }
        StyleSection::Fonts => {
            if let Some(font) = current_font {
                update_font_component(reader, element, part, font)?;
            }
        }
        StyleSection::Fills if local_name(element.name().as_ref()) == b"fgColor" => {
            if let Some(fill) = current_fill {
                if let Some(rgb) = attribute(reader, element, b"rgb", part)? {
                    fill.color = Some(parse_argb(&rgb, part)?);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn xf_style(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    fonts: &[StyleProperties],
    fills: &[Option<Color>],
    num_formats: &BTreeMap<u32, String>,
    base_xfs: usize,
) -> Result<(StyleProperties, Option<u32>), ConvertError> {
    let font_id = parse_usize_attribute(reader, element, b"fontId", part)?.unwrap_or(0);
    let fill_id = parse_usize_attribute(reader, element, b"fillId", part)?.unwrap_or(0);
    let number_id = parse_u32_attribute(reader, element, b"numFmtId", part)?.unwrap_or(0);
    let mut style = fonts
        .get(font_id)
        .cloned()
        .ok_or_else(|| invalid(part, "cell format fontId is out of bounds"))?;
    if fill_id >= fills.len() {
        return Err(invalid(part, "cell format fillId is out of bounds"));
    }
    if number_id >= 164 && !num_formats.contains_key(&number_id) {
        return Err(invalid(
            part,
            "cell format numFmtId does not reference a declared custom format",
        ));
    }
    if let Some(base_id) = parse_usize_attribute(reader, element, b"xfId", part)? {
        if base_id >= base_xfs {
            return Err(invalid(part, "cell format xfId is out of bounds"));
        }
    }
    if let Some(Some(fill)) = fills.get(fill_id) {
        style.fill = Some(fill.clone());
    }
    let custom = num_formats.get(&number_id);
    apply_number_format(&mut style, number_id, custom);
    let loss = number_format_loss(&style, number_id, custom);
    Ok((style, loss))
}

fn read_alignment(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    style: &mut StyleProperties,
) -> Result<(), ConvertError> {
    style.align = match attribute(reader, element, b"horizontal", part)?.as_deref() {
        Some("left") => Some(HorizontalAlignment::Left),
        Some("center") => Some(HorizontalAlignment::Center),
        Some("right") => Some(HorizontalAlignment::Right),
        Some("general") => Some(HorizontalAlignment::General),
        _ => None,
    };
    style.valign = match attribute(reader, element, b"vertical", part)?.as_deref() {
        Some("top") => Some(VerticalAlignment::Top),
        Some("center") => Some(VerticalAlignment::Middle),
        Some("bottom") => Some(VerticalAlignment::Bottom),
        _ => None,
    };
    style.wrap = attribute(reader, element, b"wrapText", part)?
        .map(|value| parse_bool(&value, part))
        .transpose()?;
    Ok(())
}

fn apply_number_format(style: &mut StyleProperties, id: u32, custom: Option<&String>) {
    match id {
        0 => {}
        1 => style.number = Some(NumberFormat::Integer),
        2 => style.number = Some(NumberFormat::Decimal),
        9 | 10 => style.number = Some(NumberFormat::Percent),
        14..=17 => style.number = Some(NumberFormat::Date),
        18..=22 => style.number = Some(NumberFormat::DateTime),
        _ => {
            let Some(code) = custom else { return };
            let lowercase = code.to_ascii_lowercase();
            if lowercase.contains('%') {
                style.number = Some(NumberFormat::Percent);
                style.decimals = fraction_digits(&lowercase);
            } else if lowercase.contains("[$") {
                if let Some(currency) = currency_code(&lowercase) {
                    style.number = Some(NumberFormat::Currency);
                    style.currency = Some(currency);
                    style.decimals = fraction_digits(&lowercase);
                } else if lowercase.contains(['0', '#']) {
                    // Symbol- and locale-based XLSX currency codes cannot be
                    // represented as Marksheet's required ISO 4217 code. Keep
                    // the numeric shape without constructing an invalid style;
                    // `number_format_loss` reports the dropped currency data.
                    style.number = Some(NumberFormat::Decimal);
                    style.decimals = fraction_digits(&lowercase);
                }
            } else if lowercase.contains('y') || lowercase.contains('d') {
                // Marksheet dates carry no display pattern, so the code itself
                // is not reversible; `number_format_loss` reports the residue.
                style.number = Some(if lowercase.contains('h') || lowercase.contains('s') {
                    NumberFormat::DateTime
                } else {
                    NumberFormat::Date
                });
            } else if lowercase.contains(['0', '#']) {
                style.number = Some(NumberFormat::Decimal);
                style.decimals = fraction_digits(&lowercase);
            }
        }
    }
}

/// Counts the fixed decimal places a numeric format code requests. A code
/// without a fractional section requests none, which is distinct from a style
/// that leaves the decimal count unspecified.
fn fraction_digits(code: &str) -> Option<u8> {
    let count = code.split_once('.').map_or(0, |(_, fraction)| {
        fraction
            .chars()
            .take_while(|character| *character == '0')
            .count()
    });
    (count <= 15).then(|| u8::try_from(count).expect("count is bounded to 15"))
}

fn currency_code(code: &str) -> Option<String> {
    let (_, rest) = code.split_once("[$-")?;
    let (currency, _) = rest.split_once(']')?;
    (currency.len() == 3 && currency.bytes().all(|byte| byte.is_ascii_alphabetic()))
        .then(|| currency.to_ascii_uppercase())
}

/// Reports the `numFmtId` whenever the Marksheet style it produced would not
/// re-export as the same OOXML number format. Comparing against the exporter's
/// own rendering keeps a Marksheet-authored workbook exact while every format
/// this profile cannot carry stays visible in the conversion report.
fn number_format_loss(style: &StyleProperties, id: u32, custom: Option<&String>) -> Option<u32> {
    let rendered = super::export::custom_number_format_code(style);
    let reversible = match custom {
        Some(code) => rendered.is_some_and(|rendered| rendered == *code),
        None => rendered.is_none() && super::export::built_in_number_format(style) == id,
    };
    (!reversible).then_some(id)
}

struct WorksheetParseContext<'a> {
    shared_strings: &'a [String],
    styles: &'a [StyleProperties],
    sheet_id: &'a SheetId,
    formula_names: FormulaNames<'a>,
    date_1904: bool,
    limits: ConversionLimits,
}

/// The spellings an Excel formula body has to be rewritten to: sheet labels and
/// defined names both become their assigned Marksheet identifiers, which are
/// lowercase and may differ from what the XLSX says.
#[derive(Clone, Copy, Debug)]
struct FormulaNames<'a> {
    sheets: &'a BTreeMap<String, SheetId>,
    names: &'a BTreeMap<String, NameId>,
}

fn parse_worksheet(
    bytes: &[u8],
    part: &str,
    context: &WorksheetParseContext<'_>,
    report: &mut ConversionReport,
) -> Result<WorksheetData, ConvertError> {
    #[derive(Clone, Copy, Default, Eq, PartialEq)]
    enum CellTextState {
        #[default]
        None,
        Value,
        Formula,
        Inline,
    }
    #[derive(Default)]
    struct CellBuilder {
        coordinate: Option<Coordinate>,
        kind: Option<String>,
        style: usize,
        value: String,
        formula: String,
        inline_text: String,
        has_value: bool,
        has_formula: bool,
        formula_kind: Option<String>,
        text_state: CellTextState,
    }
    validate_consumed_part_namespaces(bytes, part)?;
    let mut reader = Reader::from_reader(bytes);
    let mut worksheet = WorksheetData::default();
    let mut cell: Option<CellBuilder> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                record_unsupported_worksheet_element(
                    &reader,
                    &element,
                    part,
                    &mut worksheet.omitted_features,
                )?;
                match local_name(element.name().as_ref()) {
                    b"c" => {
                        let coordinate =
                            Coordinate::parse(&required_attribute(&reader, &element, b"r", part)?)
                                .map_err(|error| invalid(part, &error.to_string()))?;
                        check_excel_coordinate(coordinate, part)?;
                        cell = Some(CellBuilder {
                            coordinate: Some(coordinate),
                            kind: attribute(&reader, &element, b"t", part)?,
                            style: parse_usize_attribute(&reader, &element, b"s", part)?
                                .unwrap_or(0),
                            ..CellBuilder::default()
                        });
                    }
                    b"v" => {
                        if let Some(cell) = &mut cell {
                            cell.has_value = true;
                            cell.text_state = CellTextState::Value;
                        }
                    }
                    b"f" => {
                        if let Some(cell) = &mut cell {
                            cell.has_formula = true;
                            cell.formula_kind = attribute(&reader, &element, b"t", part)?;
                            cell.text_state = CellTextState::Formula;
                        }
                    }
                    b"t" => {
                        if let Some(cell) = &mut cell {
                            cell.text_state = CellTextState::Inline;
                        }
                    }
                    b"row" => {
                        record_unsupported_dimension_attributes(
                            &reader,
                            &element,
                            part,
                            "row_attributes",
                            &mut worksheet.omitted_features,
                        )?;
                        read_row_geometry(&reader, &element, part, &mut worksheet.rows)?;
                    }
                    b"col" => {
                        record_unsupported_dimension_attributes(
                            &reader,
                            &element,
                            part,
                            "column_attributes",
                            &mut worksheet.omitted_features,
                        )?;
                        read_column_geometry(&reader, &element, part, &mut worksheet.columns)?;
                    }
                    b"tablePart" => worksheet
                        .table_relationships
                        .push(required_attribute(&reader, &element, b"id", part)?),
                    b"mergeCells" => {
                        worksheet.omitted_features.insert("merged_cells".to_owned());
                    }
                    b"conditionalFormatting" => {
                        worksheet
                            .omitted_features
                            .insert("conditional_formatting".to_owned());
                    }
                    b"drawing" => {
                        worksheet.omitted_features.insert("drawing".to_owned());
                    }
                    b"dataValidations" | b"dataValidation" => {
                        worksheet
                            .omitted_features
                            .insert("data_validation".to_owned());
                    }
                    b"hyperlinks" | b"hyperlink" => {
                        worksheet.omitted_features.insert("hyperlinks".to_owned());
                    }
                    b"sheetProtection" | b"protectedRanges" | b"protectedRange" => {
                        worksheet
                            .omitted_features
                            .insert("sheet_protection".to_owned());
                    }
                    b"autoFilter" => {
                        worksheet.omitted_features.insert("auto_filter".to_owned());
                    }
                    b"legacyDrawing" | b"oleObjects" | b"oleObject" | b"controls" | b"control" => {
                        worksheet
                            .omitted_features
                            .insert("embedded_objects".to_owned());
                    }
                    b"extLst" => {
                        worksheet
                            .omitted_features
                            .insert("worksheet_extensions".to_owned());
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(element)) => {
                record_unsupported_worksheet_element(
                    &reader,
                    &element,
                    part,
                    &mut worksheet.omitted_features,
                )?;
                match local_name(element.name().as_ref()) {
                    b"c" => {
                        let coordinate =
                            Coordinate::parse(&required_attribute(&reader, &element, b"r", part)?)
                                .map_err(|error| invalid(part, &error.to_string()))?;
                        check_excel_coordinate(coordinate, part)?;
                        let style =
                            parse_usize_attribute(&reader, &element, b"s", part)?.unwrap_or(0);
                        insert_style_only(
                            &mut worksheet,
                            coordinate,
                            style,
                            context.styles,
                            part,
                            context.limits,
                        )?;
                    }
                    b"v" => {
                        if let Some(cell) = &mut cell {
                            cell.has_value = true;
                        }
                    }
                    b"f" => {
                        if let Some(cell) = &mut cell {
                            cell.has_formula = true;
                            cell.formula_kind = attribute(&reader, &element, b"t", part)?;
                        }
                    }
                    b"row" => {
                        record_unsupported_dimension_attributes(
                            &reader,
                            &element,
                            part,
                            "row_attributes",
                            &mut worksheet.omitted_features,
                        )?;
                        read_row_geometry(&reader, &element, part, &mut worksheet.rows)?;
                    }
                    b"col" => {
                        record_unsupported_dimension_attributes(
                            &reader,
                            &element,
                            part,
                            "column_attributes",
                            &mut worksheet.omitted_features,
                        )?;
                        read_column_geometry(&reader, &element, part, &mut worksheet.columns)?;
                    }
                    b"tablePart" => worksheet
                        .table_relationships
                        .push(required_attribute(&reader, &element, b"id", part)?),
                    b"mergeCells" => {
                        worksheet.omitted_features.insert("merged_cells".to_owned());
                    }
                    b"conditionalFormatting" => {
                        worksheet
                            .omitted_features
                            .insert("conditional_formatting".to_owned());
                    }
                    b"drawing" => {
                        worksheet.omitted_features.insert("drawing".to_owned());
                    }
                    b"dataValidations" | b"dataValidation" => {
                        worksheet
                            .omitted_features
                            .insert("data_validation".to_owned());
                    }
                    b"hyperlinks" | b"hyperlink" => {
                        worksheet.omitted_features.insert("hyperlinks".to_owned());
                    }
                    b"sheetProtection" | b"protectedRanges" | b"protectedRange" => {
                        worksheet
                            .omitted_features
                            .insert("sheet_protection".to_owned());
                    }
                    b"autoFilter" => {
                        worksheet.omitted_features.insert("auto_filter".to_owned());
                    }
                    b"legacyDrawing" | b"oleObjects" | b"oleObject" | b"controls" | b"control" => {
                        worksheet
                            .omitted_features
                            .insert("embedded_objects".to_owned());
                    }
                    b"extLst" => {
                        worksheet
                            .omitted_features
                            .insert("worksheet_extensions".to_owned());
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(cell) = &mut cell {
                    match cell.text_state {
                        CellTextState::None => {}
                        CellTextState::Value => {
                            append_text(&mut cell.value, &text, part, context.limits)?;
                        }
                        CellTextState::Formula => {
                            append_text(&mut cell.formula, &text, part, context.limits)?;
                        }
                        CellTextState::Inline => {
                            append_text(&mut cell.inline_text, &text, part, context.limits)?;
                        }
                    }
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if let Some(cell) = &mut cell {
                    match cell.text_state {
                        CellTextState::None => {}
                        CellTextState::Value => {
                            append_reference(&mut cell.value, &reference, part, context.limits)?;
                        }
                        CellTextState::Formula => {
                            append_reference(&mut cell.formula, &reference, part, context.limits)?;
                        }
                        CellTextState::Inline => {
                            append_reference(
                                &mut cell.inline_text,
                                &reference,
                                part,
                                context.limits,
                            )?;
                        }
                    }
                }
            }
            Ok(Event::End(element)) => match local_name(element.name().as_ref()) {
                b"v" | b"f" | b"t" => {
                    if let Some(cell) = &mut cell {
                        cell.text_state = CellTextState::None;
                    }
                }
                b"c" => {
                    let builder = cell
                        .take()
                        .ok_or_else(|| invalid(part, "cell closes without opening"))?;
                    let coordinate = builder.coordinate.expect("cell builder has coordinate");
                    if builder.style >= context.styles.len() {
                        return Err(invalid(part, "cell style index is out of bounds"));
                    }
                    if builder.has_formula {
                        worksheet.formula_count = worksheet
                            .formula_count
                            .checked_add(1)
                            .ok_or_else(|| resource(part, "worksheet formula count overflow"))?;
                        if worksheet.formula_count > context.limits.max_formulas {
                            return Err(resource(
                                part,
                                "worksheet formula count exceeds the configured limit",
                            ));
                        }
                    }
                    let style_only = !builder.has_value
                        && !builder.has_formula
                        && matches!(builder.kind.as_deref(), None | Some("n"));
                    if style_only {
                        insert_style_only(
                            &mut worksheet,
                            coordinate,
                            builder.style,
                            context.styles,
                            part,
                            context.limits,
                        )?;
                    } else {
                        let value = cell_value(
                            builder.kind.as_deref(),
                            &builder.value,
                            &builder.formula,
                            builder.has_formula,
                            builder.formula_kind.as_deref(),
                            &builder.inline_text,
                            context
                                .styles
                                .get(builder.style)
                                .unwrap_or(&StyleProperties::default()),
                            context.shared_strings,
                            context.sheet_id,
                            context.formula_names,
                            context.date_1904,
                            coordinate,
                            part,
                            context.limits,
                            report,
                        )?;
                        insert_cell(
                            &mut worksheet,
                            coordinate,
                            value,
                            builder.style,
                            context.styles,
                            part,
                            context.limits,
                        )?;
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(invalid(part, &format!("malformed worksheet XML: {error}"))),
        }
    }
    if cell.is_some() {
        return Err(invalid(part, "worksheet ended inside a cell"));
    }
    Ok(worksheet)
}

#[allow(clippy::too_many_arguments)]
fn cell_value(
    kind: Option<&str>,
    raw: &str,
    formula: &str,
    has_formula: bool,
    formula_kind: Option<&str>,
    inline: &str,
    style: &StyleProperties,
    shared_strings: &[String],
    sheet: &SheetId,
    formula_names: FormulaNames<'_>,
    date_1904: bool,
    coordinate: Coordinate,
    part: &str,
    limits: ConversionLimits,
    report: &mut ConversionReport,
) -> Result<Value, ConvertError> {
    if !formula.is_empty() {
        let source = translate_excel_formula(formula, formula_names);
        let parsed = parse_portable_formula(&source, limits);
        if parsed
            .as_ref()
            .is_ok_and(|formula| !formula_references_in_xlsx_grid(&formula.expression))
        {
            return Err(invalid(part, "formula reference exceeds the XLSX grid")
                .at(ConversionLocation::cell(sheet.clone(), coordinate)));
        }
        let profile_error = parsed
            .as_ref()
            .ok()
            .and_then(|formula| validate_formula_expression(&formula.expression).err());
        if parsed.is_err() || profile_error.is_some() {
            let replacement = if profile_error
                .as_ref()
                .is_some_and(FormulaProfileError::is_invalid_arity)
            {
                CellError::Value
            } else {
                CellError::Name
            };
            let reason = profile_error.as_ref().map_or_else(
                || "formula is outside portable-a1@1 syntax".to_owned(),
                ToString::to_string,
            );
            report.approximate(
                ConversionEvent::new(
                    ConversionFeature::Formula,
                    format!(
                        "unsupported Excel formula was replaced with {} ({reason})",
                        replacement.token()
                    ),
                )
                .formula(FormulaDisposition::Replaced)
                .at(ConversionLocation::cell(sheet.clone(), coordinate)),
            );
            return Ok(Value::Error(replacement));
        }
        let event = ConversionEvent::new(
                ConversionFeature::Formula,
                if formula_kind.is_some() {
                    "supported formula body was imported, but its shared, array, or data-table mode was flattened"
                } else {
                    "supported Excel formula was translated to portable-a1@1"
                },
            )
            .formula(FormulaDisposition::Translated)
            .at(ConversionLocation::cell(sheet.clone(), coordinate));
        if formula_kind.is_some() {
            report.approximate(event);
        } else {
            report.exact_event(event);
        }
        return FormulaSource::new(source)
            .map(Value::Formula)
            .map_err(|error| invalid(part, &error.to_string()));
    }
    if has_formula {
        report.approximate(
            ConversionEvent::new(
                ConversionFeature::Formula,
                "formula record without a portable body was replaced by its cached scalar value",
            )
            .formula(FormulaDisposition::Replaced)
            .at(ConversionLocation::cell(sheet.clone(), coordinate)),
        );
    }
    match kind {
        Some("inlineStr") => Ok(Value::Text(inline.to_owned())),
        Some("s") => {
            let index = raw
                .parse::<usize>()
                .map_err(|_| invalid(part, "shared string index is not an integer"))?;
            shared_strings
                .get(index)
                .cloned()
                .map(Value::Text)
                .ok_or_else(|| invalid(part, "shared string index is out of bounds"))
        }
        Some("str") => Ok(Value::Text(raw.to_owned())),
        Some("b") => match raw {
            "0" => Ok(Value::Boolean(false)),
            "1" => Ok(Value::Boolean(true)),
            _ => Err(invalid(part, "Boolean cell value must be 0 or 1")),
        },
        Some("e") => CellError::parse(raw).map(Value::Error).ok_or_else(|| {
            ConvertError::new(
                ConvertErrorCode::UnsupportedPackage,
                format!("unsupported XLSX error token {raw:?}"),
            )
            .at(xlsx_location(part, Some(&coordinate.to_string())))
        }),
        Some("d") => {
            if raw.len() == 10 {
                return parse_iso_date(raw, part).map(Value::Date);
            }
            let datetime =
                time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
                    .map_err(|_| invalid(part, "ISO datetime cell is not valid RFC 3339"))?;
            Ok(Value::DateTime(datetime))
        }
        None | Some("n") => {
            if raw.is_empty() {
                return Ok(Value::Blank);
            }
            let number = raw
                .parse::<f64>()
                .map_err(|_| invalid(part, "numeric cell contains an invalid number"))?;
            if !number.is_finite() {
                return Err(invalid(part, "numeric cell is not finite"));
            }
            let Some(format @ (NumberFormat::Date | NumberFormat::DateTime)) = style.number else {
                return Ok(Value::Number(number));
            };
            // The serial only means a date because of its cell format, and
            // Excel's fictitious 1900 leap day makes the mapping approximate.
            let value = excel_serial(number, format == NumberFormat::DateTime, date_1904, part)?;
            report.approximate(
                ConversionEvent::new(
                    ConversionFeature::Cell,
                    "numeric cell was reinterpreted as an absolute date by its date number format",
                )
                .at(ConversionLocation::cell(sheet.clone(), coordinate)),
            );
            Ok(value)
        }
        Some(other) => Err(ConvertError::new(
            ConvertErrorCode::UnsupportedPackage,
            format!("unsupported XLSX cell type {other:?}"),
        )
        .at(xlsx_location(part, Some(&coordinate.to_string())))),
    }
}

fn parse_iso_date(raw: &str, part: &str) -> Result<Date, ConvertError> {
    let mut fields = raw.split('-');
    let year = fields
        .next()
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or_else(|| invalid(part, "ISO date year is invalid"))?;
    let month = fields
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .and_then(|value| Month::try_from(value).ok())
        .ok_or_else(|| invalid(part, "ISO date month is invalid"))?;
    let day = fields
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or_else(|| invalid(part, "ISO date day is invalid"))?;
    if fields.next().is_some() {
        return Err(invalid(part, "ISO date has trailing fields"));
    }
    Date::from_calendar_date(year, month, day)
        .map_err(|_| invalid(part, "ISO date is outside the supported calendar"))
}

fn parse_portable_formula(
    source: &str,
    limits: ConversionLimits,
) -> Result<marksheet_calc::formula::Formula, marksheet_calc::formula::FormulaError> {
    parse_formula(
        source,
        &ParseLimits {
            max_source_bytes: limits.max_string_bytes,
            max_tokens: limits.max_string_bytes.min(100_000),
            max_depth: limits.max_xml_depth,
            max_nodes: limits.max_string_bytes.min(100_000),
            max_function_arguments: limits.max_string_bytes.min(10_000),
        },
    )
}

fn formula_references_in_xlsx_grid(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Literal { .. } => true,
        ExprKind::Reference { reference } => match reference {
            marksheet_calc::formula::Reference::Cell { address, .. } => {
                coordinate_in_xlsx_grid(address.coordinate)
            }
            marksheet_calc::formula::Reference::Range(range) => {
                coordinate_in_xlsx_grid(range.start.coordinate)
                    && coordinate_in_xlsx_grid(range.end.coordinate)
            }
            marksheet_calc::formula::Reference::Name { .. }
            | marksheet_calc::formula::Reference::Structured(_) => true,
        },
        ExprKind::Unary { operand, .. } => formula_references_in_xlsx_grid(operand),
        ExprKind::Binary { left, right, .. } => {
            formula_references_in_xlsx_grid(left) && formula_references_in_xlsx_grid(right)
        }
        ExprKind::Call { call } => call.arguments.iter().all(formula_references_in_xlsx_grid),
    }
}

const fn coordinate_in_xlsx_grid(coordinate: Coordinate) -> bool {
    coordinate.column <= 16_384 && coordinate.row <= 1_048_576
}

fn parse_table(
    bytes: &[u8],
    part: &str,
    limits: ConversionLimits,
) -> Result<ImportedTable, ConvertError> {
    validate_consumed_part_namespaces(bytes, part)?;
    let omitted_features = scan_table_unsupported(bytes, part)?;
    let mut reader = Reader::from_reader(bytes);
    let mut display_name = None;
    let mut range = None;
    let mut headers = Vec::new();
    let mut calculated = BTreeMap::new();
    let mut current_header: Option<String> = None;
    let mut current_formula = String::new();
    let mut inside_calculated = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element))
                if local_name(element.name().as_ref()) == b"table" =>
            {
                let name = attribute(&reader, &element, b"displayName", part)?
                    .or(attribute(&reader, &element, b"name", part)?)
                    .ok_or_else(|| invalid(part, "table name is missing"))?;
                if name.is_empty() || name.len() > limits.max_string_bytes {
                    return Err(invalid(part, "table name is empty or too long"));
                }
                display_name = Some(name);
                range = Some(
                    Range::parse(&required_attribute(&reader, &element, b"ref", part)?)
                        .map_err(|error| invalid(part, &error.to_string()))?,
                );
            }
            Ok(Event::Start(element)) if local_name(element.name().as_ref()) == b"tableColumn" => {
                let header = required_attribute(&reader, &element, b"name", part)?;
                if header.is_empty() || header.len() > limits.max_string_bytes {
                    return Err(invalid(part, "table header is empty or too long"));
                }
                current_header = Some(header);
                current_formula.clear();
            }
            Ok(Event::Empty(element)) if local_name(element.name().as_ref()) == b"tableColumn" => {
                let header = required_attribute(&reader, &element, b"name", part)?;
                if header.is_empty() || header.len() > limits.max_string_bytes {
                    return Err(invalid(part, "table header is empty or too long"));
                }
                headers.push(header);
            }
            Ok(Event::Start(element))
                if local_name(element.name().as_ref()) == b"calculatedColumnFormula" =>
            {
                if current_header.is_none() {
                    return Err(invalid(
                        part,
                        "calculated formula is outside a table column",
                    ));
                }
                inside_calculated = true;
            }
            Ok(Event::Text(text)) if inside_calculated => {
                append_text(&mut current_formula, &text, part, limits)?;
            }
            Ok(Event::GeneralRef(reference)) if inside_calculated => {
                append_reference(&mut current_formula, &reference, part, limits)?;
            }
            Ok(Event::End(element))
                if local_name(element.name().as_ref()) == b"calculatedColumnFormula" =>
            {
                inside_calculated = false;
            }
            Ok(Event::End(element)) if local_name(element.name().as_ref()) == b"tableColumn" => {
                let header = current_header
                    .take()
                    .ok_or_else(|| invalid(part, "table column closes without opening"))?;
                if !current_formula.is_empty()
                    && calculated
                        .insert(header.clone(), current_formula.clone())
                        .is_some()
                {
                    return Err(invalid(part, "duplicate calculated table column"));
                }
                headers.push(header);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(invalid(part, &format!("malformed table XML: {error}"))),
        }
    }
    let range = range.ok_or_else(|| invalid(part, "table range is missing"))?;
    check_excel_coordinate(range.start, part)?;
    check_excel_coordinate(range.end, part)?;
    if range
        .width()
        .map_err(|error| invalid(part, &error.to_string()))?
        != u64::try_from(headers.len()).unwrap_or(u64::MAX)
    {
        return Err(invalid(part, "table column count does not match its range"));
    }
    let unique: BTreeSet<_> = headers.iter().collect();
    if unique.len() != headers.len() {
        return Err(invalid(part, "table headers are not unique"));
    }
    Ok(ImportedTable {
        display_name: display_name.ok_or_else(|| invalid(part, "table name is missing"))?,
        range,
        headers,
        calculated,
        omitted_features,
    })
}

fn scan_table_unsupported(bytes: &[u8], part: &str) -> Result<BTreeSet<String>, ConvertError> {
    let mut reader = Reader::from_reader(bytes);
    let mut unsupported = BTreeSet::new();
    let mut table_range = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let allowed_attributes: &[&[u8]] = match name {
                    b"table" => &[b"id", b"name", b"displayName", b"ref", b"headerRowCount"],
                    b"autoFilter" => &[b"ref"],
                    b"tableColumns" => &[b"count"],
                    b"tableColumn" => &[b"id", b"name"],
                    _ => &[],
                };
                match name {
                    b"filterColumn" | b"filters" | b"filter" | b"sortState" | b"sortCondition" => {
                        unsupported.insert("table_filters_and_sorting".to_owned());
                    }
                    b"totalsRowFormula" | b"totalsRowLabel" => {
                        unsupported.insert("table_totals".to_owned());
                    }
                    b"tableStyleInfo" => {
                        unsupported.insert("table_style".to_owned());
                    }
                    b"extLst" | b"ext" => {
                        unsupported.insert("table_extensions".to_owned());
                    }
                    b"table"
                    | b"autoFilter"
                    | b"tableColumns"
                    | b"tableColumn"
                    | b"calculatedColumnFormula" => {}
                    _ => {
                        unsupported.insert("unknown_table_content".to_owned());
                    }
                }
                if name == b"table" {
                    table_range = attribute(&reader, &element, b"ref", part)?
                        .map(|value| {
                            Range::parse(&value).map_err(|error| invalid(part, &error.to_string()))
                        })
                        .transpose()?;
                    if attribute(&reader, &element, b"headerRowCount", part)?
                        .is_some_and(|value| value != "1")
                    {
                        unsupported.insert("table_header_configuration".to_owned());
                    }
                }
                if name == b"autoFilter" {
                    let filter_range = attribute(&reader, &element, b"ref", part)?
                        .map(|value| {
                            Range::parse(&value).map_err(|error| invalid(part, &error.to_string()))
                        })
                        .transpose()?;
                    if filter_range.is_none() || filter_range != table_range {
                        unsupported.insert("table_filter_range".to_owned());
                    }
                }
                if (name == b"table"
                    && element_has_any_named_attribute(
                        &element,
                        &[b"totalsRowCount", b"totalsRowShown"],
                        part,
                    )?)
                    || (name == b"tableColumn"
                        && element_has_any_named_attribute(
                            &element,
                            &[b"totalsRowFunction", b"totalsRowLabel"],
                            part,
                        )?)
                {
                    unsupported.insert("table_totals".to_owned());
                }
                if element_has_unsupported_attributes(&element, allowed_attributes, part)? {
                    unsupported.insert("unsupported_table_attributes".to_owned());
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(invalid(
                    part,
                    &format!("malformed table XML during feature inventory: {error}"),
                ));
            }
        }
    }
    Ok(unsupported)
}

fn element_has_any_named_attribute(
    element: &quick_xml::events::BytesStart<'_>,
    names: &[&[u8]],
    part: &str,
) -> Result<bool, ConvertError> {
    for candidate in element.attributes() {
        let candidate = candidate
            .map_err(|error| invalid(part, &format!("malformed XML attribute: {error}")))?;
        let local = local_name(candidate.key.as_ref());
        if names.contains(&local) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Defined names that survived the import, plus the ones that could not be
/// represented. Omitted names are keyed by the identifier a portable formula
/// body would have to spell to reach them, so formula rewriting can find the
/// references that are now unresolved.
#[derive(Debug, Default)]
struct ImportedNames {
    names: Vec<Name>,
    omitted: BTreeSet<NameId>,
}

/// True for the `_xlnm.` built-ins (print areas, print titles, filter
/// databases) that Marksheet never represents as workbook names.
fn is_builtin_defined_name(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("_xlnm.")
}

/// Chooses the Marksheet identifier for every defined name that can become a
/// workbook name, keyed by the case-folded XLSX spelling a formula body uses to
/// reach it.
///
/// Identifiers are claimed in workbook order, and also for names whose target
/// later turns out to be unsupported, so an omitted name still owns the
/// spelling its callers were translated to. Built-ins are skipped, and so is
/// the second half of a case-insensitive duplicate — `import_names` rejects
/// that package outright, so no identifier is owed to it.
fn assign_name_identifiers(
    names: &[DefinedName],
) -> Result<BTreeMap<String, NameId>, ConvertError> {
    let mut used = BTreeSet::new();
    let mut assigned = BTreeMap::new();
    for source in names {
        if is_builtin_defined_name(&source.name) {
            continue;
        }
        let folded = source.name.to_lowercase();
        if assigned.contains_key(&folded) {
            continue;
        }
        let id = unique_identifier::<NameId>(&source.name, "xlsx_name", &mut used)?;
        assigned.insert(folded, id);
    }
    Ok(assigned)
}

fn import_names(
    names: &[DefinedName],
    name_ids: &BTreeMap<String, NameId>,
    source_sheets: &[WorkbookSheet],
    sheet_ids: &[SheetId],
    tables: &BTreeMap<String, TableId>,
    table_headers: &BTreeMap<TableId, BTreeSet<String>>,
    report: &mut ConversionReport,
) -> Result<ImportedNames, ConvertError> {
    let table_ids: BTreeSet<_> = tables.values().map(|id| id.as_str().to_owned()).collect();
    let mut source_names = BTreeSet::new();
    let mut result = ImportedNames::default();
    for source in names {
        if is_builtin_defined_name(&source.name) {
            report.omit(
                ConversionEvent::new(
                    ConversionFeature::Name,
                    "global built-in XLSX defined name is not represented in Marksheet",
                )
                .at(xlsx_location("xl/workbook.xml", Some(&source.name))),
            );
            continue;
        }
        let folded = source.name.to_lowercase();
        if !source_names.insert(folded.clone()) {
            return Err(invalid(
                "xl/workbook.xml",
                "duplicate case-insensitive defined name",
            ));
        }
        if tables.contains_key(&folded) {
            return Err(invalid(
                "xl/workbook.xml",
                "defined names and table display names share a case-insensitive namespace",
            ));
        }
        // The identifier is claimed even when the target turns out to be
        // unsupported, so a later name cannot silently occupy the spelling an
        // existing formula body uses for the omitted one.
        let id = name_ids.get(&folded).cloned().ok_or_else(|| {
            ConvertError::new(
                ConvertErrorCode::Internal,
                format!(
                    "defined name {:?} was not assigned an identifier",
                    source.name
                ),
            )
        })?;
        // A collision between the identifier namespaces is a property of the
        // package, not of one name's target, so it stays fatal and is checked
        // before the target is resolved: an omitted name still claims its
        // identifier, and every formula body that reached it has already been
        // rewritten to spell that identifier.
        if table_ids.contains(id.as_str()) {
            return Err(invalid(
                "xl/workbook.xml",
                "defined name and table IDs collide after identifier normalization",
            ));
        }
        let target =
            match resolve_name_target(source, source_sheets, sheet_ids, tables, table_headers) {
                Ok(target) => target,
                Err(reason) => {
                    report.omit(
                        ConversionEvent::new(
                            ConversionFeature::Name,
                            format!("defined name {:?} was not imported: {reason}", source.name),
                        )
                        .at(xlsx_location("xl/workbook.xml", Some(&source.name))),
                    );
                    // Keyed by the assigned identifier, not the XLSX spelling:
                    // that is what a translated formula body now says.
                    result.omitted.insert(id);
                    continue;
                }
            };
        if id.as_str() == source.name {
            report.exact_event(
                ConversionEvent::new(ConversionFeature::Name, "defined name target was imported")
                    .at(xlsx_location("xl/workbook.xml", Some(&source.name))),
            );
        } else {
            report.approximate(
                ConversionEvent::new(
                    ConversionFeature::Name,
                    format!(
                        "defined name {:?} was assigned Marksheet ID {id}",
                        source.name
                    ),
                )
                .at(xlsx_location("xl/workbook.xml", Some(&source.name))),
            );
        }
        result.names.push(Name {
            id,
            target,
            origin: None,
        });
    }
    Ok(result)
}

/// Resolves one defined-name target, or explains why Marksheet cannot express
/// it. Unsupported shapes are a property of the individual name, so the caller
/// omits that name and keeps importing the rest of the package.
fn resolve_name_target(
    source: &DefinedName,
    source_sheets: &[WorkbookSheet],
    sheet_ids: &[SheetId],
    tables: &BTreeMap<String, TableId>,
    table_headers: &BTreeMap<TableId, BTreeSet<String>>,
) -> Result<NameTarget, String> {
    if let Some((sheet, area)) = split_sheet_reference(&source.expression) {
        let sheet_index = source_sheets
            .iter()
            .position(|candidate| candidate.label.eq_ignore_ascii_case(&sheet))
            .ok_or_else(|| format!("it refers to unknown sheet {sheet:?}"))?;
        let range = Range::parse(&area.replace('$', ""))
            .map_err(|_| "it is not a finite A1 target".to_owned())?;
        if !coordinate_in_xlsx_grid(range.start) || !coordinate_in_xlsx_grid(range.end) {
            return Err("its target exceeds the XLSX grid".to_owned());
        }
        if range.start == range.end {
            Ok(NameTarget::Cell(SheetCoordinate {
                sheet: sheet_ids[sheet_index].clone(),
                coordinate: range.start,
            }))
        } else {
            Ok(NameTarget::Range(SheetRange {
                sheet: sheet_ids[sheet_index].clone(),
                range,
            }))
        }
    } else if let Some((table, header)) = parse_structured_name(&source.expression) {
        let table_id = tables
            .get(&table.to_lowercase())
            .ok_or_else(|| format!("it refers to unknown table {table}"))?;
        if !table_headers
            .get(table_id)
            .is_some_and(|headers| headers.contains(&header))
        {
            return Err(format!(
                "it refers to missing header {header:?} on table {table}"
            ));
        }
        Ok(NameTarget::TableColumn {
            table: table_id.clone(),
            header,
        })
    } else {
        Err("it is not a supported cell, range, or table column".to_owned())
    }
}

/// What one table calculated column already claimed in the report before
/// defined names resolved.
///
/// Defined-name targets resolve only after every sheet has been read, so a
/// fill that turns out to reach an omitted name has to reopen the exact `@fill`
/// outcome recorded at its source `xl/tables` part, and the per-cell
/// translation outcomes of the body cells the fill absorbed — those cells hold
/// the formula this pass is about to destroy.
#[derive(Clone, Debug)]
struct FillOutcome {
    location: ConversionLocation,
    sheet: SheetId,
    body: Range,
}

impl FillOutcome {
    /// True for every report location the fill's own outcome superseded.
    fn covers(&self, candidate: &ConversionLocation) -> bool {
        match candidate {
            ConversionLocation::Cell { sheet, cell } => {
                *sheet == self.sheet
                    && Coordinate::parse(cell)
                        .is_ok_and(|coordinate| self.body.contains(coordinate))
            }
            other => *other == self.location,
        }
    }
}

type FillOutcomes = BTreeMap<(TableId, String), FillOutcome>;

/// Rewrites formulas that reach a defined name the importer omitted.
///
/// The portable evaluator resolves such a reference to `#NAME?`, and keeping
/// the unresolved spelling would make the workbook impossible to export back
/// to XLSX, so the importer substitutes the same typed error it already uses
/// for other unsupported formula content and reports every substitution.
///
/// Defined-name targets resolve only once every sheet has been read, so each
/// rewritten formula already carries the outcome the first pass recorded for
/// it — "translated to portable-a1@1", or an exact `@fill`. That claim is no
/// longer true of a formula this pass destroys, so it is retracted before the
/// replacement is recorded; otherwise the report would assert both.
fn replace_formulas_referencing_omitted_names(
    sheets: &mut [Sheet],
    omitted: &BTreeSet<NameId>,
    fill_outcomes: &FillOutcomes,
    limits: ConversionLimits,
    report: &mut ConversionReport,
) -> Result<(), ConvertError> {
    let unresolved = FormulaSource::new(format!("={}", CellError::Name.token()))
        .map_err(|error| ConvertError::new(ConvertErrorCode::Internal, error.to_string()))?;
    for sheet in sheets {
        let sheet_id = sheet.id.clone();
        for item in &mut sheet.items {
            match item {
                SheetItem::Block(block) => {
                    replace_block_formulas(block, &sheet_id, omitted, limits, report)?;
                }
                SheetItem::Table(table) => {
                    replace_block_formulas(&mut table.block, &sheet_id, omitted, limits, report)?;
                }
                SheetItem::Fill(fill) => {
                    if !references_omitted_name(fill.formula.as_str(), omitted, limits) {
                        continue;
                    }
                    let location = match &fill.target {
                        FillTarget::Range(range) => {
                            ConversionLocation::range(sheet_id.clone(), *range)
                        }
                        FillTarget::TableColumn { table, header } => {
                            if let Some(recorded) =
                                fill_outcomes.get(&(table.clone(), header.clone()))
                            {
                                report.retract(ConversionFeature::Formula, |candidate| {
                                    recorded.covers(candidate)
                                });
                                recorded.location.clone()
                            } else {
                                ConversionLocation::table_on_sheet(sheet_id.clone(), table.clone())
                            }
                        }
                    };
                    report.approximate(omitted_name_reference_event().at(location));
                    fill.formula = unresolved.clone();
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn replace_block_formulas(
    block: &mut Block,
    sheet: &SheetId,
    omitted: &BTreeSet<NameId>,
    limits: ConversionLimits,
    report: &mut ConversionReport,
) -> Result<(), ConvertError> {
    let anchor = block.anchor;
    for (row_offset, row) in block.cells.iter_mut().enumerate() {
        for (column_offset, cell) in row.iter_mut().enumerate() {
            let Value::Formula(formula) = &cell.value else {
                continue;
            };
            if !references_omitted_name(formula.as_str(), omitted, limits) {
                continue;
            }
            let coordinate = cell_offset(anchor, column_offset, row_offset)?;
            let location = ConversionLocation::cell(sheet.clone(), coordinate);
            // The first pass reported this body as translated; that outcome
            // does not survive the substitution below.
            report.retract(ConversionFeature::Formula, |candidate| {
                *candidate == location
            });
            report.approximate(omitted_name_reference_event().at(location));
            cell.value = Value::Error(CellError::Name);
        }
    }
    Ok(())
}

fn cell_offset(
    anchor: Coordinate,
    columns: usize,
    rows: usize,
) -> Result<Coordinate, ConvertError> {
    let internal = |message: String| ConvertError::new(ConvertErrorCode::Internal, message);
    let columns = u64::try_from(columns).map_err(|error| internal(error.to_string()))?;
    let rows = u64::try_from(rows).map_err(|error| internal(error.to_string()))?;
    anchor
        .offset(columns, rows)
        .map_err(|error| internal(error.to_string()))
}

fn omitted_name_reference_event() -> ConversionEvent {
    ConversionEvent::new(
        ConversionFeature::Formula,
        format!(
            "formula referencing an omitted defined name was replaced with {}",
            CellError::Name.token()
        ),
    )
    .formula(FormulaDisposition::Replaced)
}

fn references_omitted_name(
    source: &str,
    omitted: &BTreeSet<NameId>,
    limits: ConversionLimits,
) -> bool {
    parse_portable_formula(source, limits)
        .is_ok_and(|formula| expression_references_omitted_name(&formula.expression, omitted))
}

fn expression_references_omitted_name(expression: &Expr, omitted: &BTreeSet<NameId>) -> bool {
    match &expression.kind {
        ExprKind::Literal { .. } => false,
        ExprKind::Reference { reference } => matches!(
            reference,
            marksheet_calc::formula::Reference::Name { name } if omitted.contains(name)
        ),
        ExprKind::Unary { operand, .. } => expression_references_omitted_name(operand, omitted),
        ExprKind::Binary { left, right, .. } => {
            expression_references_omitted_name(left, omitted)
                || expression_references_omitted_name(right, omitted)
        }
        ExprKind::Call { call } => call
            .arguments
            .iter()
            .any(|argument| expression_references_omitted_name(argument, omitted)),
    }
}

fn reject_case_insensitive_duplicates<'a>(
    values: impl IntoIterator<Item = &'a str>,
    part: &str,
    kind: &str,
) -> Result<(), ConvertError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value.to_lowercase()) {
            return Err(invalid(part, &format!("duplicate case-insensitive {kind}")));
        }
    }
    Ok(())
}

fn derive_sheet_ids(sheets: &[WorkbookSheet], report: &mut ConversionReport) -> Vec<SheetId> {
    let mut used = BTreeSet::new();
    sheets
        .iter()
        .map(|sheet| {
            let id = unique_identifier::<SheetId>(&sheet.label, "sheet", &mut used)
                .expect("fallback sheet identifier is valid");
            if id.as_str() != sheet.label {
                report.approximate(
                    ConversionEvent::new(
                        ConversionFeature::Sheet,
                        format!("sheet label {:?} was assigned stable ID {id}", sheet.label),
                    )
                    .at(xlsx_location("xl/workbook.xml", Some(&sheet.label))),
                );
            }
            id
        })
        .collect()
}

trait TypedId: Sized {
    fn parse_id(value: &str) -> Result<Self, marksheet_model::IdentifierError>;
}
macro_rules! typed_id {
    ($type:ty) => {
        impl TypedId for $type {
            fn parse_id(value: &str) -> Result<Self, marksheet_model::IdentifierError> {
                Self::parse(value)
            }
        }
    };
}
typed_id!(SheetId);
typed_id!(TableId);
typed_id!(NameId);

fn unique_identifier<T: TypedId>(
    source: &str,
    fallback: &str,
    used: &mut BTreeSet<String>,
) -> Result<T, ConvertError> {
    let mut base = String::new();
    for character in source.chars().flat_map(char::to_lowercase) {
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
        fallback.clone_into(&mut base);
    }
    let mut candidate = base.clone();
    for suffix in 2_u64.. {
        if !used.contains(&candidate) {
            let id = T::parse_id(&candidate).map_err(|error| {
                ConvertError::new(ConvertErrorCode::Internal, error.to_string())
            })?;
            used.insert(candidate);
            return Ok(id);
        }
        candidate = format!("{base}_{suffix}");
    }
    unreachable!()
}

fn rectangular_cells(
    cells: &BTreeMap<Coordinate, ImportedCell>,
    range: Range,
    limits: ConversionLimits,
) -> Result<Vec<Vec<Cell>>, ConvertError> {
    let area = range
        .width()
        .and_then(|width| range.height().map(|height| (width, height)))
        .map_err(|error| ConvertError::new(ConvertErrorCode::InvalidPackage, error.to_string()))?;
    if area
        .0
        .checked_mul(area.1)
        .is_none_or(|count| count > limits.max_cells)
    {
        return Err(ConvertError::new(
            ConvertErrorCode::ResourceLimit,
            "table range exceeds the configured cell limit",
        ));
    }
    let mut rows = Vec::new();
    for row in range.start.row..=range.end.row {
        let mut values = Vec::new();
        for column in range.start.column..=range.end.column {
            values.push(Cell::new(
                cells
                    .get(&Coordinate { column, row })
                    .map_or(Value::Blank, |cell| cell.value.clone()),
            ));
        }
        rows.push(values);
    }
    Ok(rows)
}

fn read_row_geometry(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    output: &mut Vec<RowGeometry>,
) -> Result<(), ConvertError> {
    let Some(height) = attribute(reader, element, b"ht", part)? else {
        return Ok(());
    };
    let row = required_attribute(reader, element, b"r", part)?
        .parse::<u64>()
        .map_err(|_| invalid(part, "row index is invalid"))?;
    if row > 1_048_576 {
        return Err(invalid(part, "row geometry exceeds the XLSX grid"));
    }
    let height = height
        .parse::<f64>()
        .map_err(|_| invalid(part, "row height is invalid"))?;
    if !height.is_finite() || height <= 0.0 {
        return Err(invalid(part, "row height must be finite and positive"));
    }
    output.push(RowGeometry {
        rows: RowRange::new(row, row).map_err(|error| invalid(part, &error.to_string()))?,
        height,
        origin: None,
    });
    Ok(())
}

fn record_unsupported_dimension_attributes(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    feature: &str,
    output: &mut BTreeSet<String>,
) -> Result<(), ConvertError> {
    for name in [
        b"hidden".as_slice(),
        b"outlineLevel".as_slice(),
        b"collapsed".as_slice(),
        b"s".as_slice(),
        b"style".as_slice(),
        b"customFormat".as_slice(),
        b"bestFit".as_slice(),
    ] {
        if attribute(reader, element, name, part)?.is_some() {
            output.insert(feature.to_owned());
            break;
        }
    }
    Ok(())
}

fn record_unsupported_worksheet_element(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    output: &mut BTreeSet<String>,
) -> Result<(), ConvertError> {
    let qualified_name = element.name();
    let name = local_name(qualified_name.as_ref());
    let allowed_attributes: &[&[u8]] = match name {
        b"dimension" => &[b"ref"],
        b"row" => &[b"r", b"ht", b"customHeight"],
        b"col" => &[b"min", b"max", b"width", b"customWidth"],
        b"c" => &[b"r", b"t", b"s"],
        b"f" => &[b"t"],
        b"t" => &[b"space"],
        b"tableParts" => &[b"count"],
        b"tablePart" => &[b"id"],
        _ => &[],
    };
    match name {
        b"sheetViews" | b"sheetView" | b"pane" | b"selection" => {
            output.insert("sheet_views".to_owned());
        }
        b"worksheet"
        | b"dimension"
        | b"sheetData"
        | b"row"
        | b"cols"
        | b"col"
        | b"c"
        | b"v"
        | b"f"
        | b"is"
        | b"t"
        | b"tableParts"
        | b"tablePart"
        | b"mergeCells"
        | b"mergeCell"
        | b"conditionalFormatting"
        | b"cfRule"
        | b"drawing"
        | b"dataValidations"
        | b"dataValidation"
        | b"hyperlinks"
        | b"hyperlink"
        | b"sheetProtection"
        | b"protectedRanges"
        | b"protectedRange"
        | b"autoFilter"
        | b"filterColumn"
        | b"filters"
        | b"filter"
        | b"legacyDrawing"
        | b"oleObjects"
        | b"oleObject"
        | b"controls"
        | b"control"
        | b"extLst"
        | b"ext" => {}
        b"r" | b"rPr" => {
            output.insert("inline_rich_text".to_owned());
        }
        _ => {
            output.insert("unknown_worksheet_content".to_owned());
        }
    }
    if element_has_unsupported_attributes(element, allowed_attributes, part)? {
        output.insert("unsupported_worksheet_attributes".to_owned());
    }
    if name == b"row"
        && attribute(reader, element, b"customHeight", part)?
            .map(|value| parse_bool(&value, part))
            .transpose()?
            == Some(false)
    {
        output.insert("row_attributes".to_owned());
    }
    if name == b"col"
        && attribute(reader, element, b"customWidth", part)?
            .map(|value| parse_bool(&value, part))
            .transpose()?
            == Some(false)
    {
        output.insert("column_attributes".to_owned());
    }
    Ok(())
}

fn element_has_unsupported_attributes(
    element: &quick_xml::events::BytesStart<'_>,
    allowed: &[&[u8]],
    part: &str,
) -> Result<bool, ConvertError> {
    for candidate in element.attributes() {
        let candidate = candidate
            .map_err(|error| invalid(part, &format!("malformed XML attribute: {error}")))?;
        let key = candidate.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        let local = local_name(key);
        if !allowed.contains(&local) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Rejects namespace aliases that the local-name semantic parsers could otherwise
/// mistake for `SpreadsheetML`. Namespace-free XML remains accepted for the small
/// parser-level fixtures used in this module; packaged XLSX parts use the bound
/// transitional or strict `SpreadsheetML` namespace.
fn validate_consumed_part_namespaces(bytes: &[u8], part: &str) -> Result<(), ConvertError> {
    let mut reader = NsReader::from_reader(bytes);
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                let (namespace, element_local) = reader.resolver().resolve_element(element.name());
                if !supported_element_namespace(&namespace) {
                    return Err(ConvertError::new(
                        ConvertErrorCode::UnsupportedPackage,
                        "foreign or unresolved XML namespaces are not accepted in consumed XLSX parts",
                    )
                    .at(xlsx_location(part, None)));
                }
                for candidate in element.attributes() {
                    let candidate = candidate.map_err(|error| {
                        invalid(part, &format!("malformed XML attribute: {error}"))
                    })?;
                    let key = candidate.key.as_ref();
                    if key == b"xmlns" || key.starts_with(b"xmlns:") {
                        continue;
                    }
                    let (namespace, attribute_local) =
                        reader.resolver().resolve_attribute(candidate.key);
                    if !supported_attribute_namespace(
                        &namespace,
                        attribute_local.as_ref(),
                        element_local.as_ref(),
                    ) {
                        return Err(ConvertError::new(
                            ConvertErrorCode::UnsupportedPackage,
                            "foreign or unresolved XML attribute namespaces are not accepted in consumed XLSX parts",
                        )
                        .at(xlsx_location(part, None)));
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(invalid(
                    part,
                    &format!("malformed XML during namespace validation: {error}"),
                ));
            }
        }
    }
    Ok(())
}

fn supported_element_namespace(namespace: &ResolveResult<'_>) -> bool {
    match namespace {
        ResolveResult::Unbound => true,
        ResolveResult::Bound(namespace) => {
            matches!(namespace.0, SPREADSHEETML_NS | STRICT_SPREADSHEETML_NS)
        }
        ResolveResult::Unknown(_) => false,
    }
}

fn supported_attribute_namespace(
    namespace: &ResolveResult<'_>,
    attribute: &[u8],
    element: &[u8],
) -> bool {
    match namespace {
        ResolveResult::Unbound => true,
        ResolveResult::Bound(namespace) if namespace.0 == XML_NS => attribute == b"space",
        ResolveResult::Bound(namespace)
            if matches!(
                namespace.0,
                OFFICE_RELATIONSHIPS_NS | STRICT_OFFICE_RELATIONSHIPS_NS
            ) =>
        {
            attribute == b"id"
                && matches!(
                    element,
                    b"sheet"
                        | b"tablePart"
                        | b"drawing"
                        | b"legacyDrawing"
                        | b"oleObject"
                        | b"control"
                )
        }
        ResolveResult::Bound(_) | ResolveResult::Unknown(_) => false,
    }
}

fn read_column_geometry(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    output: &mut Vec<ColumnGeometry>,
) -> Result<(), ConvertError> {
    let Some(width) = attribute(reader, element, b"width", part)? else {
        return Ok(());
    };
    let start = required_attribute(reader, element, b"min", part)?
        .parse::<u64>()
        .map_err(|_| invalid(part, "column minimum is invalid"))?;
    let end = required_attribute(reader, element, b"max", part)?
        .parse::<u64>()
        .map_err(|_| invalid(part, "column maximum is invalid"))?;
    if start > 16_384 || end > 16_384 {
        return Err(invalid(part, "column geometry exceeds the XLSX grid"));
    }
    let width = width
        .parse::<f64>()
        .map_err(|_| invalid(part, "column width is invalid"))?;
    if !width.is_finite() || width <= 0.0 {
        return Err(invalid(part, "column width must be finite and positive"));
    }
    output.push(ColumnGeometry {
        columns: ColumnRange::new(start, end).map_err(|error| invalid(part, &error.to_string()))?,
        width,
        origin: None,
    });
    Ok(())
}

fn insert_cell(
    worksheet: &mut WorksheetData,
    coordinate: Coordinate,
    value: Value,
    style: usize,
    styles: &[StyleProperties],
    part: &str,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    if style >= styles.len() {
        return Err(invalid(part, "cell style index is out of bounds"));
    }
    let record_count = worksheet
        .cells
        .len()
        .checked_add(worksheet.style_only.len())
        .ok_or_else(|| resource(part, "worksheet cell record count overflow"))?;
    if record_count >= usize::try_from(limits.max_cells).unwrap_or(usize::MAX) {
        return Err(resource(
            part,
            "worksheet cell count exceeds the configured limit",
        ));
    }
    if worksheet.style_only.contains_key(&coordinate)
        || worksheet
            .cells
            .insert(coordinate, ImportedCell { value, style })
            .is_some()
    {
        return Err(invalid(part, "duplicate cell coordinate"));
    }
    Ok(())
}

fn insert_style_only(
    worksheet: &mut WorksheetData,
    coordinate: Coordinate,
    style: usize,
    styles: &[StyleProperties],
    part: &str,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    if style >= styles.len() {
        return Err(invalid(part, "cell style index is out of bounds"));
    }
    if style == 0
        && styles
            .first()
            .is_some_and(|properties| *properties == StyleProperties::default())
    {
        // An empty OOXML record that inherits an unstyled `cellXfs[0]` has no
        // Marksheet semantic effect. A non-default default format does.
        return Ok(());
    }
    let record_count = worksheet
        .cells
        .len()
        .checked_add(worksheet.style_only.len())
        .ok_or_else(|| resource(part, "worksheet cell record count overflow"))?;
    if record_count >= usize::try_from(limits.max_cells).unwrap_or(usize::MAX) {
        return Err(resource(
            part,
            "worksheet cell count exceeds the configured limit",
        ));
    }
    if worksheet.cells.contains_key(&coordinate)
        || worksheet.style_only.insert(coordinate, style).is_some()
    {
        return Err(invalid(part, "duplicate cell coordinate"));
    }
    Ok(())
}

fn excel_serial(
    value: f64,
    force_datetime: bool,
    date_1904: bool,
    part: &str,
) -> Result<Value, ConvertError> {
    if value < 0.0 {
        return Err(ConvertError::new(
            ConvertErrorCode::UnsupportedPackage,
            "negative Excel date serial is outside the initial profile",
        ));
    }
    let whole_float = value.floor();
    let whole = format!("{whole_float:.0}")
        .parse::<i64>()
        .map_err(|_| invalid(part, "Excel date serial is out of range"))?;
    if !date_1904 && whole == 60 {
        return Err(ConvertError::new(
            ConvertErrorCode::UnsupportedPackage,
            "Excel's fictitious 1900-02-29 cannot be represented",
        ));
    }
    let adjusted = if !date_1904 && whole > 60 {
        whole - 1
    } else {
        whole
    };
    let (epoch_year, epoch_month, epoch_day) = if date_1904 {
        (1904, Month::January, 1)
    } else {
        (1899, Month::December, 31)
    };
    let epoch = Date::from_calendar_date(epoch_year, epoch_month, epoch_day)
        .map_err(|error| invalid(part, &error.to_string()))?;
    let date = epoch
        .checked_add(Duration::days(adjusted))
        .ok_or_else(|| invalid(part, "Excel date serial is out of range"))?;
    let fraction = value - value.floor();
    if !force_datetime && fraction.abs() < f64::EPSILON {
        return Ok(Value::Date(date));
    }
    let nanoseconds = format!("{:.0}", (fraction * 86_400_000_000_000.0).round())
        .parse::<u64>()
        .map_err(|_| invalid(part, "Excel datetime fraction is out of range"))?;
    let hour = u8::try_from(nanoseconds / 3_600_000_000_000)
        .map_err(|_| invalid(part, "datetime hour overflow"))?;
    let remainder = nanoseconds % 3_600_000_000_000;
    let minute = u8::try_from(remainder / 60_000_000_000)
        .map_err(|_| invalid(part, "datetime minute overflow"))?;
    let remainder = remainder % 60_000_000_000;
    let second = u8::try_from(remainder / 1_000_000_000)
        .map_err(|_| invalid(part, "datetime second overflow"))?;
    let nano = u32::try_from(remainder % 1_000_000_000)
        .map_err(|_| invalid(part, "datetime nanosecond overflow"))?;
    let time = Time::from_hms_nano(hour, minute, second, nano)
        .map_err(|error| invalid(part, &error.to_string()))?;
    Ok(Value::DateTime(
        PrimitiveDateTime::new(date, time).assume_utc(),
    ))
}

fn update_font_component(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    part: &str,
    font: &mut StyleProperties,
) -> Result<(), ConvertError> {
    match local_name(element.name().as_ref()) {
        b"b" => {
            font.bold = Some(
                attribute(reader, element, b"val", part)?
                    .map_or(Ok(true), |value| parse_bool(&value, part))?,
            );
        }
        b"i" => {
            font.italic = Some(
                attribute(reader, element, b"val", part)?
                    .map_or(Ok(true), |value| parse_bool(&value, part))?,
            );
        }
        b"sz" => {
            font.font_size = Some(
                required_attribute(reader, element, b"val", part)?
                    .parse::<f64>()
                    .map_err(|_| invalid(part, "font size is invalid"))?,
            );
        }
        b"color" => {
            if let Some(rgb) = attribute(reader, element, b"rgb", part)? {
                font.text_color = Some(parse_argb(&rgb, part)?);
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_argb(value: &str, part: &str) -> Result<Color, ConvertError> {
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(part, "style RGB color must be AARRGGBB"));
    }
    let css = if &value[..2].to_ascii_uppercase() == "FF" {
        format!("#{}", &value[2..])
    } else {
        format!("#{}{}", &value[2..], &value[..2])
    };
    Color::parse(&css).map_err(|error| invalid(part, &error.to_string()))
}

fn append_text(
    output: &mut String,
    text: &quick_xml::events::BytesText<'_>,
    part: &str,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let decoded = text
        .decode()
        .map_err(|error| invalid(part, &format!("invalid text encoding: {error}")))?;
    let decoded = unescape(&decoded)
        .map_err(|error| invalid(part, &format!("invalid XML entity: {error}")))?;
    append_bounded(output, &decoded, part, limits)
}

/// Appends the character an entity or character reference stands for.
///
/// quick-xml reports every `&...;` in character data as its own event rather
/// than folding it into the surrounding text, so a reader that only handles
/// [`Event::Text`] silently drops the referenced character.
fn append_reference(
    output: &mut String,
    reference: &quick_xml::events::BytesRef<'_>,
    part: &str,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let resolved = if let Some(character) = reference
        .resolve_char_ref()
        .map_err(|error| invalid(part, &format!("invalid XML character reference: {error}")))?
    {
        character
    } else {
        let name = reference
            .decode()
            .map_err(|error| invalid(part, &format!("invalid text encoding: {error}")))?;
        // OOXML parts declare no DTD, so only the five predefined general
        // entities can appear.
        match name.as_ref() {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "apos" => '\'',
            "quot" => '"',
            _ => return Err(invalid(part, &format!("unknown XML entity &{name};"))),
        }
    };
    if !is_xml_character(resolved) {
        return Err(invalid(
            part,
            "XML character reference resolves to a character XML forbids",
        ));
    }
    append_bounded(output, resolved.encode_utf8(&mut [0_u8; 4]), part, limits)
}

fn append_bounded(
    output: &mut String,
    value: &str,
    part: &str,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let length = output
        .len()
        .checked_add(value.len())
        .ok_or_else(|| resource(part, "text length overflow"))?;
    if length > limits.max_string_bytes {
        return Err(resource(part, "text exceeds the configured string limit"));
    }
    output.push_str(value);
    Ok(())
}

fn parse_usize_attribute(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    part: &str,
) -> Result<Option<usize>, ConvertError> {
    attribute(reader, element, name, part)?
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| invalid(part, "integer attribute is invalid"))
        })
        .transpose()
}
fn parse_u32_attribute(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    part: &str,
) -> Result<Option<u32>, ConvertError> {
    attribute(reader, element, name, part)?
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|_| invalid(part, "integer attribute is invalid"))
        })
        .transpose()
}
fn parse_bool(value: &str, part: &str) -> Result<bool, ConvertError> {
    match value {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(invalid(part, "Boolean attribute is invalid")),
    }
}

fn split_sheet_reference(expression: &str) -> Option<(String, String)> {
    let (sheet, range) = expression.rsplit_once('!')?;
    let sheet = if sheet.starts_with('\'') && sheet.ends_with('\'') {
        sheet[1..sheet.len() - 1].replace("''", "'")
    } else {
        sheet.to_owned()
    };
    Some((sheet, range.to_owned()))
}

/// Rewrites an Excel formula body into portable-a1@1 source, replacing sheet
/// labels and defined-name references with the identifiers the importer
/// assigned them. Excel spells both case-insensitively while portable-a1@1
/// requires the exact lowercase identifier, so a body that is not rewritten
/// would fail to parse — even when the name it reaches was imported.
fn translate_excel_formula(formula: &str, names: FormulaNames<'_>) -> String {
    let sheet_labels = names.sheets;
    let mut output = String::from("=");
    let bytes = formula.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'"' {
                    index += 1;
                    if index < bytes.len() && bytes[index] == b'"' {
                        index += 1;
                        continue;
                    }
                    break;
                }
                index += 1;
            }
            output.push_str(&formula[start..index]);
            continue;
        }
        if bytes[index] == b'\'' {
            let start = index;
            index += 1;
            let mut label = String::new();
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        label.push('\'');
                        index += 2;
                    } else {
                        index += 1;
                        closed = true;
                        break;
                    }
                } else {
                    let character = formula[index..]
                        .chars()
                        .next()
                        .expect("index is within UTF-8 formula");
                    label.push(character);
                    index += character.len_utf8();
                }
            }
            if closed && bytes.get(index) == Some(&b'!') {
                if let Some(sheet) = sheet_labels.get(&label.to_lowercase()) {
                    output.push_str(sheet.as_str());
                    output.push('!');
                } else {
                    output.push_str(&formula[start..=index]);
                }
                index += 1;
                continue;
            }
            output.push_str(&formula[start..index]);
            continue;
        }
        // Structured selectors contain exact header data, not formula name
        // tokens. Copy the selector spelling wholesale so an identically
        // spelled defined name cannot rewrite its header.
        if bytes[index] == b'[' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b']' {
                    if bytes.get(index + 1) == Some(&b']') {
                        index += 2;
                        continue;
                    }
                    index += 1;
                    break;
                }
                index += 1;
            }
            output.push_str(&formula[start..index]);
            continue;
        }
        // Error literals have word-shaped bodies (`#REF!`, `#NAME?`, ...),
        // but those bodies are never defined-name references.
        if bytes[index] == b'#' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'/'))
            {
                index += 1;
            }
            if matches!(bytes.get(index), Some(b'!' | b'?')) {
                index += 1;
            }
            output.push_str(&formula[start..index]);
            continue;
        }
        let starts_name = bytes[index].is_ascii_alphabetic()
            || bytes[index] == b'_'
            || (bytes[index] == b'\\'
                && bytes
                    .get(index + 1)
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_'));
        if starts_name {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'.'))
            {
                index += 1;
            }
            if bytes.get(index) == Some(&b'!') {
                let label = &formula[start..index];
                if let Some(sheet) = sheet_labels.get(&label.to_lowercase()) {
                    output.push_str(sheet.as_str());
                    output.push('!');
                    index += 1;
                    continue;
                }
            }
            let word = &formula[start..index];
            // A word that opens a call or a structured selector is a function
            // or a table, never a defined-name reference, even when a name
            // happens to share its spelling.
            let renamed = names
                .names
                .get(&word.to_lowercase())
                .filter(|_| !word_opens_call_or_selector(bytes, index));
            output.push_str(renamed.map_or(word, NameId::as_str));
            continue;
        }
        let character = formula[index..]
            .chars()
            .next()
            .expect("index is within UTF-8 formula");
        output.push(character);
        index += character.len_utf8();
    }
    output
}

/// True when the token that follows the word ending at `index` makes that word
/// a function call or a structured reference.
fn word_opens_call_or_selector(bytes: &[u8], index: usize) -> bool {
    bytes[index..]
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| matches!(byte, b'(' | b'['))
}

fn parse_structured_name(expression: &str) -> Option<(String, String)> {
    let (table, header) = expression.split_once('[')?;
    let header = header.strip_suffix(']')?.replace("]]", "]");
    Some((table.to_owned(), header))
}
fn style_id(index: usize) -> Result<StyleId, ConvertError> {
    StyleId::parse(&format!("xlsx_style_{index}"))
        .map_err(|error| ConvertError::new(ConvertErrorCode::Internal, error.to_string()))
}
fn check_excel_coordinate(coordinate: Coordinate, part: &str) -> Result<(), ConvertError> {
    if coordinate.column > 16_384 || coordinate.row > 1_048_576 {
        return Err(invalid(part, "cell coordinate exceeds the XLSX grid"));
    }
    Ok(())
}

fn record_unconsumed_package_content(
    package: &Package,
    consumed_parts: &BTreeSet<String>,
    limits: ConversionLimits,
    report: &mut ConversionReport,
) -> Result<(), ConvertError> {
    if package.is_macro_enabled() {
        report.omit(
            ConversionEvent::new(
                ConversionFeature::Macro,
                "macro-enabled workbook content type was imported without VBA semantics",
            )
            .at(ConversionLocation::source("xl/workbook.xml")),
        );
    }

    for name in package.names() {
        if consumed_parts.contains(name) {
            continue;
        }
        let folded = name.to_ascii_lowercase();
        let (feature, detail) = if folded.contains("vbaproject") {
            (ConversionFeature::Macro, "VBA macros are not imported")
        } else if folded.starts_with("xl/charts/") {
            (
                ConversionFeature::Chart,
                "charts are outside the initial import profile",
            )
        } else if folded.starts_with("xl/pivot") {
            (
                ConversionFeature::PivotTable,
                "pivot tables are outside the initial import profile",
            )
        } else if folded.starts_with("xl/externallinks/") {
            (
                ConversionFeature::ExternalLink,
                "external link data is not imported",
            )
        } else if folded.contains("drawing") {
            (
                ConversionFeature::Other("drawing".to_owned()),
                "drawing content is outside the initial import profile",
            )
        } else if folded.contains("comment") {
            (
                ConversionFeature::Other("comment".to_owned()),
                "comment content is outside the initial import profile",
            )
        } else if folded.contains("theme") {
            (
                ConversionFeature::Other("theme".to_owned()),
                "theme content is not represented in Marksheet",
            )
        } else {
            (
                ConversionFeature::Other("ooxml_part".to_owned()),
                "unconsumed OOXML package part is not represented in Marksheet",
            )
        };
        report.omit(ConversionEvent::new(feature, detail).at(ConversionLocation::source(name)));
    }

    for record in package.relationship_inventory(limits)? {
        let relationship = &record.relationship;
        let consumed = if relationship.kind.ends_with("/officeDocument") {
            record.source.is_empty() && relationship.target == "xl/workbook.xml"
        } else if relationship.kind.ends_with("/worksheet")
            || relationship.kind.ends_with("/styles")
            || relationship.kind.ends_with("/sharedStrings")
        {
            record.source == "xl/workbook.xml" && consumed_parts.contains(&relationship.target)
        } else if relationship.kind.ends_with("/table") {
            record.source.starts_with("xl/worksheets/")
                && consumed_parts.contains(&record.source)
                && consumed_parts.contains(&relationship.target)
        } else {
            false
        };
        if !consumed {
            report.omit(
                ConversionEvent::new(
                    ConversionFeature::Other("ooxml_relationship".to_owned()),
                    format!(
                        "unconsumed OOXML relationship {} from {:?} to {:?} is not represented",
                        relationship.id, record.source, relationship.target
                    ),
                )
                .at(xlsx_location(&record.rels_part, Some(&relationship.id))),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_xlsx;
    use marksheet_model::{Block, Cell, Fill, FormulaSource, Sheet, Table};
    use std::io::{Cursor, Read, Write};
    use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

    fn rewrite_package(
        bytes: &[u8],
        replacements: &[(&str, &str)],
        additions: &[(&str, &str)],
    ) -> Vec<u8> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut parts = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            let name = entry.name().to_owned();
            if let Some((_, replacement)) = replacements.iter().find(|(part, _)| *part == name) {
                content = replacement.as_bytes().to_vec();
            }
            parts.push((name, content));
        }
        parts.extend(
            additions
                .iter()
                .map(|(name, content)| ((*name).to_owned(), content.as_bytes().to_vec())),
        );
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
        for (name, content) in parts {
            writer.start_file(name, options).unwrap();
            writer.write_all(&content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn package_text(bytes: &[u8], name: &str) -> String {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut entry = archive.by_name(name).unwrap();
        let mut content = String::new();
        entry.read_to_string(&mut content).unwrap();
        content
    }

    /// A workbook with no sheet labels or defined names to rewrite.
    static NO_SHEET_LABELS: BTreeMap<String, SheetId> = BTreeMap::new();
    static NO_DEFINED_NAMES: BTreeMap<String, NameId> = BTreeMap::new();
    const fn no_formula_names() -> FormulaNames<'static> {
        FormulaNames {
            sheets: &NO_SHEET_LABELS,
            names: &NO_DEFINED_NAMES,
        }
    }

    /// Runs the two halves of defined-name import the way `import_xlsx` does:
    /// identifiers are assigned first, then targets are resolved against them.
    fn import_defined_names(
        names: &[DefinedName],
        source_sheets: &[WorkbookSheet],
        sheet_ids: &[SheetId],
        tables: &BTreeMap<String, TableId>,
        table_headers: &BTreeMap<TableId, BTreeSet<String>>,
        report: &mut ConversionReport,
    ) -> Result<ImportedNames, ConvertError> {
        let name_ids = assign_name_identifiers(names)?;
        import_names(
            names,
            &name_ids,
            source_sheets,
            sheet_ids,
            tables,
            table_headers,
            report,
        )
    }

    /// Renames the single worksheet part to `part` and repoints the content
    /// type override and the workbook relationship at the new name, so the
    /// worksheet is reachable only through its relationship role.
    fn disguise_worksheet_part(bytes: &[u8], part: &str, worksheet: &str) -> Vec<u8> {
        let target = part.strip_prefix("xl/").unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut parts = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_owned();
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            let (name, content) = match name.as_str() {
                "xl/worksheets/sheet1.xml" => (part.to_owned(), worksheet.as_bytes().to_vec()),
                "[Content_Types].xml" => {
                    let text = String::from_utf8(content)
                        .unwrap()
                        .replace("/xl/worksheets/sheet1.xml", &format!("/{part}"));
                    (name, text.into_bytes())
                }
                "xl/_rels/workbook.xml.rels" => {
                    let text = String::from_utf8(content).unwrap().replace(
                        "Target=\"worksheets/sheet1.xml\"",
                        &format!("Target=\"{target}\""),
                    );
                    (name, text.into_bytes())
                }
                _ => (name, content),
            };
            parts.push((name, content));
        }
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
        for (name, content) in parts {
            writer.start_file(name, options).unwrap();
            writer.write_all(&content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn basic_workbook() -> Workbook {
        Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
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
        }
    }

    #[test]
    fn exported_workbook_imports_with_scalar_semantics() {
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("B2").unwrap(),
                        vec![vec![
                            Cell::new(Value::Text(String::new())),
                            Cell::new(Value::Blank),
                            Cell::new(Value::Boolean(true)),
                        ]],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
        let values: Vec<_> = imported.value.sheets[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::Block(block) => Some(block.cells[0][0].value.clone()),
                _ => None,
            })
            .collect();
        assert!(values.contains(&Value::Text(String::new())));
        assert!(values.contains(&Value::Blank));
        assert!(values.contains(&Value::Boolean(true)));
    }

    #[test]
    fn formulas_follow_derived_ids_for_quoted_sheet_labels() {
        let source = Workbook {
            sheets: vec![
                Sheet {
                    id: SheetId::parse("input_data").unwrap(),
                    label: "Input Sheet's Data".to_owned(),
                    items: vec![SheetItem::Block(
                        Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![vec![Cell::new(Value::Number(7.0))]],
                        )
                        .unwrap(),
                    )],
                    origin: None,
                },
                Sheet {
                    id: SheetId::parse("summary").unwrap(),
                    label: "Summary".to_owned(),
                    items: vec![SheetItem::Block(
                        Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![vec![Cell::new(Value::Formula(
                                FormulaSource::new("=input_data!A1").unwrap(),
                            ))]],
                        )
                        .unwrap(),
                    )],
                    origin: None,
                },
            ],
            ..Workbook::default()
        };

        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
        assert_eq!(
            imported.value.sheets[0].id,
            SheetId::parse("input_sheet_s_data").unwrap()
        );
        let formula = imported.value.sheets[1]
            .items
            .iter()
            .find_map(|item| match item {
                SheetItem::Block(block) => match &block.cells[0][0].value {
                    Value::Formula(formula) => Some(formula.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert_eq!(formula, "=input_sheet_s_data!A1");
    }

    #[test]
    fn table_calculated_column_round_trips_as_fill() {
        let table_id = TableId::parse("sales").unwrap();
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![
                    SheetItem::Table(Table {
                        id: table_id.clone(),
                        block: Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![
                                vec![
                                    Cell::new(Value::Text("Amount".to_owned())),
                                    Cell::new(Value::Text("Total".to_owned())),
                                ],
                                vec![Cell::new(Value::Number(2.0)), Cell::new(Value::Blank)],
                                vec![Cell::new(Value::Number(3.0)), Cell::new(Value::Blank)],
                            ],
                        )
                        .unwrap(),
                        origin: None,
                    }),
                    SheetItem::Fill(Fill {
                        target: FillTarget::TableColumn {
                            table: table_id,
                            header: "Total".to_owned(),
                        },
                        formula: FormulaSource::new("=[@Amount]*2").unwrap(),
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };

        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        assert!(exported.report.is_lossless());
        let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
        let items = &imported.value.sheets[0].items;
        let table = items
            .iter()
            .find_map(|item| match item {
                SheetItem::Table(table) => Some(table),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.block.cells[1][1].value, Value::Blank);
        assert_eq!(table.block.cells[2][1].value, Value::Blank);
        let fill = items
            .iter()
            .find_map(|item| match item {
                SheetItem::Fill(fill) => Some(fill),
                _ => None,
            })
            .unwrap();
        assert_eq!(fill.formula.as_str(), "=[@Amount]*2");
        assert!(matches!(
            &fill.target,
            FillTarget::TableColumn { table, header }
                if table.as_str() == "sales" && header == "Total"
        ));
    }

    #[test]
    fn styled_absence_stays_distinct_from_authored_blank() {
        use marksheet_calc::{PreparedWorkbook, prepare::PrepareLimits};

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
                    SheetItem::Block(
                        Block::new(
                            Coordinate::parse("B1").unwrap(),
                            vec![vec![Cell::new(Value::Blank)]],
                        )
                        .unwrap(),
                    ),
                    SheetItem::Apply(Apply {
                        target: ApplyTarget::Range(Range::parse("A1:B1").unwrap()),
                        styles: vec![style_id],
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };

        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        assert!(exported.report.is_lossless());
        let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
        let prepared = PreparedWorkbook::build(&imported.value, PrepareLimits::default()).unwrap();
        let sheet = prepared.sheet(&SheetId::parse("data").unwrap()).unwrap();
        assert!(
            sheet
                .authored_cell(Coordinate::parse("A1").unwrap())
                .is_none()
        );
        assert_eq!(
            sheet
                .authored_cell(Coordinate::parse("B1").unwrap())
                .unwrap()
                .cell
                .value,
            Value::Blank
        );
    }

    #[test]
    fn syntax_valid_unsupported_functions_are_replaced() {
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let value = cell_value(
            None,
            "0",
            "XLOOKUP(A1,B1:B2,C1:C2)",
            true,
            None,
            "",
            &StyleProperties::default(),
            &[],
            &SheetId::parse("data").unwrap(),
            no_formula_names(),
            false,
            Coordinate::parse("A1").unwrap(),
            "xl/worksheets/sheet1.xml",
            ConversionLimits::default(),
            &mut report,
        )
        .unwrap();
        assert_eq!(value, Value::Error(CellError::Name));
        assert!(!report.finish().is_lossless());

        let parsed = parse_portable_formula("=SUM(A1,XFE1)", ConversionLimits::default()).unwrap();
        assert!(!formula_references_in_xlsx_grid(&parsed.expression));
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        assert!(
            cell_value(
                None,
                "0",
                "SUM(A1,XFE1)",
                true,
                None,
                "",
                &StyleProperties::default(),
                &[],
                &SheetId::parse("data").unwrap(),
                no_formula_names(),
                false,
                Coordinate::parse("A1").unwrap(),
                "xl/worksheets/sheet1.xml",
                ConversionLimits::default(),
                &mut report,
            )
            .is_err()
        );
    }

    #[test]
    fn wrong_arity_if_is_replaced_with_value_error() {
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let value = cell_value(
            None,
            "0",
            "IF(TRUE,1)",
            true,
            None,
            "",
            &StyleProperties::default(),
            &[],
            &SheetId::parse("data").unwrap(),
            no_formula_names(),
            false,
            Coordinate::parse("A1").unwrap(),
            "xl/worksheets/sheet1.xml",
            ConversionLimits::default(),
            &mut report,
        )
        .unwrap();
        assert_eq!(value, Value::Error(CellError::Value));
        let report = report.finish();
        assert!(!report.is_lossless());
        assert!(report.outcomes().iter().any(|outcome| {
            outcome.feature == "portable_formulas"
                && outcome.outcome == crate::FeatureOutcome::Approximated
        }));
    }

    #[test]
    fn iso_date_cells_preserve_date_only_and_offset_datetime_values() {
        let sheet = SheetId::parse("data").unwrap();
        let coordinate = Coordinate::parse("A1").unwrap();
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let date = cell_value(
            Some("d"),
            "2026-08-17",
            "",
            false,
            None,
            "",
            &StyleProperties::default(),
            &[],
            &sheet,
            no_formula_names(),
            false,
            coordinate,
            "xl/worksheets/sheet1.xml",
            ConversionLimits::default(),
            &mut report,
        )
        .unwrap();
        assert_eq!(
            date,
            Value::Date(Date::from_calendar_date(2026, Month::August, 17).unwrap())
        );

        let timestamp = "2026-08-17T12:34:56+05:30";
        let datetime = cell_value(
            Some("d"),
            timestamp,
            "",
            false,
            None,
            "",
            &StyleProperties::default(),
            &[],
            &sheet,
            no_formula_names(),
            false,
            coordinate,
            "xl/worksheets/sheet1.xml",
            ConversionLimits::default(),
            &mut report,
        )
        .unwrap();
        let expected =
            time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
                .unwrap();
        assert_eq!(datetime, Value::DateTime(expected));

        assert_eq!(
            excel_serial(1.0, false, true, "xl/worksheets/sheet1.xml").unwrap(),
            Value::Date(Date::from_calendar_date(1904, Month::January, 2).unwrap())
        );
        assert!(
            parse_workbook(
                b"<workbook><workbookPr date1904=\"1\"/></workbook>",
                ConversionLimits::default(),
            )
            .unwrap()
            .date_1904
        );
    }

    #[test]
    fn shared_formula_followers_use_cached_values_with_lossy_evidence() {
        let styles = vec![StyleProperties::default()];
        let sheet_id = SheetId::parse("data").unwrap();
        let labels = BTreeMap::new();
        let defined_names = BTreeMap::new();
        let context = WorksheetParseContext {
            shared_strings: &[],
            styles: &styles,
            sheet_id: &sheet_id,
            formula_names: FormulaNames {
                sheets: &labels,
                names: &defined_names,
            },
            date_1904: false,
            limits: ConversionLimits::default(),
        };
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let worksheet = parse_worksheet(
            b"<worksheet><sheetData><row r=\"1\" hidden=\"1\"><c r=\"A1\"><f t=\"shared\" si=\"0\"/><v>3</v></c></row></sheetData><dataValidations count=\"1\"><dataValidation sqref=\"A1\"/></dataValidations></worksheet>",
            "xl/worksheets/sheet1.xml",
            &context,
            &mut report,
        )
        .unwrap();
        assert_eq!(
            worksheet
                .cells
                .get(&Coordinate::parse("A1").unwrap())
                .unwrap()
                .value,
            Value::Number(3.0)
        );
        assert!(worksheet.omitted_features.contains("row_attributes"));
        assert!(worksheet.omitted_features.contains("data_validation"));
        assert!(!report.finish().is_lossless());
    }

    #[test]
    fn unsupported_calculated_column_does_not_create_fill() {
        let table_id = TableId::parse("sales").unwrap();
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![
                    SheetItem::Table(Table {
                        id: table_id.clone(),
                        block: Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![
                                vec![
                                    Cell::new(Value::Text("Amount".to_owned())),
                                    Cell::new(Value::Text("Total".to_owned())),
                                ],
                                vec![Cell::new(Value::Number(2.0)), Cell::new(Value::Blank)],
                            ],
                        )
                        .unwrap(),
                        origin: None,
                    }),
                    SheetItem::Fill(Fill {
                        target: FillTarget::TableColumn {
                            table: table_id,
                            header: "Total".to_owned(),
                        },
                        formula: FormulaSource::new("=[@Amount]*2").unwrap(),
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let table_xml = package_text(&exported.value, "xl/tables/table1.xml");
        let changed = table_xml.replace(
            "[@Amount]*2",
            "XLOOKUP([@Amount],sales[Amount],sales[Total])",
        );
        let bytes = rewrite_package(&exported.value, &[("xl/tables/table1.xml", &changed)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();
        assert!(!imported.report.is_lossless());
        assert!(
            !imported.value.sheets[0]
                .items
                .iter()
                .any(|item| matches!(item, SheetItem::Fill(_)))
        );
    }

    #[test]
    fn calculated_table_wrong_arity_does_not_create_fill() {
        let table_id = TableId::parse("sales").unwrap();
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![
                    SheetItem::Table(Table {
                        id: table_id.clone(),
                        block: Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![
                                vec![Cell::new(Value::Text("Total".to_owned()))],
                                vec![Cell::new(Value::Blank)],
                            ],
                        )
                        .unwrap(),
                        origin: None,
                    }),
                    SheetItem::Fill(Fill {
                        target: FillTarget::TableColumn {
                            table: table_id,
                            header: "Total".to_owned(),
                        },
                        formula: FormulaSource::new("=1+1").unwrap(),
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let table = package_text(&exported.value, "xl/tables/table1.xml");
        let changed = table.replace("1+1", "IF(TRUE,1)");
        assert_ne!(changed, table);
        let bytes = rewrite_package(&exported.value, &[("xl/tables/table1.xml", &changed)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();
        assert!(!imported.report.is_lossless());
        assert!(
            !imported.value.sheets[0]
                .items
                .iter()
                .any(|item| matches!(item, SheetItem::Fill(_)))
        );
    }

    #[test]
    fn unparsable_calculated_column_formula_drops_only_the_fill() {
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Table(Table {
                    id: TableId::parse("sales").unwrap(),
                    block: Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![
                            vec![Cell::new(Value::Text("Amount".to_owned()))],
                            vec![Cell::new(Value::Number(2.0))],
                        ],
                    )
                    .unwrap(),
                    origin: None,
                })],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let table_xml = package_text(&exported.value, "xl/tables/table1.xml");
        let changed = table_xml.replace(
            "<tableColumn id=\"1\" name=\"Amount\"/>",
            "<tableColumn id=\"1\" name=\"Amount\"><calculatedColumnFormula>SUM(</calculatedColumnFormula></tableColumn>",
        );
        assert_ne!(changed, table_xml);
        let bytes = rewrite_package(&exported.value, &[("xl/tables/table1.xml", &changed)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        // The column keeps the values Excel cached; only the @fill is lost.
        let table = imported.value.sheets[0]
            .items
            .iter()
            .find_map(|item| match item {
                SheetItem::Table(table) => Some(table),
                _ => None,
            })
            .expect("the table still imports");
        assert_eq!(table.block.cells[1][0].value, Value::Number(2.0));
        assert!(
            !imported.value.sheets[0]
                .items
                .iter()
                .any(|item| matches!(item, SheetItem::Fill(_)))
        );
        assert!(imported.report.outcomes().iter().any(|outcome| {
            outcome.feature == "portable_formulas"
                && outcome.formula == Some(FormulaDisposition::Replaced)
                && outcome
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("outside portable-a1@1 syntax"))
        }));
        assert!(!imported.report.is_lossless());
    }

    #[test]
    fn xml_hardening_follows_the_part_role_not_the_part_name() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let limits = ConversionLimits::default();
        let part = "xl/worksheets/sheet1.dat";

        let mut doctype = String::from("<!DOCTYPE worksheet [<!ENTITY payload \"value\">]>");
        doctype.push_str(&"<nest>".repeat(5_000));
        doctype.push_str(&"</nest>".repeat(5_000));
        let bytes = disguise_worksheet_part(&exported.value, part, &doctype);
        assert_eq!(
            import_xlsx(&bytes, limits).unwrap_err().error.code,
            ConvertErrorCode::UnsupportedPackage
        );

        let mut deep = "<nest>".repeat(5_000);
        deep.push_str(&"</nest>".repeat(5_000));
        let bytes = disguise_worksheet_part(&exported.value, part, &deep);
        assert_eq!(
            import_xlsx(&bytes, limits).unwrap_err().error.code,
            ConvertErrorCode::ResourceLimit
        );

        let worksheet = package_text(&exported.value, "xl/worksheets/sheet1.xml");
        let bytes = disguise_worksheet_part(&exported.value, part, &worksheet);
        assert_eq!(import_xlsx(&bytes, limits).unwrap().value.sheets.len(), 1);
    }

    #[test]
    fn unconsumed_parts_relationships_and_macro_mime_are_lossy() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let content_types = package_text(&exported.value, "[Content_Types].xml").replace(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
        );
        let relationships = package_text(&exported.value, "xl/_rels/workbook.xml.rels").replace(
            "</Relationships>",
            "<Relationship Id=\"rMetadata\" Type=\"http://example.test/relationships/metadata\" Target=\"metadata.xml\"/></Relationships>",
        );
        let bytes = rewrite_package(
            &exported.value,
            &[
                ("[Content_Types].xml", &content_types),
                ("xl/_rels/workbook.xml.rels", &relationships),
            ],
            &[
                ("xl/metadata.xml", "<metadata/>"),
                ("xl/VBAPROJECT.BIN", "macro"),
            ],
        );
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();
        assert!(!imported.report.is_lossless());
        let features: BTreeSet<_> = imported
            .report
            .outcomes()
            .iter()
            .map(|outcome| outcome.feature.as_str())
            .collect();
        assert!(features.contains("macro"));
        assert!(features.contains("ooxml_part"));
        assert!(features.contains("ooxml_relationship"));
    }

    #[test]
    fn unsupported_content_inside_consumed_styles_is_lossy() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let styles = package_text(&exported.value, "xl/styles.xml");
        let styles = styles.replacen("<font>", "<font><name val=\"Calibri\"/><u/>", 1);
        let styles = styles.replacen(
            "<left/>",
            "<left style=\"thin\"><color theme=\"1\" tint=\"0.25\"/></left>",
            1,
        );
        let styles = styles.replace(
            "</styleSheet>",
            "<tableStyles count=\"0\" defaultTableStyle=\"TableStyleMedium2\"/></styleSheet>",
        );
        let bytes = rewrite_package(&exported.value, &[("xl/styles.xml", &styles)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();
        assert!(!imported.report.is_lossless());
        let features: BTreeSet<_> = imported
            .report
            .outcomes()
            .iter()
            .map(|outcome| outcome.feature.as_str())
            .collect();
        assert!(features.contains("xlsx_style_font_metadata"));
        assert!(features.contains("xlsx_style_borders"));
        assert!(features.contains("xlsx_style_theme_or_indexed_color"));
        assert!(features.contains("xlsx_style_table_defaults"));
    }

    #[test]
    fn rich_shared_strings_are_flattened_with_lossy_evidence() {
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![vec![Cell::new(Value::Text("rich".to_owned()))]],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let worksheet = package_text(&exported.value, "xl/worksheets/sheet1.xml");
        let worksheet = worksheet.replace(
            "<c r=\"A1\" t=\"inlineStr\"><is><t>rich</t></is></c>",
            "<c r=\"A1\" t=\"s\"><v>0</v></c>",
        );
        let relationships = package_text(&exported.value, "xl/_rels/workbook.xml.rels").replace(
            "</Relationships>",
            "<Relationship Id=\"rSharedStrings\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings\" Target=\"sharedStrings.xml\"/></Relationships>",
        );
        let content_types = package_text(&exported.value, "[Content_Types].xml").replace(
            "</Types>",
            "<Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml\"/></Types>",
        );
        let bytes = rewrite_package(
            &exported.value,
            &[
                ("xl/worksheets/sheet1.xml", &worksheet),
                ("xl/_rels/workbook.xml.rels", &relationships),
                ("[Content_Types].xml", &content_types),
            ],
            &[(
                "xl/sharedStrings.xml",
                "<sst><si><r><rPr><b/></rPr><t>rich</t></r></si></sst>",
            )],
        );
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();
        assert!(!imported.report.is_lossless());
        assert!(imported.report.outcomes().iter().any(|outcome| {
            outcome.feature == "xlsx_shared_string_rich_text"
                && outcome.outcome == crate::FeatureOutcome::Omitted
        }));
        assert!(
            imported.value.sheets[0]
                .items
                .iter()
                .any(|item| match item {
                    SheetItem::Block(block) =>
                        block.cells[0][0].value == Value::Text("rich".to_owned()),
                    _ => false,
                })
        );
    }

    #[test]
    fn consumed_workbook_worksheet_and_table_features_are_lossy() {
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Table(Table {
                    id: TableId::parse("sales").unwrap(),
                    block: Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![
                            vec![Cell::new(Value::Text("Amount".to_owned()))],
                            vec![Cell::new(Value::Number(2.0))],
                        ],
                    )
                    .unwrap(),
                    origin: None,
                })],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let workbook = package_text(&exported.value, "xl/workbook.xml")
            .replace(
                "<sheets>",
                "<workbookPr date1904=\"0\" filterPrivacy=\"1\"/><workbookViews><workbookView activeTab=\"0\"/></workbookViews><sheets>",
            )
            .replace("r:id=\"rId1\"", "state=\"hidden\" r:id=\"rId1\"");
        let worksheet = package_text(&exported.value, "xl/worksheets/sheet1.xml").replace(
            "<sheetData>",
            "<sheetViews><sheetView workbookViewId=\"0\"><pane xSplit=\"1\"/></sheetView></sheetViews><futureFeature/><sheetData>",
        );
        let table = package_text(&exported.value, "xl/tables/table1.xml")
            .replacen("<table ", "<table totalsRowShown=\"1\" ", 1)
            .replace(
                "<tableColumns",
                "<autoFilter ref=\"A1:A2\"><filterColumn colId=\"0\"><filters><filter val=\"2\"/></filters></filterColumn></autoFilter><tableColumns",
            )
            .replace(
                "</table>",
                "<tableStyleInfo name=\"TableStyleMedium2\" showRowStripes=\"1\"/></table>",
            );
        let bytes = rewrite_package(
            &exported.value,
            &[
                ("xl/workbook.xml", &workbook),
                ("xl/worksheets/sheet1.xml", &worksheet),
                ("xl/tables/table1.xml", &table),
            ],
            &[],
        );
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();
        assert!(!imported.report.is_lossless());
        let features: BTreeSet<_> = imported
            .report
            .outcomes()
            .iter()
            .map(|outcome| outcome.feature.as_str())
            .collect();
        for expected in [
            "workbook_views",
            "sheet_visibility",
            "unsupported_workbook_attributes",
            "sheet_views",
            "unknown_worksheet_content",
            "table_filters_and_sorting",
            "table_totals",
            "table_style",
        ] {
            assert!(features.contains(expected), "missing feature {expected}");
        }
    }

    #[test]
    fn exporter_canonical_geometry_table_and_calc_properties_import_exactly() {
        let source = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                // A canonical lowercase label maps back to the same sheet id, so this
                // fixture isolates XML feature inventory from identifier translation.
                label: "data".to_owned(),
                items: vec![
                    SheetItem::Table(Table {
                        id: TableId::parse("sales").unwrap(),
                        block: Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![
                                vec![Cell::new(Value::Text("Amount".to_owned()))],
                                vec![Cell::new(Value::Number(2.0))],
                            ],
                        )
                        .unwrap(),
                        origin: None,
                    }),
                    SheetItem::RowGeometry(RowGeometry {
                        rows: RowRange::new(1, 1).unwrap(),
                        height: 20.0,
                        origin: None,
                    }),
                    SheetItem::ColumnGeometry(ColumnGeometry {
                        columns: ColumnRange::new(1, 1).unwrap(),
                        width: 12.0,
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };

        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        assert!(exported.report.is_lossless());
        let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
        assert!(
            imported.report.is_lossless(),
            "canonical exporter XML produced import outcomes: {:?}",
            imported.report.outcomes()
        );
    }

    #[test]
    fn noncanonical_consumed_values_are_reported_and_namespace_spoofs_are_rejected() {
        let workbook = parse_workbook(
            b"<workbook><calcPr calcId=\"7\" fullCalcOnLoad=\"0\" forceFullCalc=\"1\"/></workbook>",
            ConversionLimits::default(),
        )
        .unwrap();
        assert!(
            workbook
                .omitted_features
                .contains("workbook_calculation_properties")
        );

        let styles = vec![StyleProperties::default()];
        let sheet_id = SheetId::parse("data").unwrap();
        let labels = BTreeMap::new();
        let defined_names = BTreeMap::new();
        let context = WorksheetParseContext {
            shared_strings: &[],
            styles: &styles,
            sheet_id: &sheet_id,
            formula_names: FormulaNames {
                sheets: &labels,
                names: &defined_names,
            },
            date_1904: false,
            limits: ConversionLimits::default(),
        };
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let worksheet = parse_worksheet(
            b"<worksheet><cols><col min=\"1\" max=\"1\" width=\"12\" customWidth=\"0\"/></cols><sheetData><row r=\"1\" ht=\"20\" customHeight=\"0\"/></sheetData></worksheet>",
            "xl/worksheets/sheet1.xml",
            &context,
            &mut report,
        )
        .unwrap();
        assert!(worksheet.omitted_features.contains("row_attributes"));
        assert!(worksheet.omitted_features.contains("column_attributes"));

        let table = parse_table(
            b"<table displayName=\"sales\" ref=\"A1:A2\" headerRowCount=\"0\"><autoFilter ref=\"A1:A1\"/><tableColumns count=\"1\"><tableColumn id=\"1\" name=\"Amount\"/></tableColumns></table>",
            "xl/tables/table1.xml",
            ConversionLimits::default(),
        )
        .unwrap();
        assert!(
            table
                .omitted_features
                .contains("table_header_configuration")
        );
        assert!(table.omitted_features.contains("table_filter_range"));

        let foreign_element = "<workbook xmlns:e=\"urn:evil\"><e:sheet name=\"Data\" sheetId=\"1\" id=\"rId1\"/></workbook>";
        assert!(parse_workbook(foreign_element.as_bytes(), ConversionLimits::default()).is_err());
        let foreign_attribute =
            b"<workbook xmlns:e=\"urn:evil\"><sheet name=\"Data\" sheetId=\"1\" e:id=\"rId1\"/></workbook>";
        assert!(parse_workbook(foreign_attribute, ConversionLimits::default()).is_err());
        let reserved_attribute_spoof = format!(
            "<workbook xmlns:xml=\"{}\"><sheet xml:name=\"Data\" sheetId=\"1\" id=\"rId1\"/></workbook>",
            std::str::from_utf8(XML_NS).unwrap()
        );
        assert!(
            parse_workbook(
                reserved_attribute_spoof.as_bytes(),
                ConversionLimits::default()
            )
            .is_err()
        );
        let relationship_attribute_spoof = format!(
            "<table xmlns:r=\"{}\" displayName=\"sales\" ref=\"A1:A2\" r:ref=\"B1:B2\"><tableColumns count=\"1\"><tableColumn id=\"1\" name=\"Amount\"/></tableColumns></table>",
            std::str::from_utf8(OFFICE_RELATIONSHIPS_NS).unwrap()
        );
        assert!(
            parse_table(
                relationship_attribute_spoof.as_bytes(),
                "xl/tables/table1.xml",
                ConversionLimits::default()
            )
            .is_err()
        );

        let prefixed_main = format!(
            "<s:workbook xmlns:s=\"{}\"><s:calcPr calcId=\"0\" fullCalcOnLoad=\"1\" forceFullCalc=\"1\"/></s:workbook>",
            std::str::from_utf8(SPREADSHEETML_NS).unwrap()
        );
        assert!(
            parse_workbook(prefixed_main.as_bytes(), ConversionLimits::default())
                .unwrap()
                .omitted_features
                .is_empty()
        );
    }

    #[test]
    fn style_semantics_are_inventoried_without_flagging_empty_optional_tables() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let canonical = package_text(&exported.value, "xl/styles.xml");
        let benign = canonical.replace(
            "</styleSheet>",
            "<dxfs count=\"0\"/><tableStyles count=\"0\"/></styleSheet>",
        );
        let benign_features = parse_styles(
            benign.as_bytes(),
            "xl/styles.xml",
            ConversionLimits::default(),
        )
        .unwrap()
        .unsupported;
        assert!(benign_features.is_empty(), "{benign_features:?}");

        let adversarial = canonical
            .replacen("borderId=\"0\"/></cellStyleXfs>", "borderId=\"0\" applyFont=\"1\"/></cellStyleXfs>", 1)
            .replacen("applyFill=\"1\"", "applyFill=\"0\"", 1)
            .replacen("name=\"Normal\"", "name=\"Custom\"", 1)
            .replace(
                "</styleSheet>",
                "<dxfs count=\"1\"><dxf/></dxfs><tableStyles count=\"1\" defaultTableStyle=\"TableStyleMedium2\"/></styleSheet>",
            );
        let adversarial_features = parse_styles(
            adversarial.as_bytes(),
            "xl/styles.xml",
            ConversionLimits::default(),
        )
        .unwrap()
        .unsupported;
        for expected in [
            "xlsx_style_base_formats",
            "xlsx_style_apply_flags",
            "xlsx_style_named_styles",
            "xlsx_style_differential_formats",
            "xlsx_style_table_defaults",
        ] {
            assert!(
                adversarial_features.contains(expected),
                "missing {expected}: {adversarial_features:?}"
            );
        }
    }

    #[test]
    fn style_counts_and_references_are_strictly_validated() {
        let limits = ConversionLimits::default();
        let count_mismatch = "<styleSheet><fonts count=\"2\"><font/></fonts><fills count=\"1\"><fill/></fills><cellStyleXfs count=\"1\"><xf/></cellStyleXfs><cellXfs count=\"1\"><xf fontId=\"0\" fillId=\"0\" xfId=\"0\"/></cellXfs></styleSheet>";
        assert!(parse_styles(count_mismatch.as_bytes(), "xl/styles.xml", limits).is_err());

        let bad_font = "<styleSheet><fonts count=\"1\"><font/></fonts><fills count=\"1\"><fill/></fills><cellStyleXfs count=\"1\"><xf/></cellStyleXfs><cellXfs count=\"1\"><xf fontId=\"1\" fillId=\"0\" xfId=\"0\"/></cellXfs></styleSheet>";
        assert!(parse_styles(bad_font.as_bytes(), "xl/styles.xml", limits).is_err());

        let bad_number = "<styleSheet><fonts count=\"1\"><font/></fonts><fills count=\"1\"><fill/></fills><cellStyleXfs count=\"1\"><xf/></cellStyleXfs><cellXfs count=\"1\"><xf fontId=\"0\" fillId=\"0\" numFmtId=\"164\" xfId=\"0\"/></cellXfs></styleSheet>";
        assert!(parse_styles(bad_number.as_bytes(), "xl/styles.xml", limits).is_err());

        let bad_base = "<styleSheet><fonts count=\"1\"><font/></fonts><fills count=\"1\"><fill/></fills><cellStyleXfs count=\"1\"><xf/></cellStyleXfs><cellXfs count=\"1\"><xf fontId=\"0\" fillId=\"0\" xfId=\"1\"/></cellXfs></styleSheet>";
        assert!(parse_styles(bad_base.as_bytes(), "xl/styles.xml", limits).is_err());
    }

    #[test]
    fn duplicate_labels_namespaces_and_global_table_limit_are_rejected() {
        assert!(reject_case_insensitive_duplicates(["Data", "data"], "part", "sheet").is_err());

        let table_id = TableId::parse("sales").unwrap();
        let mut tables = BTreeMap::new();
        tables.insert("sales".to_owned(), table_id);
        let name = DefinedName {
            name: "SALES".to_owned(),
            expression: "Data!A1".to_owned(),
        };
        assert!(
            import_defined_names(
                &[name],
                &[WorkbookSheet {
                    label: "Data".to_owned(),
                    relationship: "rId1".to_owned(),
                }],
                &[SheetId::parse("data").unwrap()],
                &tables,
                &BTreeMap::new(),
                &mut ConversionReport::new(
                    FormatDescriptor::xlsx(),
                    FormatDescriptor::marksheet_ir()
                ),
            )
            .is_err()
        );

        let mut builtin_report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let builtins = import_defined_names(
            &[DefinedName {
                name: "_xlnm.Print_Area".to_owned(),
                expression: "Data!A1".to_owned(),
            }],
            &[WorkbookSheet {
                label: "Data".to_owned(),
                relationship: "rId1".to_owned(),
            }],
            &[SheetId::parse("data").unwrap()],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &mut builtin_report,
        )
        .unwrap();
        assert!(builtins.names.is_empty());
        assert!(builtins.omitted.is_empty());
        assert!(!builtin_report.finish().is_lossless());

        let table_sheet = |id: &str, table: &str| Sheet {
            id: SheetId::parse(id).unwrap(),
            label: id.to_owned(),
            items: vec![SheetItem::Table(Table {
                id: TableId::parse(table).unwrap(),
                block: Block::new(
                    Coordinate::parse("A1").unwrap(),
                    vec![vec![Cell::new(Value::Text("Header".to_owned()))]],
                )
                .unwrap(),
                origin: None,
            })],
            origin: None,
        };
        let workbook = Workbook {
            sheets: vec![
                table_sheet("one", "one_table"),
                table_sheet("two", "two_table"),
            ],
            ..Workbook::default()
        };
        let exported = export_xlsx(&workbook, ConversionLimits::default()).unwrap();
        let limits = ConversionLimits {
            max_tables: 1,
            ..ConversionLimits::default()
        };
        assert_eq!(
            import_xlsx(&exported.value, limits).unwrap_err().code,
            ConvertErrorCode::ResourceLimit
        );

        let limits = ConversionLimits {
            max_cells: 3,
            ..ConversionLimits::default()
        };
        assert_eq!(
            import_xlsx(&exported.value, limits).unwrap_err().code,
            ConvertErrorCode::ResourceLimit
        );
    }

    #[test]
    fn import_resource_and_grid_limits_cover_all_semantic_tables() {
        let limits = ConversionLimits {
            max_shared_strings: 0,
            ..ConversionLimits::default()
        };
        assert_eq!(
            parse_shared_strings(
                b"<sst><si><t>one</t></si></sst>",
                "xl/sharedStrings.xml",
                limits,
            )
            .unwrap_err()
            .code,
            ConvertErrorCode::ResourceLimit
        );

        let num_formats = "<styleSheet><numFmts count=\"2\"><numFmt numFmtId=\"164\" formatCode=\"0\"/><numFmt numFmtId=\"165\" formatCode=\"0.0\"/></numFmts><fonts count=\"1\"><font/></fonts><fills count=\"1\"><fill/></fills><cellStyleXfs count=\"1\"><xf/></cellStyleXfs><cellXfs count=\"1\"><xf fontId=\"0\" fillId=\"0\" xfId=\"0\"/></cellXfs></styleSheet>";
        let limits = ConversionLimits {
            max_styles: 1,
            ..ConversionLimits::default()
        };
        assert_eq!(
            parse_styles(num_formats.as_bytes(), "xl/styles.xml", limits)
                .unwrap_err()
                .code,
            ConvertErrorCode::ResourceLimit
        );

        let outside_table = "<table displayName=\"outside\" ref=\"XFE1:XFE1\"><tableColumns count=\"1\"><tableColumn id=\"1\" name=\"Header\"/></tableColumns></table>";
        assert!(
            parse_table(
                outside_table.as_bytes(),
                "xl/tables/table1.xml",
                ConversionLimits::default(),
            )
            .is_err()
        );

        let styles = vec![StyleProperties::default()];
        let sheet_id = SheetId::parse("data").unwrap();
        let labels = BTreeMap::new();
        let defined_names = BTreeMap::new();
        let context = WorksheetParseContext {
            shared_strings: &[],
            styles: &styles,
            sheet_id: &sheet_id,
            formula_names: FormulaNames {
                sheets: &labels,
                names: &defined_names,
            },
            date_1904: false,
            limits: ConversionLimits::default(),
        };
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        assert!(
            parse_worksheet(
                b"<worksheet><sheetData><row r=\"1048577\" ht=\"12\"/></sheetData></worksheet>",
                "xl/worksheets/sheet1.xml",
                &context,
                &mut report,
            )
            .is_err()
        );
        assert!(
            parse_worksheet(
                b"<worksheet><cols><col min=\"1\" max=\"16385\" width=\"12\"/></cols></worksheet>",
                "xl/worksheets/sheet1.xml",
                &context,
                &mut report,
            )
            .is_err()
        );

        let formula_workbook = Workbook {
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![vec![Cell::new(Value::Formula(
                            FormulaSource::new("=1+1").unwrap(),
                        ))]],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        };
        let exported = export_xlsx(&formula_workbook, ConversionLimits::default()).unwrap();
        let limits = ConversionLimits {
            max_formulas: 0,
            ..ConversionLimits::default()
        };
        assert_eq!(
            import_xlsx(&exported.value, limits).unwrap_err().code,
            ConvertErrorCode::ResourceLimit
        );
    }

    fn named_range_workbook(formula: &str) -> Workbook {
        Workbook {
            names: vec![Name {
                id: NameId::parse("total").unwrap(),
                target: NameTarget::Range(SheetRange {
                    sheet: SheetId::parse("data").unwrap(),
                    range: Range::parse("A1:A2").unwrap(),
                }),
                origin: None,
            }],
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![SheetItem::Block(
                    Block::new(
                        Coordinate::parse("A1").unwrap(),
                        vec![
                            vec![Cell::new(Value::Number(1.0))],
                            vec![Cell::new(Value::Number(2.0))],
                            vec![Cell::new(Value::Formula(
                                FormulaSource::new(formula).unwrap(),
                            ))],
                        ],
                    )
                    .unwrap(),
                )],
                origin: None,
            }],
            ..Workbook::default()
        }
    }

    fn formula_outcomes_at<'report>(
        report: &'report ConversionReport,
        location: &ConversionLocation,
    ) -> Vec<&'report ConversionEvent> {
        report
            .outcomes()
            .iter()
            .filter(|outcome| {
                outcome.feature == "portable_formulas" && outcome.locations == [location.clone()]
            })
            .collect()
    }

    fn omitted_name_details(report: &ConversionReport) -> Vec<&str> {
        report
            .outcomes()
            .iter()
            .filter(|outcome| {
                outcome.feature == "named_ranges"
                    && outcome.outcome == crate::FeatureOutcome::Omitted
            })
            .filter_map(|outcome| outcome.detail.as_deref())
            .collect()
    }

    fn import_with_name_target(target: &str) -> ConversionResult<Workbook> {
        let exported = export_xlsx(
            &named_range_workbook("=SUM(total)"),
            ConversionLimits::default(),
        )
        .unwrap();
        let workbook =
            package_text(&exported.value, "xl/workbook.xml").replace("'Data'!$A$1:$A$2", target);
        let bytes = rewrite_package(&exported.value, &[("xl/workbook.xml", &workbook)], &[]);
        import_xlsx(&bytes, ConversionLimits::default())
    }

    #[test]
    fn unsupported_defined_name_targets_are_omitted_per_name() {
        let sheets = [WorkbookSheet {
            label: "Data".to_owned(),
            relationship: "rId1".to_owned(),
        }];
        let sheet_ids = [SheetId::parse("data").unwrap()];
        let sales_id = TableId::parse("sales").unwrap();
        let tables = BTreeMap::from([("sales".to_owned(), sales_id.clone())]);
        let headers = BTreeMap::from([(sales_id, BTreeSet::from(["Amount".to_owned()]))]);
        let unsupported = [
            ("whole_column", "Data!$A:$A"),
            ("multi_area", "Data!$A$1,Data!$C$3"),
            ("unknown_sheet", "Missing!$A$1"),
            ("outside_grid", "Data!XFE1"),
            ("unknown_table", "orders[Amount]"),
            ("missing_header", "sales[Missing]"),
            ("no_reference", "42"),
        ];
        let mut names: Vec<_> = unsupported
            .iter()
            .map(|(name, expression)| DefinedName {
                name: (*name).to_owned(),
                expression: (*expression).to_owned(),
            })
            .collect();
        names.push(DefinedName {
            name: "supported".to_owned(),
            expression: "Data!$B$2".to_owned(),
        });
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let imported =
            import_defined_names(&names, &sheets, &sheet_ids, &tables, &headers, &mut report)
                .unwrap();

        assert_eq!(imported.names.len(), 1);
        assert_eq!(imported.names[0].id.as_str(), "supported");
        assert_eq!(
            imported.names[0].target,
            NameTarget::Cell(SheetCoordinate {
                sheet: SheetId::parse("data").unwrap(),
                coordinate: Coordinate::parse("B2").unwrap(),
            })
        );
        let expected: BTreeSet<_> = unsupported
            .iter()
            .map(|(name, _)| NameId::parse(name).unwrap())
            .collect();
        assert_eq!(imported.omitted, expected);
        let report = report.finish();
        assert_eq!(omitted_name_details(&report).len(), unsupported.len());
    }

    #[test]
    fn whole_column_defined_name_is_omitted_without_failing_the_import() {
        let imported = import_with_name_target("'Data'!$A:$A").unwrap();

        assert!(imported.value.names.is_empty());
        let details = omitted_name_details(&imported.report);
        assert_eq!(details.len(), 1);
        assert!(
            details[0].contains("\"total\"") && details[0].contains("finite A1 target"),
            "unexpected omission detail {:?}",
            details[0]
        );
        let values: Vec<_> = imported.value.sheets[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::Block(block) => Some(block.cells[0][0].value.clone()),
                _ => None,
            })
            .collect();
        assert!(values.contains(&Value::Number(1.0)));
        assert!(values.contains(&Value::Number(2.0)));
    }

    #[test]
    fn formulas_reaching_an_omitted_name_are_replaced_and_reported() {
        let imported = import_with_name_target("'Data'!$A:$A").unwrap();

        let values: Vec<_> = imported.value.sheets[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::Block(block) => Some(block.cells[0][0].value.clone()),
                _ => None,
            })
            .collect();
        assert!(
            values.contains(&Value::Error(CellError::Name)),
            "formula reaching the omitted name kept an unresolved reference: {values:?}"
        );
        // The substitution is the only formula outcome the cell may carry. The
        // first parse pass ran before defined names resolved and recorded an
        // exact translation for this body; leaving that behind would make the
        // report claim a lossless translation and a destroyed formula for the
        // same cell, and `finish` sorts the false `Exact` claim first.
        let outcomes = formula_outcomes_at(
            &imported.report,
            &ConversionLocation::cell(
                SheetId::parse("data").unwrap(),
                Coordinate::parse("A3").unwrap(),
            ),
        );
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].outcome, crate::FeatureOutcome::Approximated);
        assert_eq!(outcomes[0].formula, Some(FormulaDisposition::Replaced));
        assert!(
            outcomes[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("omitted defined name")),
            "{outcomes:?}"
        );
        // The substitution keeps the artifact exportable; an unresolved name
        // reference would not survive the XLSX writer.
        export_xlsx(&imported.value, ConversionLimits::default()).unwrap();
    }

    /// A translated formula the importer later destroys must not leave its
    /// superseded outcome in the report, so no location may end up carrying an
    /// `Exact` and an `Approximated` claim about the same formula.
    #[test]
    fn a_replaced_formula_retracts_the_translation_it_superseded() {
        for imported in [
            import_with_name_target("'Data'!$A:$A").unwrap(),
            calculated_column_import_with_omitted_name(),
        ] {
            let stale: Vec<_> = imported
                .report
                .outcomes()
                .iter()
                .filter(|outcome| {
                    outcome.feature == "portable_formulas"
                        && outcome.formula == Some(FormulaDisposition::Translated)
                })
                .collect();
            assert!(
                stale.is_empty(),
                "superseded translation survived: {stale:?}"
            );
        }
    }

    #[test]
    fn sheet_labels_keep_xml_whitespace_across_the_round_trip() {
        for (label, reference) in [
            ("Line\nTwo", "Line&#10;Two"),
            ("Tab\tTwo", "Tab&#9;Two"),
            ("Cr\rTwo", "Cr&#13;Two"),
        ] {
            let mut source = basic_workbook();
            source.sheets[0].label = label.to_owned();
            let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
            // A literal tab, line feed, or carriage return in an attribute
            // value becomes a space under XML 1.0 attribute-value
            // normalization, so only a character reference survives a parse.
            assert!(
                package_text(&exported.value, "xl/workbook.xml")
                    .contains(&format!("<sheet name=\"{reference}\"")),
                "sheet label {label:?} must be written as {reference}"
            );
            let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
            assert_eq!(imported.value.sheets[0].label, label);
        }
    }

    #[test]
    fn multi_area_defined_name_degrades_while_supported_names_import_exactly() {
        let exported = export_xlsx(
            &named_range_workbook("=SUM(A1:A2)"),
            ConversionLimits::default(),
        )
        .unwrap();
        let workbook = package_text(&exported.value, "xl/workbook.xml").replace(
            "<definedName name=\"total\">'Data'!$A$1:$A$2</definedName>",
            "<definedName name=\"spread\">'Data'!$A$1,'Data'!$C$3</definedName><definedName name=\"total\">'Data'!$A$1:$A$2</definedName>",
        );
        let bytes = rewrite_package(&exported.value, &[("xl/workbook.xml", &workbook)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        assert_eq!(imported.value.names.len(), 1);
        assert_eq!(imported.value.names[0].id.as_str(), "total");
        assert_eq!(
            imported.value.names[0].target,
            NameTarget::Range(SheetRange {
                sheet: SheetId::parse("data").unwrap(),
                range: Range::parse("A1:A2").unwrap(),
            })
        );
        let details = omitted_name_details(&imported.report);
        assert_eq!(details.len(), 1);
        assert!(
            details[0].contains("\"spread\""),
            "unexpected omission detail {:?}",
            details[0]
        );
    }

    /// A table whose `Share` column is computed from the `total` named range —
    /// the shape Excel writes as a calculated column referencing a defined name.
    fn calculated_column_named_workbook() -> Workbook {
        Workbook {
            names: vec![Name {
                id: NameId::parse("total").unwrap(),
                target: NameTarget::Range(SheetRange {
                    sheet: SheetId::parse("data").unwrap(),
                    range: Range::parse("A2:A3").unwrap(),
                }),
                origin: None,
            }],
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![
                    SheetItem::Table(Table {
                        id: TableId::parse("sales").unwrap(),
                        block: Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![
                                vec![
                                    Cell::new(Value::Text("Amount".to_owned())),
                                    Cell::new(Value::Text("Share".to_owned())),
                                ],
                                vec![Cell::new(Value::Number(1.0)), Cell::new(Value::Blank)],
                                vec![Cell::new(Value::Number(2.0)), Cell::new(Value::Blank)],
                            ],
                        )
                        .unwrap(),
                        origin: None,
                    }),
                    SheetItem::Fill(Fill {
                        target: FillTarget::TableColumn {
                            table: TableId::parse("sales").unwrap(),
                            header: "Share".to_owned(),
                        },
                        formula: FormulaSource::new("=SUM(total)").unwrap(),
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        }
    }

    /// Asserts the calculated column survived import as a reported `#NAME?`
    /// fill instead of taking the whole package down with it.
    fn assert_calculated_column_degraded(imported: &Conversion<Workbook>) {
        let fills: Vec<_> = imported.value.sheets[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::Fill(fill) => Some(fill.formula.as_str().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(fills, vec!["=#NAME?".to_owned()]);
        assert!(imported.value.names.is_empty());
        assert_eq!(omitted_name_details(&imported.report).len(), 1);
        // The replacement is the column's only formula outcome: the exact
        // `@fill` the first pass recorded at `xl/tables`, and the per-cell
        // translations of the body cells the fill absorbed, no longer describe
        // anything this workbook contains.
        let outcomes: Vec<_> = imported
            .report
            .outcomes()
            .iter()
            .filter(|outcome| outcome.feature == "portable_formulas")
            .collect();
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].outcome, crate::FeatureOutcome::Approximated);
        assert_eq!(outcomes[0].formula, Some(FormulaDisposition::Replaced));
        assert!(
            outcomes[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("omitted defined name")),
            "{outcomes:?}"
        );
        export_xlsx(&imported.value, ConversionLimits::default()).unwrap();
        let text = marksheet_syntax::serialize_workbook(&imported.value).unwrap();
        assert!(!marksheet_syntax::parse(&text).has_errors());
    }

    /// Imports [`calculated_column_named_workbook`] with its `total` name
    /// widened to a whole column, the shape the importer cannot express.
    fn calculated_column_import_with_omitted_name() -> Conversion<Workbook> {
        let exported = export_xlsx(
            &calculated_column_named_workbook(),
            ConversionLimits::default(),
        )
        .unwrap();
        let workbook = package_text(&exported.value, "xl/workbook.xml")
            .replace("'Data'!$A$2:$A$3", "'Data'!$A:$A");
        let bytes = rewrite_package(&exported.value, &[("xl/workbook.xml", &workbook)], &[]);
        import_xlsx(&bytes, ConversionLimits::default()).unwrap()
    }

    #[test]
    fn calculated_column_reaching_an_omitted_name_is_replaced() {
        assert_calculated_column_degraded(&calculated_column_import_with_omitted_name());
    }

    #[test]
    fn omitted_name_replacements_keep_each_calculated_column_location() {
        let sheet_id = SheetId::parse("data").unwrap();
        let table_id = TableId::parse("sales").unwrap();
        let left_location = xlsx_location("xl/tables/table1.xml", Some("Left"));
        let right_location = xlsx_location("xl/tables/table1.xml", Some("Right"));
        let mut sheets = vec![Sheet {
            id: sheet_id.clone(),
            label: "Data".to_owned(),
            items: vec![
                SheetItem::Fill(Fill {
                    target: FillTarget::TableColumn {
                        table: table_id.clone(),
                        header: "Left".to_owned(),
                    },
                    formula: FormulaSource::new("=SUM(total)").unwrap(),
                    origin: None,
                }),
                SheetItem::Fill(Fill {
                    target: FillTarget::TableColumn {
                        table: table_id.clone(),
                        header: "Right".to_owned(),
                    },
                    formula: FormulaSource::new("=SUM(total)").unwrap(),
                    origin: None,
                }),
            ],
            origin: None,
        }];
        let fill_outcomes = BTreeMap::from([
            (
                (table_id.clone(), "Left".to_owned()),
                FillOutcome {
                    location: left_location.clone(),
                    sheet: sheet_id.clone(),
                    body: Range::parse("A2:A3").unwrap(),
                },
            ),
            (
                (table_id, "Right".to_owned()),
                FillOutcome {
                    location: right_location.clone(),
                    sheet: sheet_id,
                    body: Range::parse("B2:B3").unwrap(),
                },
            ),
        ]);
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        for location in [left_location.clone(), right_location.clone()] {
            report.exact_event(
                ConversionEvent::new(ConversionFeature::Formula, "calculated column")
                    .formula(FormulaDisposition::Translated)
                    .at(location),
            );
        }

        replace_formulas_referencing_omitted_names(
            &mut sheets,
            &BTreeSet::from([NameId::parse("total").unwrap()]),
            &fill_outcomes,
            ConversionLimits::default(),
            &mut report,
        )
        .unwrap();

        let replacements: Vec<_> = report
            .outcomes()
            .iter()
            .filter(|outcome| outcome.formula == Some(FormulaDisposition::Replaced))
            .map(|outcome| outcome.locations.clone())
            .collect();
        assert_eq!(replacements.len(), 2, "{replacements:?}");
        assert!(
            replacements.contains(&vec![left_location]),
            "{replacements:?}"
        );
        assert!(
            replacements.contains(&vec![right_location]),
            "{replacements:?}"
        );
    }

    /// Excel authors defined names in whatever case they like, and writes that
    /// spelling into every formula that reaches them. The importer has to
    /// rewrite those references to the identifier it assigned the name, or a
    /// calculated column referencing an omitted name fails to parse and takes
    /// the whole package down instead of degrading to `#NAME?`.
    #[test]
    fn calculated_column_reaching_an_omitted_mixed_case_name_is_replaced() {
        let exported = export_xlsx(
            &calculated_column_named_workbook(),
            ConversionLimits::default(),
        )
        .unwrap();
        let workbook = package_text(&exported.value, "xl/workbook.xml")
            .replace("name=\"total\"", "name=\"Total\"")
            .replace("'Data'!$A$2:$A$3", "'Data'!$A:$A");
        let table = package_text(&exported.value, "xl/tables/table1.xml")
            .replace("SUM(total)", "SUM(Total)");
        assert!(workbook.contains("name=\"Total\""), "{workbook}");
        assert!(table.contains("SUM(Total)"), "{table}");
        let bytes = rewrite_package(
            &exported.value,
            &[
                ("xl/workbook.xml", &workbook),
                ("xl/tables/table1.xml", &table),
            ],
            &[],
        );

        assert_calculated_column_degraded(
            &import_xlsx(&bytes, ConversionLimits::default()).unwrap(),
        );
    }

    /// A cell formula reaching an Excel-cased omitted name has to be replaced
    /// through the omitted-name path, which names the cause, rather than by the
    /// generic fallback for bodies that simply fail to parse.
    #[test]
    fn cell_formula_reaching_an_omitted_mixed_case_name_is_reported_as_such() {
        let exported = export_xlsx(
            &named_range_workbook("=SUM(total)"),
            ConversionLimits::default(),
        )
        .unwrap();
        let workbook = package_text(&exported.value, "xl/workbook.xml")
            .replace("name=\"total\"", "name=\"Total\"")
            .replace("'Data'!$A$1:$A$2", "'Data'!$A:$A");
        let sheet = package_text(&exported.value, "xl/worksheets/sheet1.xml")
            .replace("SUM(total)", "SUM(Total)");
        assert!(sheet.contains("SUM(Total)"), "{sheet}");
        let bytes = rewrite_package(
            &exported.value,
            &[
                ("xl/workbook.xml", &workbook),
                ("xl/worksheets/sheet1.xml", &sheet),
            ],
            &[],
        );
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        let values: Vec<_> = imported.value.sheets[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::Block(block) => Some(block.cells[0][0].value.clone()),
                _ => None,
            })
            .collect();
        assert!(
            values.contains(&Value::Error(CellError::Name)),
            "{values:?}"
        );
        let details: Vec<_> = imported
            .report
            .outcomes()
            .iter()
            .filter(|outcome| outcome.feature == "portable_formulas")
            .filter_map(|outcome| outcome.detail.as_deref())
            .collect();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("omitted defined name")),
            "{details:?}"
        );
        assert!(
            !details
                .iter()
                .any(|detail| detail.contains("outside portable-a1@1 syntax")),
            "{details:?}"
        );
    }

    /// Two identifier namespaces colliding is a property of the package, not
    /// of one name's target, so it stays fatal even when that name's target is
    /// itself unimportable: the omitted name still claims the identifier every
    /// formula body reaching it was rewritten to spell.
    #[test]
    fn a_name_colliding_with_a_table_id_is_fatal_even_when_its_target_is_omitted() {
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let error = import_defined_names(
            &[DefinedName {
                name: "My.Sales".to_owned(),
                expression: "Data!$A:$A".to_owned(),
            }],
            &[WorkbookSheet {
                label: "Data".to_owned(),
                relationship: "rId1".to_owned(),
            }],
            &[SheetId::parse("data").unwrap()],
            &BTreeMap::from([("my sales".to_owned(), TableId::parse("my_sales").unwrap())]),
            &BTreeMap::new(),
            &mut report,
        )
        .unwrap_err();

        assert_eq!(error.code, ConvertErrorCode::InvalidPackage);
        assert!(
            error
                .message
                .contains("collide after identifier normalization"),
            "{}",
            error.message
        );
    }

    /// The omitted set is keyed by the identifier a translated formula body
    /// spells, which for an Excel-cased name is not the XLSX spelling.
    #[test]
    fn omitted_names_are_keyed_by_their_assigned_identifier() {
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        let imported = import_defined_names(
            &[DefinedName {
                name: "Total".to_owned(),
                expression: "Data!$A:$A".to_owned(),
            }],
            &[WorkbookSheet {
                label: "Data".to_owned(),
                relationship: "rId1".to_owned(),
            }],
            &[SheetId::parse("data").unwrap()],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &mut report,
        )
        .unwrap();

        assert!(imported.names.is_empty());
        assert_eq!(
            imported.omitted,
            BTreeSet::from([NameId::parse("total").unwrap()])
        );
        assert_eq!(omitted_name_details(&report.finish()).len(), 1);
    }

    /// A supported name spelled in Excel's casing still has to be reachable:
    /// its references are rewritten rather than degraded to `#NAME?`.
    #[test]
    fn mixed_case_name_references_are_rewritten_to_the_assigned_identifier() {
        let exported = export_xlsx(
            &named_range_workbook("=SUM(total)"),
            ConversionLimits::default(),
        )
        .unwrap();
        let workbook = package_text(&exported.value, "xl/workbook.xml")
            .replace("name=\"total\"", "name=\"Total\"");
        let sheet = package_text(&exported.value, "xl/worksheets/sheet1.xml")
            .replace("SUM(total)", "SUM(Total)");
        assert!(sheet.contains("SUM(Total)"), "{sheet}");
        let bytes = rewrite_package(
            &exported.value,
            &[
                ("xl/workbook.xml", &workbook),
                ("xl/worksheets/sheet1.xml", &sheet),
            ],
            &[],
        );
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        assert_eq!(imported.value.names.len(), 1);
        assert_eq!(imported.value.names[0].id.as_str(), "total");
        let formulas: Vec<_> = imported.value.sheets[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::Block(block) => match &block.cells[0][0].value {
                    Value::Formula(formula) => Some(formula.as_str().to_owned()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(formulas, vec!["=SUM(total)".to_owned()]);
    }

    #[test]
    fn function_and_table_spellings_are_not_mistaken_for_defined_names() {
        let names = BTreeMap::from([
            ("sum".to_owned(), NameId::parse("sum_range").unwrap()),
            ("sales".to_owned(), NameId::parse("sales_name").unwrap()),
            ("total".to_owned(), NameId::parse("grand_total").unwrap()),
        ]);
        let sheets = BTreeMap::new();
        let translated = translate_excel_formula(
            "SUM (Total)+Sales[Amount]+\"Total\"",
            FormulaNames {
                sheets: &sheets,
                names: &names,
            },
        );

        assert_eq!(
            translated,
            "=SUM (grand_total)+Sales[Amount]+\"Total\"".to_owned()
        );
    }

    #[test]
    fn non_default_cell_xfs_zero_is_materialized_and_reported() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let styles = package_text(&exported.value, "xl/styles.xml")
            .replacen("<font>", "<font><b/>", 1)
            .replacen(
                "<cellXfs count=\"1\"><xf numFmtId=\"0\"",
                "<cellXfs count=\"1\"><xf numFmtId=\"14\"",
                1,
            );
        let bytes = rewrite_package(&exported.value, &[("xl/styles.xml", &styles)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        // The workbook default format is emitted instead of being dropped, and
        // it carries the very properties the importer used to read A1's value.
        let default_style = imported
            .value
            .styles
            .iter()
            .find(|style| style.id == style_id(0).unwrap())
            .expect("cellXfs[0] is emitted as a Marksheet style");
        assert_eq!(default_style.properties.bold, Some(true));
        assert_eq!(default_style.properties.number, Some(NumberFormat::Date));
        assert!(imported.value.sheets[0].items.iter().any(|item| matches!(
            item,
            SheetItem::Apply(apply)
                if apply.target == ApplyTarget::Range(Range::parse("A1").unwrap())
                    && apply.styles == vec![style_id(0).unwrap()]
        )));
        assert!(imported.value.sheets[0].items.iter().any(|item| matches!(
            item,
            SheetItem::Block(block) if matches!(block.cells[0][0].value, Value::Date(_))
        )));

        assert_eq!(imported.report.fidelity(), crate::Fidelity::Lossy);
        assert!(imported.report.outcomes().iter().any(|outcome| {
            outcome.feature == "core_styles"
                && outcome.outcome == crate::FeatureOutcome::Approximated
                && outcome.locations.contains(&ConversionLocation::Xlsx {
                    part: "xl/styles.xml".to_owned(),
                    reference: Some("cellXfs[0]".to_owned()),
                })
        }));
    }

    #[test]
    fn exported_zero_decimal_number_format_re_imports_exactly() {
        let style_id = StyleId::parse("count").unwrap();
        let properties = StyleProperties {
            number: Some(NumberFormat::Decimal),
            decimals: Some(0),
            ..StyleProperties::default()
        };
        let source = Workbook {
            styles: vec![Style {
                id: style_id.clone(),
                properties: properties.clone(),
                origin: None,
            }],
            sheets: vec![Sheet {
                id: SheetId::parse("data").unwrap(),
                label: "Data".to_owned(),
                items: vec![
                    SheetItem::Block(
                        Block::new(
                            Coordinate::parse("A1").unwrap(),
                            vec![vec![Cell::new(Value::Number(1.0))]],
                        )
                        .unwrap(),
                    ),
                    SheetItem::Apply(Apply {
                        target: ApplyTarget::Range(Range::parse("A1").unwrap()),
                        styles: vec![style_id],
                        origin: None,
                    }),
                ],
                origin: None,
            }],
            ..Workbook::default()
        };

        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        assert!(
            package_text(&exported.value, "xl/styles.xml")
                .contains("<numFmt numFmtId=\"165\" formatCode=\"0\"/>")
        );
        let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
        assert_eq!(
            imported
                .value
                .styles
                .iter()
                .map(|style| style.properties.clone())
                .collect::<Vec<_>>(),
            vec![properties]
        );
        assert!(
            !imported.report.outcomes().iter().any(|outcome| {
                outcome.feature == "core_styles" && outcome.outcome != crate::FeatureOutcome::Exact
            }),
            "{:?}",
            imported.report.outcomes()
        );
    }

    #[test]
    fn defined_name_rewrite_preserves_structured_selector_headers() {
        let names = BTreeMap::from([("total".to_owned(), NameId::parse("named_total").unwrap())]);

        assert_eq!(
            translate_excel_formula(
                "SUM(sales[Total])+Total",
                FormulaNames {
                    sheets: &BTreeMap::new(),
                    names: &names,
                },
            ),
            "=SUM(sales[Total])+named_total"
        );
    }

    #[test]
    fn number_formats_without_a_marksheet_equivalent_are_reported() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let styles = package_text(&exported.value, "xl/styles.xml")
            .replacen(
                "<fonts count=\"1\">",
                "<numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"@\"/></numFmts><fonts count=\"2\">",
                1,
            )
            .replacen("<font></font>", "<font></font><font></font>", 1)
            .replacen(
                "<cellXfs count=\"1\"><xf numFmtId=\"0\"",
                "<cellXfs count=\"2\"><xf numFmtId=\"3\"",
                1,
            )
            .replacen(
                "</cellXfs>",
                "<xf numFmtId=\"164\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/></cellXfs>",
                1,
            );
        let bytes = rewrite_package(&exported.value, &[("xl/styles.xml", &styles)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        assert_eq!(imported.report.fidelity(), crate::Fidelity::Lossy);
        let details: Vec<_> = imported
            .report
            .outcomes()
            .iter()
            .filter(|outcome| {
                outcome.feature == "core_styles"
                    && outcome.outcome == crate::FeatureOutcome::Approximated
            })
            .filter_map(|outcome| outcome.detail.clone())
            .collect();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("built-in XLSX number format 3")),
            "{details:?}"
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("XLSX number format 164 (\"@\")")),
            "{details:?}"
        );
    }

    #[test]
    fn overprecise_number_format_does_not_create_an_invalid_style() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let styles = package_text(&exported.value, "xl/styles.xml")
            .replacen(
                "<fonts count=\"1\">",
                "<numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"0.0000000000000000\"/></numFmts><fonts count=\"1\">",
                1,
            )
            .replacen(
                "<cellXfs count=\"1\"><xf numFmtId=\"0\"",
                "<cellXfs count=\"1\"><xf numFmtId=\"164\"",
                1,
            );
        let bytes = rewrite_package(&exported.value, &[("xl/styles.xml", &styles)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        assert_eq!(imported.value.styles[0].properties.decimals, None);
        let text = marksheet_syntax::serialize_workbook(&imported.value).unwrap();
        assert!(!marksheet_syntax::parse(&text).has_errors());
        assert!(imported.report.outcomes().iter().any(|outcome| {
            outcome.feature == "core_styles"
                && outcome.outcome == crate::FeatureOutcome::Approximated
                && outcome.locations.contains(&ConversionLocation::Xlsx {
                    part: "xl/styles.xml".to_owned(),
                    reference: Some("cellXfs[0]".to_owned()),
                })
        }));
    }

    #[test]
    fn symbol_currency_format_does_not_create_an_invalid_style() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let styles = package_text(&exported.value, "xl/styles.xml")
            .replacen(
                "<fonts count=\"1\">",
                "<numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"[$$-409]#,##0.00\"/></numFmts><fonts count=\"1\">",
                1,
            )
            .replacen(
                "<cellXfs count=\"1\"><xf numFmtId=\"0\"",
                "<cellXfs count=\"1\"><xf numFmtId=\"164\"",
                1,
            );
        let bytes = rewrite_package(&exported.value, &[("xl/styles.xml", &styles)], &[]);
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        let properties = &imported.value.styles[0].properties;
        assert_eq!(properties.number, Some(NumberFormat::Decimal));
        assert_eq!(properties.decimals, Some(2));
        assert_eq!(properties.currency, None);
        let text = marksheet_syntax::serialize_workbook(&imported.value).unwrap();
        assert!(!marksheet_syntax::parse(&text).has_errors());
        assert!(imported.report.outcomes().iter().any(|outcome| {
            outcome.feature == "core_styles"
                && outcome.outcome == crate::FeatureOutcome::Approximated
                && outcome.locations.contains(&ConversionLocation::Xlsx {
                    part: "xl/styles.xml".to_owned(),
                    reference: Some("cellXfs[0]".to_owned()),
                })
        }));
    }

    #[test]
    fn numeric_cells_under_a_date_format_report_the_coercion() {
        let exported = export_xlsx(&basic_workbook(), ConversionLimits::default()).unwrap();
        let styles = package_text(&exported.value, "xl/styles.xml")
            .replacen("<fonts count=\"1\">", "<fonts count=\"2\">", 1)
            .replacen("<font></font>", "<font></font><font></font>", 1)
            .replacen("<cellXfs count=\"1\">", "<cellXfs count=\"2\">", 1)
            .replacen(
                "</cellXfs>",
                "<xf numFmtId=\"14\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/></cellXfs>",
                1,
            );
        let worksheet = package_text(&exported.value, "xl/worksheets/sheet1.xml")
            .replace("<c r=\"A1\"", "<c r=\"A1\" s=\"1\"")
            .replace("<v>1</v>", "<v>5</v>");
        let bytes = rewrite_package(
            &exported.value,
            &[
                ("xl/styles.xml", &styles),
                ("xl/worksheets/sheet1.xml", &worksheet),
            ],
            &[],
        );
        let imported = import_xlsx(&bytes, ConversionLimits::default()).unwrap();

        assert!(imported.value.sheets[0].items.iter().any(|item| matches!(
            item,
            SheetItem::Block(block)
                if block.cells[0][0].value
                    == Value::Date(Date::from_calendar_date(1900, Month::January, 5).unwrap())
        )));
        assert_eq!(imported.report.fidelity(), crate::Fidelity::Lossy);
        assert!(
            imported.report.outcomes().iter().any(|outcome| {
                outcome.feature == "scalar_cells"
                    && outcome.outcome == crate::FeatureOutcome::Approximated
                    && outcome.locations
                        == vec![ConversionLocation::cell(
                            SheetId::parse("data").unwrap(),
                            Coordinate::parse("A1").unwrap(),
                        )]
            }),
            "{:?}",
            imported.report.outcomes()
        );
    }

    #[test]
    fn defined_name_rewrite_recognizes_a_leading_backslash() {
        let names = BTreeMap::from([(
            "\\total".to_owned(),
            NameId::parse("escaped_total").unwrap(),
        )]);

        assert_eq!(
            translate_excel_formula(
                "SUM(\\Total)",
                FormulaNames {
                    sheets: &BTreeMap::new(),
                    names: &names,
                },
            ),
            "=SUM(escaped_total)"
        );
    }

    #[test]
    fn defined_name_rewrite_preserves_error_literals() {
        let names = BTreeMap::from([("ref".to_owned(), NameId::parse("named_ref").unwrap())]);

        assert_eq!(
            translate_excel_formula(
                "#REF!+REF",
                FormulaNames {
                    sheets: &BTreeMap::new(),
                    names: &names,
                },
            ),
            "=#REF!+named_ref"
        );
    }

    #[test]
    fn quoted_csv_table_headers_keep_xml_whitespace_across_the_round_trip() {
        for (header, reference) in [
            ("Head\nOne", "Head&#10;One"),
            ("Head\tOne", "Head&#9;One"),
            ("Head\rOne", "Head&#13;One"),
        ] {
            // Marksheet source has no spelling for a bare CR in a CSV field,
            // so that case builds the same semantic workbook directly.
            let workbook = if header.contains('\r') {
                Workbook {
                    sheets: vec![Sheet {
                        id: SheetId::parse("data").unwrap(),
                        label: "Data".to_owned(),
                        items: vec![SheetItem::Table(Table {
                            id: TableId::parse("costs").unwrap(),
                            block: Block::new(
                                Coordinate::parse("A1").unwrap(),
                                vec![
                                    vec![
                                        Cell::new(Value::Text(header.to_owned())),
                                        Cell::new(Value::Text("Other".to_owned())),
                                    ],
                                    vec![
                                        Cell::new(Value::Number(1.0)),
                                        Cell::new(Value::Number(2.0)),
                                    ],
                                ],
                            )
                            .unwrap(),
                            origin: None,
                        })],
                        origin: None,
                    }],
                    ..Workbook::default()
                }
            } else {
                let source = format!(
                    "#!marksheet 0.1\n@sheet data \"Data\"\n@table costs A1 csv\n\"{header}\",Other\n1,2\n@end\n"
                );
                let parsed = marksheet_syntax::parse(source.as_bytes());
                assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
                parsed.workbook.expect("source lowers to a workbook")
            };

            let exported = export_xlsx(&workbook, ConversionLimits::default()).unwrap();
            assert!(
                package_text(&exported.value, "xl/tables/table1.xml")
                    .contains(&format!("name=\"{reference}\"")),
                "table column {header:?} must be written as {reference}"
            );
            let imported = import_xlsx(&exported.value, ConversionLimits::default())
                .expect("the exported package must be importable");
            let headers = imported.value.sheets[0]
                .items
                .iter()
                .find_map(|item| match item {
                    SheetItem::Table(table) => Some(table.block.cells[0].clone()),
                    _ => None,
                })
                .expect("imported table");
            assert_eq!(headers[0].value, Value::Text(header.to_owned()));
        }
    }

    #[test]
    fn escaped_text_references_are_decoded_rather_than_dropped() {
        let mut source = basic_workbook();
        source.sheets[0].items = vec![SheetItem::Block(
            Block::new(
                Coordinate::parse("A1").unwrap(),
                vec![vec![Cell::new(Value::Text("a&b<c>d\re".to_owned()))]],
            )
            .unwrap(),
        )];
        let exported = export_xlsx(&source, ConversionLimits::default()).unwrap();
        let imported = import_xlsx(&exported.value, ConversionLimits::default()).unwrap();
        let value = imported.value.sheets[0]
            .items
            .iter()
            .find_map(|item| match item {
                SheetItem::Block(block) => Some(block.cells[0][0].value.clone()),
                _ => None,
            })
            .expect("imported block");
        assert_eq!(value, Value::Text("a&b<c>d\re".to_owned()));
    }
}
