use marksheet_model::{Coordinate, Fill, FillTarget, Origin, Range, Value};

use super::{FillIndex, FootprintKind, PrepareError, PrepareLimits, PreparedSheet, VirtualCell};

/// Adds one finite fill to `sheet` without changing authored cells.
pub(crate) fn expand_fill(
    sheet: &mut PreparedSheet,
    fill: &Fill,
    source_order: u64,
    limits: PrepareLimits,
) -> Result<(), PrepareError> {
    let (range, table) = resolve_target(sheet, fill, source_order)?;
    let cell_count = check_range_limit(range, limits.max_range_cells, fill.origin)?;
    let pending_total = u64::try_from(sheet.virtual_cells.len())
        .map_err(|_| PrepareError::SourceOrderOverflow)?
        .checked_add(cell_count)
        .ok_or(PrepareError::VirtualCellLimitExceeded {
            sheet: sheet.id.clone(),
            limit: limits.max_virtual_cells,
            origin: fill.origin,
        })?;
    if pending_total > limits.max_virtual_cells {
        return Err(PrepareError::VirtualCellLimitExceeded {
            sheet: sheet.id.clone(),
            limit: limits.max_virtual_cells,
            origin: fill.origin,
        });
    }

    // Validate every source field before writing a virtual index entry.  The
    // whole prepared workbook is discarded on error, but this ordering also
    // avoids exposing a partially expanded fill to a future incremental API.
    for_each_coordinate(range, |coordinate| {
        let Some(authored) = sheet.authored_cells.get(&coordinate) else {
            return Err(PrepareError::FillTargetsAbsentCell {
                sheet: sheet.id.clone(),
                coordinate,
                origin: fill.origin,
            });
        };
        if !matches!(authored.cell.value, Value::Blank) {
            return Err(PrepareError::FillTargetsNonBlankCell {
                sheet: sheet.id.clone(),
                coordinate,
                origin: fill.origin,
            });
        }
        if let Some(existing) = sheet.virtual_cells.get(&coordinate) {
            return Err(PrepareError::OverlappingFills {
                sheet: sheet.id.clone(),
                coordinate,
                first_origin: existing.fill_origin,
                second_origin: fill.origin,
            });
        }
        Ok(())
    })?;

    let fill_anchor = range.start;
    for_each_coordinate(range, |coordinate| {
        let table_row = table
            .as_ref()
            .and_then(|_| sheet.current_row_context(coordinate));
        let previous = sheet.virtual_cells.insert(
            coordinate,
            VirtualCell {
                formula: fill.formula.clone(),
                fill_origin: fill.origin,
                fill_anchor,
                source_order,
                table_row,
            },
        );
        debug_assert!(
            previous.is_none(),
            "validated fill destinations cannot overlap"
        );
        Ok(())
    })?;
    sheet.fills.push(FillIndex {
        range,
        formula: fill.formula.clone(),
        origin: fill.origin,
        source_order,
        table,
    });
    Ok(())
}

fn resolve_target(
    sheet: &PreparedSheet,
    fill: &Fill,
    source_order: u64,
) -> Result<(Range, Option<marksheet_model::TableId>), PrepareError> {
    match &fill.target {
        FillTarget::Range(range) => {
            let owners: Vec<_> = sheet
                .footprints
                .iter()
                .filter(|footprint| {
                    footprint.source_order < source_order
                        && footprint.range.contains(range.start)
                        && footprint.range.contains(range.end)
                })
                .collect();
            match owners.len() {
                0 => Err(PrepareError::FillHasNoOwner {
                    sheet: sheet.id.clone(),
                    target: *range,
                    origin: fill.origin,
                }),
                1 => {
                    let table = match &owners[0].kind {
                        FootprintKind::Block => None,
                        FootprintKind::Table(table) => Some(table.clone()),
                    };
                    Ok((*range, table))
                }
                _ => Err(PrepareError::FillHasMultipleOwners {
                    sheet: sheet.id.clone(),
                    target: *range,
                    origin: fill.origin,
                }),
            }
        }
        FillTarget::TableColumn { table, header } => {
            let Some(resolved) = sheet.tables.get(table) else {
                return Err(PrepareError::UnresolvedTable {
                    table: table.clone(),
                    origin: fill.origin,
                });
            };
            if resolved.source_order >= source_order {
                return Err(PrepareError::FillMustFollowOwner {
                    sheet: sheet.id.clone(),
                    table: table.clone(),
                    origin: fill.origin,
                });
            }
            if !resolved.headers.contains_key(header) {
                return Err(PrepareError::UnresolvedTableHeader {
                    table: table.clone(),
                    header: header.clone(),
                    origin: fill.origin,
                });
            }
            let Some(range) = resolved.data_column(header) else {
                return Err(PrepareError::HeaderOnlyTableFill {
                    sheet: sheet.id.clone(),
                    table: table.clone(),
                    header: header.clone(),
                    origin: fill.origin,
                });
            };
            Ok((range, Some(table.clone())))
        }
    }
}

/// Counts an inclusive range with checked `u64` arithmetic and applies a
/// caller-supplied bound before any coordinate iteration starts.
pub(crate) fn check_range_limit(
    range: Range,
    limit: u64,
    origin: Option<Origin>,
) -> Result<u64, PrepareError> {
    let width = range.width()?;
    let height = range.height()?;
    let count = width
        .checked_mul(height)
        .ok_or(PrepareError::RangeLimitExceeded {
            range,
            limit,
            origin,
        })?;
    if count > limit {
        return Err(PrepareError::RangeLimitExceeded {
            range,
            limit,
            origin,
        });
    }
    Ok(count)
}

fn for_each_coordinate(
    range: Range,
    mut visit: impl FnMut(Coordinate) -> Result<(), PrepareError>,
) -> Result<(), PrepareError> {
    let mut row = range.start.row;
    loop {
        let mut column = range.start.column;
        loop {
            visit(Coordinate { column, row })?;
            if column == range.end.column {
                break;
            }
            column = column.checked_add(1).ok_or(PrepareError::Coordinate {
                source: marksheet_model::CoordinateError::Overflow,
            })?;
        }
        if row == range.end.row {
            break;
        }
        row = row.checked_add(1).ok_or(PrepareError::Coordinate {
            source: marksheet_model::CoordinateError::Overflow,
        })?;
    }
    Ok(())
}
