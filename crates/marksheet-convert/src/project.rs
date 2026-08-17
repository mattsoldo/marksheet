#![allow(clippy::too_many_lines)] // Projection validation is one atomic source-order pass.

use std::collections::{BTreeMap, BTreeSet};

use marksheet_calc::{
    PreparedWorkbook,
    formula::{FormulaTemplate, ParseLimits, format_formula, parse},
    prepare::PrepareLimits,
};
use marksheet_model::{
    ApplyTarget, ColumnGeometry, Coordinate, FormulaSource, Name, Range, RowGeometry, SheetId,
    SheetItem, StyleId, StyleProperties, TableId, TableRegion, Value, Workbook,
};

use crate::{ConversionLimits, ConvertError, ConvertErrorCode};

pub(crate) const XLSX_MAX_COLUMN: u64 = 16_384;
pub(crate) const XLSX_MAX_ROW: u64 = 1_048_576;

// A row with custom height needs both an entry in the temporary height map and
// a `<row>` element. This is deliberately larger than the shortest possible
// XML spelling, so geometry cannot consume the output/allocation budget at a
// materially different rate from ordinary cells before the ZIP writer runs.
const ROW_GEOMETRY_WORK_BYTES: u64 = 64;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectedCell {
    pub value: Value,
    pub from_fill: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectedTable {
    pub id: TableId,
    pub range: Range,
    pub headers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectedSheet {
    pub id: SheetId,
    pub label: String,
    pub cells: BTreeMap<Coordinate, ProjectedCell>,
    /// Resolved styles include style-only cells that have no authored value.
    pub styles: BTreeMap<Coordinate, StyleProperties>,
    pub tables: Vec<ProjectedTable>,
    pub columns: Vec<ColumnGeometry>,
    pub rows: Vec<RowGeometry>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectedWorkbook {
    pub sheets: Vec<ProjectedSheet>,
    pub names: Vec<Name>,
}

/// Workbook-wide budget for expanding row-geometry declarations during XLSX
/// emission. The source IR stores row geometry as ranges, while OOXML needs a
/// row element for every effective row; charge every declaration visit before
/// materializing that map.
#[derive(Debug)]
pub(crate) struct RowGeometryWorkBudget {
    used: u64,
    maximum: u64,
    per_sheet_maximum: u64,
}

impl RowGeometryWorkBudget {
    pub(crate) fn for_export(limits: ConversionLimits) -> Self {
        let output_rows = limits.max_output_bytes / ROW_GEOMETRY_WORK_BYTES;
        let package_rows = limits.max_zip_total_uncompressed_bytes / ROW_GEOMETRY_WORK_BYTES;
        let per_sheet_rows = limits.max_zip_entry_uncompressed_bytes / ROW_GEOMETRY_WORK_BYTES;
        let maximum = limits.max_cells.min(output_rows).min(package_rows);
        Self {
            used: 0,
            maximum,
            per_sheet_maximum: maximum.min(per_sheet_rows),
        }
    }

    fn charge(&mut self, sheet_used: u64, count: u64) -> Result<(), ConvertError> {
        let next_sheet = sheet_used
            .checked_add(count)
            .ok_or_else(|| limit("row geometry expansion count overflow"))?;
        if next_sheet > self.per_sheet_maximum {
            return Err(limit(
                "row geometry expansion exceeds the per-sheet construction budget",
            ));
        }
        let next = self
            .used
            .checked_add(count)
            .ok_or_else(|| limit("aggregate row geometry expansion count overflow"))?;
        if next > self.maximum {
            return Err(limit(
                "aggregate row geometry expansion exceeds the configured construction budget",
            ));
        }
        self.used = next;
        Ok(())
    }
}

pub(crate) fn project(
    workbook: &Workbook,
    limits: ConversionLimits,
) -> Result<ProjectedWorkbook, ConvertError> {
    if workbook.sheets.len() > limits.max_sheets {
        return Err(limit("sheet count exceeds the configured limit"));
    }
    if workbook.styles.len() > limits.max_styles {
        return Err(limit("style count exceeds the configured limit"));
    }

    let prepared = PreparedWorkbook::build(
        workbook,
        PrepareLimits {
            max_range_cells: limits.max_cells,
            max_virtual_cells: limits.max_cells,
        },
    )
    .map_err(|error| {
        ConvertError::new(
            ConvertErrorCode::InvalidWorkbook,
            format!("workbook cannot be projected: {error}"),
        )
    })?;

    let style_definitions: BTreeMap<StyleId, &StyleProperties> = workbook
        .styles
        .iter()
        .map(|style| (style.id.clone(), &style.properties))
        .collect();
    if style_definitions.len() != workbook.styles.len() {
        return Err(ConvertError::new(
            ConvertErrorCode::InvalidWorkbook,
            "workbook contains duplicate style identifiers",
        ));
    }

    let mut total_cells = 0_u64;
    let mut total_formulas = 0_u64;
    let mut total_apply_visits = 0_u64;
    let mut total_tables = 0_usize;
    let mut total_geometry_declarations = 0_u64;
    let mut projected_sheets = Vec::with_capacity(prepared.sheets.len());
    for (source_sheet, prepared_sheet) in workbook.sheets.iter().zip(&prepared.sheets) {
        let mut cells = BTreeMap::new();
        for (coordinate, authored) in &prepared_sheet.authored_cells {
            check_xlsx_coordinate(*coordinate)?;
            check_value(&authored.cell.value, limits)?;
            cells.insert(
                *coordinate,
                ProjectedCell {
                    value: authored.cell.value.clone(),
                    from_fill: false,
                },
            );
        }

        let formula_limits = ParseLimits {
            max_source_bytes: limits.max_string_bytes,
            max_tokens: limits.max_string_bytes.min(100_000),
            max_depth: limits.max_xml_depth.max(1),
            max_nodes: limits.max_string_bytes.min(100_000),
            max_function_arguments: limits.max_string_bytes.min(10_000),
        };
        for (coordinate, virtual_cell) in &prepared_sheet.virtual_cells {
            check_xlsx_coordinate(*coordinate)?;
            let parsed =
                parse(virtual_cell.formula.as_str(), &formula_limits).map_err(|error| {
                    ConvertError::new(
                        ConvertErrorCode::InvalidWorkbook,
                        format!("fill formula cannot be converted at {coordinate}: {error}"),
                    )
                })?;
            let adjusted = FormulaTemplate::new(virtual_cell.fill_anchor, parsed)
                .bind(*coordinate)
                .map_err(|error| {
                    ConvertError::new(
                        ConvertErrorCode::InvalidWorkbook,
                        format!("fill formula cannot be copied to {coordinate}: {error}"),
                    )
                })?;
            let adjusted = format_formula(&adjusted).map_err(|error| {
                ConvertError::new(
                    ConvertErrorCode::Internal,
                    format!("adjusted formula cannot be formatted: {error}"),
                )
            })?;
            let formula = FormulaSource::new(adjusted).map_err(|error| {
                ConvertError::new(ConvertErrorCode::Internal, error.to_string())
            })?;
            cells.insert(
                *coordinate,
                ProjectedCell {
                    value: Value::Formula(formula),
                    from_fill: true,
                },
            );
        }
        total_cells = total_cells
            .checked_add(u64::try_from(cells.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| limit("cell count overflow"))?;
        if total_cells > limits.max_cells {
            return Err(limit("projected cell count exceeds the configured limit"));
        }
        let sheet_formulas = u64::try_from(
            cells
                .values()
                .filter(|cell| matches!(cell.value, Value::Formula(_)))
                .count(),
        )
        .unwrap_or(u64::MAX);
        total_formulas = total_formulas
            .checked_add(sheet_formulas)
            .ok_or_else(|| limit("formula count overflow"))?;
        if total_formulas > limits.max_formulas {
            return Err(limit(
                "projected formula count exceeds the configured limit",
            ));
        }

        let tables = prepared_sheet
            .tables
            .values()
            .map(|table| {
                check_xlsx_range(table.footprint)?;
                let mut headers = vec![String::new(); table.headers.len()];
                for (header, coordinate) in &table.headers {
                    let offset = coordinate
                        .column
                        .checked_sub(table.footprint.start.column)
                        .ok_or_else(|| invalid("table header precedes its footprint"))?;
                    let index = usize::try_from(offset)
                        .map_err(|_| limit("table width exceeds addressable memory"))?;
                    let slot = headers
                        .get_mut(index)
                        .ok_or_else(|| invalid("table header falls outside its footprint"))?;
                    slot.clone_from(header);
                }
                Ok(ProjectedTable {
                    id: table.id.clone(),
                    range: table.footprint,
                    headers,
                })
            })
            .collect::<Result<Vec<_>, ConvertError>>()?;
        total_tables = total_tables
            .checked_add(tables.len())
            .ok_or_else(|| limit("table count overflow"))?;
        if total_tables > limits.max_tables {
            return Err(limit("workbook table count exceeds the configured limit"));
        }

        let mut styles = BTreeMap::new();
        for item in &source_sheet.items {
            let SheetItem::Apply(apply) = item else {
                continue;
            };
            let target = resolve_apply_target(&apply.target, prepared_sheet)?;
            check_xlsx_range(target)?;
            let target_width = target
                .width()
                .map_err(|error| invalid(&error.to_string()))?;
            let target_height = target
                .height()
                .map_err(|error| invalid(&error.to_string()))?;
            let target_area = target_width
                .checked_mul(target_height)
                .ok_or_else(|| limit("style application area overflow"))?;
            total_apply_visits = total_apply_visits
                .checked_add(target_area)
                .ok_or_else(|| limit("aggregate style application work overflow"))?;
            if total_apply_visits > limits.max_cells {
                return Err(limit(
                    "aggregate style application work exceeds the configured cell limit",
                ));
            }
            let mut layer = StyleProperties::default();
            for style_id in &apply.styles {
                let properties = style_definitions.get(style_id).ok_or_else(|| {
                    ConvertError::new(
                        ConvertErrorCode::InvalidWorkbook,
                        format!("unresolved style {style_id}"),
                    )
                })?;
                merge_style(&mut layer, properties);
            }
            for_each_coordinate(target, limits.max_cells, |coordinate| {
                let resolved = styles.entry(coordinate).or_default();
                merge_style(resolved, &layer);
                Ok(())
            })?;
        }
        let style_only = u64::try_from(
            styles
                .keys()
                .filter(|coordinate| !cells.contains_key(coordinate))
                .count(),
        )
        .unwrap_or(u64::MAX);
        total_cells = total_cells
            .checked_add(style_only)
            .ok_or_else(|| limit("style-only coordinate count overflow"))?;
        if total_cells > limits.max_cells {
            return Err(limit(
                "projected value and style-only coordinates exceed the configured cell limit",
            ));
        }

        let columns: Vec<ColumnGeometry> = source_sheet
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::ColumnGeometry(value) => Some(value.clone()),
                _ => None,
            })
            .collect();
        let rows: Vec<RowGeometry> = source_sheet
            .items
            .iter()
            .filter_map(|item| match item {
                SheetItem::RowGeometry(value) => Some(value.clone()),
                _ => None,
            })
            .collect();
        validate_geometry(&columns, &rows)?;
        let declaration_count = u64::try_from(columns.len())
            .ok()
            .zip(u64::try_from(rows.len()).ok())
            .and_then(|(columns, rows)| columns.checked_add(rows))
            .unwrap_or(u64::MAX);
        total_geometry_declarations = total_geometry_declarations
            .checked_add(declaration_count)
            .ok_or_else(|| limit("geometry declaration count overflow"))?;
        if total_geometry_declarations > limits.max_cells {
            return Err(limit(
                "aggregate geometry declaration count exceeds the configured cell limit",
            ));
        }

        projected_sheets.push(ProjectedSheet {
            id: source_sheet.id.clone(),
            label: source_sheet.label.clone(),
            cells,
            styles,
            tables,
            columns,
            rows,
        });
    }

    Ok(ProjectedWorkbook {
        sheets: projected_sheets,
        names: workbook.names.clone(),
    })
}

fn resolve_apply_target(
    target: &ApplyTarget,
    sheet: &marksheet_calc::prepare::PreparedSheet,
) -> Result<Range, ConvertError> {
    match target {
        ApplyTarget::Range(range) => Ok(*range),
        ApplyTarget::Table { table, region } => {
            let table = sheet.tables.get(table).ok_or_else(|| {
                invalid("style application refers to a table on another or missing sheet")
            })?;
            match region {
                TableRegion::Headers => Ok(Range {
                    start: table.footprint.start,
                    end: Coordinate {
                        column: table.footprint.end.column,
                        row: table.footprint.start.row,
                    },
                }),
                TableRegion::Data => table
                    .data_range
                    .ok_or_else(|| invalid("header-only table has no data style region")),
                TableRegion::Column { header } => table
                    .data_column(header)
                    .ok_or_else(|| invalid("table style refers to an unknown or empty column")),
            }
        }
    }
}

pub(crate) fn for_each_coordinate(
    range: Range,
    max_cells: u64,
    mut visit: impl FnMut(Coordinate) -> Result<(), ConvertError>,
) -> Result<(), ConvertError> {
    let area = range
        .width()
        .and_then(|width| range.height().map(|height| (width, height)))
        .map_err(|error| invalid(&error.to_string()))?;
    let cells = area
        .0
        .checked_mul(area.1)
        .ok_or_else(|| limit("range cell count overflow"))?;
    if cells > max_cells {
        return Err(limit("range exceeds the configured cell limit"));
    }
    for row in range.start.row..=range.end.row {
        for column in range.start.column..=range.end.column {
            visit(Coordinate { column, row })?;
        }
    }
    Ok(())
}

pub(crate) fn check_xlsx_coordinate(coordinate: Coordinate) -> Result<(), ConvertError> {
    if coordinate.column > XLSX_MAX_COLUMN || coordinate.row > XLSX_MAX_ROW {
        return Err(invalid(&format!(
            "cell {coordinate} exceeds the XLSX grid (XFD1048576)"
        )));
    }
    Ok(())
}

pub(crate) fn check_xlsx_range(range: Range) -> Result<(), ConvertError> {
    check_xlsx_coordinate(range.start)?;
    check_xlsx_coordinate(range.end)
}

fn check_value(value: &Value, limits: ConversionLimits) -> Result<(), ConvertError> {
    let length = match value {
        Value::Text(text) => text.len(),
        Value::Formula(formula) => formula.as_str().len(),
        _ => 0,
    };
    if length > limits.max_string_bytes {
        return Err(limit(
            "cell text or formula exceeds the configured string limit",
        ));
    }
    if let Value::Number(number) = value {
        if !number.is_finite() {
            return Err(invalid("non-finite numbers cannot be converted"));
        }
    }
    Ok(())
}

fn validate_geometry(columns: &[ColumnGeometry], rows: &[RowGeometry]) -> Result<(), ConvertError> {
    for geometry in columns {
        if geometry.columns.end > XLSX_MAX_COLUMN
            || !geometry.width.is_finite()
            || geometry.width <= 0.0
        {
            return Err(invalid("column geometry is outside XLSX limits"));
        }
    }
    for geometry in rows {
        if geometry.rows.end > XLSX_MAX_ROW
            || !geometry.height.is_finite()
            || geometry.height <= 0.0
        {
            return Err(invalid("row geometry is outside XLSX limits"));
        }
    }
    Ok(())
}

pub(crate) fn merge_style(target: &mut StyleProperties, layer: &StyleProperties) {
    macro_rules! property {
        ($name:ident) => {
            if layer.$name.is_some() {
                target.$name.clone_from(&layer.$name);
            }
        };
    }
    property!(bold);
    property!(italic);
    property!(wrap);
    property!(text_color);
    property!(fill);
    property!(font_size);
    property!(align);
    property!(valign);
    property!(number);
    property!(decimals);
    property!(currency);
}

pub(crate) fn effective_column_runs(declarations: &[ColumnGeometry]) -> Vec<(u64, u64, f64)> {
    // The previous implementation scanned every declaration at every range
    // boundary. A sweep keeps the source-order winner in an ordered active
    // set, avoiding quadratic work for overlapping column declarations.
    let mut boundaries = BTreeMap::<u64, (Vec<usize>, Vec<usize>)>::new();
    for (index, declaration) in declarations.iter().enumerate() {
        boundaries
            .entry(declaration.columns.start)
            .or_default()
            .0
            .push(index);
        if let Some(after) = declaration.columns.end.checked_add(1) {
            boundaries.entry(after).or_default().1.push(index);
        }
    }
    let points: Vec<_> = boundaries.keys().copied().collect();
    let mut result: Vec<(u64, u64, f64)> = Vec::new();
    let mut active = BTreeSet::<usize>::new();
    for (index, start) in points.iter().copied().enumerate() {
        let (starts, ends) = &boundaries[&start];
        for declaration in ends {
            active.remove(declaration);
        }
        active.extend(starts.iter().copied());
        if let (Some(end), Some(declaration)) = (
            points.get(index + 1).and_then(|next| next.checked_sub(1)),
            active.last(),
        ) {
            let width = declarations[*declaration].width;
            if let Some(previous) = result.last_mut() {
                if previous.1.checked_add(1) == Some(start)
                    && previous.2.to_bits() == width.to_bits()
                {
                    previous.1 = end;
                    continue;
                }
            }
            result.push((start, end, width));
        }
    }
    result
}

pub(crate) fn effective_row_heights(
    declarations: &[RowGeometry],
    max_cells: u64,
    work_budget: &mut RowGeometryWorkBudget,
) -> Result<BTreeMap<u64, f64>, ConvertError> {
    let mut result = BTreeMap::new();
    let mut visited = 0_u64;
    for declaration in declarations {
        let count = declaration
            .rows
            .end
            .checked_sub(declaration.rows.start)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| limit("row geometry count overflow"))?;
        visited = visited
            .checked_add(count)
            .ok_or_else(|| limit("row geometry count overflow"))?;
        if visited > max_cells {
            return Err(limit("row geometry expansion exceeds the configured limit"));
        }
        work_budget.charge(visited - count, count)?;
        for row in declaration.rows.start..=declaration.rows.end {
            result.insert(row, declaration.height);
        }
    }
    Ok(result)
}

fn invalid(message: &str) -> ConvertError {
    ConvertError::new(ConvertErrorCode::InvalidWorkbook, message)
}

fn limit(message: &str) -> ConvertError {
    ConvertError::new(ConvertErrorCode::ResourceLimit, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::ColumnRange;

    #[test]
    fn column_geometry_uses_last_declaration_without_quadratic_boundary_scans() {
        let declarations = vec![
            ColumnGeometry {
                columns: ColumnRange::new(1, 8).unwrap(),
                width: 10.0,
                origin: None,
            },
            ColumnGeometry {
                columns: ColumnRange::new(3, 6).unwrap(),
                width: 20.0,
                origin: None,
            },
            ColumnGeometry {
                columns: ColumnRange::new(5, 10).unwrap(),
                width: 30.0,
                origin: None,
            },
        ];

        assert_eq!(
            effective_column_runs(&declarations),
            vec![(1, 2, 10.0), (3, 4, 20.0), (5, 10, 30.0)]
        );
    }
}
