use std::collections::BTreeMap;

use marksheet_model::{
    Block, Cell, Coordinate, Name, NameId, NameTarget, Origin, Range, Sheet, SheetId, SheetItem,
    Table, TableId, Value, Workbook,
};

use super::{PrepareError, PrepareLimits};

/// An authored field, including authored `Blank` fields.
///
/// Absence from [`PreparedSheet::authored_cells`] is semantically different
/// from an authored cell whose value is [`Value::Blank`].
#[derive(Clone, Debug, PartialEq)]
pub struct AuthoredCell {
    pub cell: Cell,
    pub footprint_order: u64,
    pub source_order: u64,
}

/// One source reservation, retained in sheet item order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FootprintIndex {
    pub range: Range,
    pub kind: FootprintKind,
    pub source_order: u64,
    pub origin: Option<Origin>,
}

/// The source construct reserving a footprint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FootprintKind {
    Block,
    Table(TableId),
}

/// A table index suitable for structured-reference resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct TableIndex {
    pub id: TableId,
    pub sheet: SheetId,
    pub footprint: Range,
    pub headers: BTreeMap<String, Coordinate>,
    /// The data-only part of the table. `None` means that the table has only a
    /// header record and has no legal `@fill` destinations.
    pub data_range: Option<Range>,
    pub source_order: u64,
    pub origin: Option<Origin>,
}

impl TableIndex {
    /// Returns a concrete data-column range, never including the header cell.
    #[must_use]
    pub fn data_column(&self, header: &str) -> Option<Range> {
        let header_cell = *self.headers.get(header)?;
        self.data_range.map(|data| Range {
            start: Coordinate {
                column: header_cell.column,
                row: data.start.row,
            },
            end: Coordinate {
                column: header_cell.column,
                row: data.end.row,
            },
        })
    }
}

/// Current-row context for a virtual or authored table-data cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRowContext {
    pub table: TableId,
    /// Zero-based index into the table's data rows.
    pub data_row_index: u64,
}

/// A formula supplied by a fill directive rather than a source CSV field.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualCell {
    pub formula: marksheet_model::FormulaSource,
    pub fill_origin: Option<Origin>,
    /// The upper-left destination at which the authored fill formula starts.
    /// The formula parser uses this to adjust relative A1 references.
    pub fill_anchor: Coordinate,
    pub source_order: u64,
    pub table_row: Option<TableRowContext>,
}

/// A fully resolved fill target in source order.
#[derive(Clone, Debug, PartialEq)]
pub struct FillIndex {
    pub range: Range,
    pub formula: marksheet_model::FormulaSource,
    pub origin: Option<Origin>,
    pub source_order: u64,
    pub table: Option<TableId>,
}

/// A resolved workbook-scoped name.
#[derive(Clone, Debug, PartialEq)]
pub struct NameIndex {
    pub id: NameId,
    pub target: NameTarget,
    pub origin: Option<Origin>,
}

/// All calculation-facing indexes for one sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedSheet {
    pub id: SheetId,
    pub label: String,
    pub origin: Option<Origin>,
    pub authored_cells: BTreeMap<Coordinate, AuthoredCell>,
    pub footprints: Vec<FootprintIndex>,
    pub tables: BTreeMap<TableId, TableIndex>,
    pub fills: Vec<FillIndex>,
    /// Virtual fill formulas are held separately from authored cells so no
    /// calculation operation needs to rewrite the source model.
    pub virtual_cells: BTreeMap<Coordinate, VirtualCell>,
}

impl PreparedSheet {
    /// Returns the authored cell, including authored blank cells, if present.
    #[must_use]
    pub fn authored_cell(&self, coordinate: Coordinate) -> Option<&AuthoredCell> {
        self.authored_cells.get(&coordinate)
    }

    /// Returns the fill-derived cell without materializing it into source.
    #[must_use]
    pub fn virtual_cell(&self, coordinate: Coordinate) -> Option<&VirtualCell> {
        self.virtual_cells.get(&coordinate)
    }

    /// Finds the table data-row context for `coordinate`.
    #[must_use]
    pub fn current_row_context(&self, coordinate: Coordinate) -> Option<TableRowContext> {
        self.tables.values().find_map(|table| {
            let data = table.data_range?;
            data.contains(coordinate).then(|| TableRowContext {
                table: table.id.clone(),
                data_row_index: coordinate.row - data.start.row,
            })
        })
    }
}

/// Deterministic sparse indexes for an entire source workbook.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedWorkbook {
    /// Sheets in source order.
    pub sheets: Vec<PreparedSheet>,
    pub names: BTreeMap<NameId, NameIndex>,
    sheet_positions: BTreeMap<SheetId, usize>,
    table_positions: BTreeMap<TableId, usize>,
}

impl PreparedWorkbook {
    /// Builds sparse calculation indexes and expands finite fill destinations.
    ///
    /// All validation is completed before this method returns successfully.
    /// On any error, no partially prepared workbook is exposed.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when workbook indexing, reference validation,
    /// footprint validation, or finite fill expansion fails.
    pub fn build(workbook: &Workbook, limits: PrepareLimits) -> Result<Self, PrepareError> {
        let mut sheets = Vec::with_capacity(workbook.sheets.len());
        let mut sheet_positions = BTreeMap::new();

        for source_sheet in &workbook.sheets {
            if sheet_positions
                .insert(source_sheet.id.clone(), sheets.len())
                .is_some()
            {
                return Err(PrepareError::DuplicateSheet {
                    sheet: source_sheet.id.clone(),
                    origin: source_sheet.origin,
                });
            }
            sheets.push(build_sheet(source_sheet, limits)?);
        }

        let mut table_positions = BTreeMap::new();
        for (sheet_position, sheet) in sheets.iter().enumerate() {
            for (id, table) in &sheet.tables {
                if table_positions.insert(id.clone(), sheet_position).is_some() {
                    return Err(PrepareError::DuplicateTable {
                        table: id.clone(),
                        origin: table.origin,
                    });
                }
            }
        }

        let names = index_names(
            workbook,
            &sheet_positions,
            &table_positions,
            &sheets,
            limits,
        )?;
        for (table, sheet_position) in &table_positions {
            if names.keys().any(|name| name.as_str() == table.as_str()) {
                let table = &sheets[*sheet_position].tables[table];
                return Err(PrepareError::TableNameConflict {
                    identifier: table.id.as_str().to_owned(),
                    origin: table.origin,
                });
            }
        }

        Ok(Self {
            sheets,
            names,
            sheet_positions,
            table_positions,
        })
    }

    #[must_use]
    pub fn sheet(&self, id: &SheetId) -> Option<&PreparedSheet> {
        self.sheet_positions
            .get(id)
            .and_then(|position| self.sheets.get(*position))
    }

    #[must_use]
    pub fn table(&self, id: &TableId) -> Option<&TableIndex> {
        self.table_positions
            .get(id)
            .and_then(|sheet| self.sheets.get(*sheet))
            .and_then(|sheet| sheet.tables.get(id))
    }
}

fn build_sheet(sheet: &Sheet, limits: PrepareLimits) -> Result<PreparedSheet, PrepareError> {
    let mut prepared = PreparedSheet {
        id: sheet.id.clone(),
        label: sheet.label.clone(),
        origin: sheet.origin,
        authored_cells: BTreeMap::new(),
        footprints: Vec::new(),
        tables: BTreeMap::new(),
        fills: Vec::new(),
        virtual_cells: BTreeMap::new(),
    };

    for (item_position, item) in sheet.items.iter().enumerate() {
        let source_order =
            u64::try_from(item_position).map_err(|_| PrepareError::SourceOrderOverflow)?;
        match item {
            SheetItem::Block(block) => {
                add_block(&mut prepared, block, None, block.origin, source_order)?;
            }
            SheetItem::Table(table) => add_table(&mut prepared, table, source_order)?,
            SheetItem::Fill(fill) => {
                super::fill::expand_fill(&mut prepared, fill, source_order, limits)?;
            }
            SheetItem::Apply(_)
            | SheetItem::ColumnGeometry(_)
            | SheetItem::RowGeometry(_)
            | SheetItem::Extension(_) => {}
        }
    }
    Ok(prepared)
}

fn add_block(
    prepared: &mut PreparedSheet,
    block: &Block,
    table: Option<TableId>,
    origin: Option<Origin>,
    source_order: u64,
) -> Result<(), PrepareError> {
    validate_block_shape(block, &prepared.id)?;
    let range = block.footprint()?.range()?;
    if let Some(existing) = prepared
        .footprints
        .iter()
        .find(|existing| existing.range.overlaps(range))
    {
        return Err(PrepareError::OverlappingFootprints {
            sheet: prepared.id.clone(),
            first_origin: existing.origin,
            second_origin: origin,
        });
    }

    let footprint_order =
        u64::try_from(prepared.footprints.len()).map_err(|_| PrepareError::SourceOrderOverflow)?;
    for (row_offset, row) in block.cells.iter().enumerate() {
        let row_offset =
            u64::try_from(row_offset).map_err(|_| PrepareError::SourceOrderOverflow)?;
        for (column_offset, cell) in row.iter().enumerate() {
            let column_offset =
                u64::try_from(column_offset).map_err(|_| PrepareError::SourceOrderOverflow)?;
            let coordinate = block.anchor.offset(column_offset, row_offset)?;
            let previous = prepared.authored_cells.insert(
                coordinate,
                AuthoredCell {
                    cell: cell.clone(),
                    footprint_order,
                    source_order,
                },
            );
            debug_assert!(
                previous.is_none(),
                "non-overlapping blocks cannot share a cell"
            );
        }
    }
    prepared.footprints.push(FootprintIndex {
        range,
        kind: table.map_or(FootprintKind::Block, FootprintKind::Table),
        source_order,
        origin,
    });
    Ok(())
}

fn add_table(
    prepared: &mut PreparedSheet,
    table: &Table,
    source_order: u64,
) -> Result<(), PrepareError> {
    if prepared.tables.contains_key(&table.id) {
        return Err(PrepareError::DuplicateTable {
            table: table.id.clone(),
            origin: table.origin,
        });
    }
    validate_block_shape(&table.block, &prepared.id)?;
    let footprint = table.block.footprint()?.range()?;
    let mut headers = BTreeMap::new();
    for (column_offset, header) in table.block.cells[0].iter().enumerate() {
        let Value::Text(header_text) = &header.value else {
            return Err(PrepareError::InvalidTableHeader {
                table: table.id.clone(),
                origin: header.origin.or(table.origin),
            });
        };
        if header_text.is_empty() {
            return Err(PrepareError::InvalidTableHeader {
                table: table.id.clone(),
                origin: header.origin.or(table.origin),
            });
        }
        let column_offset =
            u64::try_from(column_offset).map_err(|_| PrepareError::SourceOrderOverflow)?;
        let coordinate = table.block.anchor.offset(column_offset, 0)?;
        if headers.insert(header_text.clone(), coordinate).is_some() {
            return Err(PrepareError::DuplicateTableHeader {
                table: table.id.clone(),
                header: header_text.clone(),
                origin: header.origin.or(table.origin),
            });
        }
    }
    let data_range = if table.block.cells.len() > 1 {
        let first_data_row = footprint
            .start
            .row
            .checked_add(1)
            .ok_or(marksheet_model::CoordinateError::Overflow)?;
        Some(Range {
            start: Coordinate {
                column: footprint.start.column,
                row: first_data_row,
            },
            end: footprint.end,
        })
    } else {
        None
    };

    add_block(
        prepared,
        &table.block,
        Some(table.id.clone()),
        table.origin.or(table.block.origin),
        source_order,
    )?;
    prepared.tables.insert(
        table.id.clone(),
        TableIndex {
            id: table.id.clone(),
            sheet: prepared.id.clone(),
            footprint,
            headers,
            data_range,
            source_order,
            origin: table.origin.or(table.block.origin),
        },
    );
    Ok(())
}

fn validate_block_shape(block: &Block, sheet: &SheetId) -> Result<(), PrepareError> {
    let Some(first_row) = block.cells.first() else {
        return Err(PrepareError::MalformedBlock {
            sheet: sheet.clone(),
            origin: block.origin,
        });
    };
    if first_row.is_empty() || block.cells.iter().any(|row| row.len() != first_row.len()) {
        return Err(PrepareError::MalformedBlock {
            sheet: sheet.clone(),
            origin: block.origin,
        });
    }
    Ok(())
}

fn index_names(
    workbook: &Workbook,
    sheet_positions: &BTreeMap<SheetId, usize>,
    table_positions: &BTreeMap<TableId, usize>,
    sheets: &[PreparedSheet],
    limits: PrepareLimits,
) -> Result<BTreeMap<NameId, NameIndex>, PrepareError> {
    let mut names = BTreeMap::new();
    for name in &workbook.names {
        validate_name(name, sheet_positions, table_positions, sheets, limits)?;
        if names
            .insert(
                name.id.clone(),
                NameIndex {
                    id: name.id.clone(),
                    target: name.target.clone(),
                    origin: name.origin,
                },
            )
            .is_some()
        {
            return Err(PrepareError::DuplicateName {
                name: name.id.clone(),
                origin: name.origin,
            });
        }
    }
    Ok(names)
}

fn validate_name(
    name: &Name,
    sheet_positions: &BTreeMap<SheetId, usize>,
    table_positions: &BTreeMap<TableId, usize>,
    sheets: &[PreparedSheet],
    limits: PrepareLimits,
) -> Result<(), PrepareError> {
    match &name.target {
        NameTarget::Cell(cell) => {
            if !sheet_positions.contains_key(&cell.sheet) {
                return Err(PrepareError::UnresolvedSheet {
                    sheet: cell.sheet.clone(),
                    origin: name.origin,
                });
            }
        }
        NameTarget::Range(range) => {
            if !sheet_positions.contains_key(&range.sheet) {
                return Err(PrepareError::UnresolvedSheet {
                    sheet: range.sheet.clone(),
                    origin: name.origin,
                });
            }
            super::fill::check_range_limit(range.range, limits.max_range_cells, name.origin)?;
        }
        NameTarget::TableColumn { table, header } => {
            let Some(sheet_position) = table_positions.get(table) else {
                return Err(PrepareError::UnresolvedTable {
                    table: table.clone(),
                    origin: name.origin,
                });
            };
            let resolved = sheets
                .get(*sheet_position)
                .and_then(|sheet| sheet.tables.get(table))
                .ok_or(PrepareError::UnresolvedTable {
                    table: table.clone(),
                    origin: name.origin,
                })?;
            if resolved.data_column(header).is_none() && !resolved.headers.contains_key(header) {
                return Err(PrepareError::UnresolvedTableHeader {
                    table: table.clone(),
                    header: header.clone(),
                    origin: name.origin,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use marksheet_model::{
        Block, Cell, Coordinate, Fill, FillTarget, FormulaSource, Name, NameId, NameTarget, Sheet,
        SheetId, SheetItem, SheetRange, Table, TableId, Value, Workbook,
    };

    use super::*;

    fn coordinate(value: &str) -> Coordinate {
        Coordinate::parse(value).unwrap()
    }

    fn id(value: &str) -> SheetId {
        SheetId::parse(value).unwrap()
    }

    fn block(anchor: &str, values: &[&[Value]]) -> Block {
        Block::new(
            coordinate(anchor),
            values
                .iter()
                .map(|row| row.iter().cloned().map(Cell::new).collect())
                .collect(),
        )
        .unwrap()
    }

    fn formula(value: &str) -> FormulaSource {
        FormulaSource::new(value).unwrap()
    }

    fn workbook(items: Vec<SheetItem>) -> Workbook {
        Workbook {
            sheets: vec![Sheet {
                id: id("main"),
                label: "Main".to_owned(),
                items,
                origin: None,
            }],
            ..Workbook::default()
        }
    }

    #[test]
    fn keeps_authored_blanks_distinct_from_absent_cells() {
        let prepared = PreparedWorkbook::build(
            &workbook(vec![SheetItem::Block(block(
                "B2",
                &[&[Value::Blank, Value::Text("x".to_owned())]],
            ))]),
            PrepareLimits::default(),
        )
        .unwrap();
        let sheet = &prepared.sheets[0];
        assert!(matches!(
            sheet.authored_cell(coordinate("B2")).unwrap().cell.value,
            Value::Blank
        ));
        assert!(sheet.authored_cell(coordinate("A1")).is_none());
    }

    #[test]
    fn rejects_manually_constructed_non_rectangular_blocks() {
        let malformed = Block {
            anchor: coordinate("A1"),
            cells: vec![vec![Cell::new(Value::Blank)], vec![]],
            origin: None,
        };
        let error = PreparedWorkbook::build(
            &workbook(vec![SheetItem::Block(malformed)]),
            PrepareLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error, PrepareError::MalformedBlock { .. }));
    }

    #[test]
    fn indexes_names_and_tables_in_stable_source_order() {
        let table = Table {
            id: TableId::parse("costs").unwrap(),
            block: block(
                "C4",
                &[
                    &[
                        Value::Text("Cost".to_owned()),
                        Value::Text("Qty".to_owned()),
                    ],
                    &[Value::Number(4.0), Value::Number(2.0)],
                ],
            ),
            origin: None,
        };
        let mut source = workbook(vec![
            SheetItem::Block(block("A1", &[&[Value::Text("first".to_owned())]])),
            SheetItem::Table(table),
        ]);
        source.names.push(Name {
            id: NameId::parse("cost_column").unwrap(),
            target: NameTarget::TableColumn {
                table: TableId::parse("costs").unwrap(),
                header: "Cost".to_owned(),
            },
            origin: None,
        });
        let prepared = PreparedWorkbook::build(&source, PrepareLimits::default()).unwrap();
        assert_eq!(prepared.sheets[0].footprints.len(), 2);
        assert_eq!(prepared.sheets[0].footprints[0].source_order, 0);
        assert_eq!(prepared.sheets[0].footprints[1].source_order, 1);
        assert!(
            prepared
                .names
                .contains_key(&NameId::parse("cost_column").unwrap())
        );
        assert_eq!(
            prepared
                .table(&TableId::parse("costs").unwrap())
                .unwrap()
                .data_column("Cost"),
            Some(Range::parse("C5:C5").unwrap())
        );
    }

    #[test]
    fn validates_named_ranges_against_known_sheets_and_limits() {
        let mut source = workbook(vec![]);
        source.names.push(Name {
            id: NameId::parse("too_large").unwrap(),
            target: NameTarget::Range(SheetRange {
                sheet: id("main"),
                range: Range::parse("A1:B2").unwrap(),
            }),
            origin: None,
        });
        let error = PreparedWorkbook::build(
            &source,
            PrepareLimits {
                max_range_cells: 3,
                ..PrepareLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, PrepareError::RangeLimitExceeded { .. }));
    }

    #[test]
    fn fill_does_not_rewrite_authored_blanks() {
        let prepared = PreparedWorkbook::build(
            &workbook(vec![
                SheetItem::Block(block("A1", &[&[Value::Blank, Value::Blank]])),
                SheetItem::Fill(Fill {
                    target: FillTarget::Range(Range::parse("A1:B1").unwrap()),
                    formula: formula("=1"),
                    origin: None,
                }),
            ]),
            PrepareLimits::default(),
        )
        .unwrap();
        let sheet = &prepared.sheets[0];
        assert_eq!(sheet.virtual_cells.len(), 2);
        assert!(matches!(
            sheet.authored_cell(coordinate("A1")).unwrap().cell.value,
            Value::Blank
        ));
        assert_eq!(
            sheet.virtual_cell(coordinate("B1")).unwrap().fill_anchor,
            coordinate("A1")
        );
    }

    #[test]
    fn rejects_fill_conflicts_and_overlap() {
        let nonblank = PreparedWorkbook::build(
            &workbook(vec![
                SheetItem::Block(block("A1", &[&[Value::Number(1.0)]])),
                SheetItem::Fill(Fill {
                    target: FillTarget::Range(Range::parse("A1").unwrap()),
                    formula: formula("=1"),
                    origin: None,
                }),
            ]),
            PrepareLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(
            nonblank,
            PrepareError::FillTargetsNonBlankCell { .. }
        ));

        let overlap = PreparedWorkbook::build(
            &workbook(vec![
                SheetItem::Block(block("A1", &[&[Value::Blank, Value::Blank]])),
                SheetItem::Fill(Fill {
                    target: FillTarget::Range(Range::parse("A1:B1").unwrap()),
                    formula: formula("=1"),
                    origin: None,
                }),
                SheetItem::Fill(Fill {
                    target: FillTarget::Range(Range::parse("B1").unwrap()),
                    formula: formula("=2"),
                    origin: None,
                }),
            ]),
            PrepareLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(overlap, PrepareError::OverlappingFills { .. }));
    }

    #[test]
    fn table_fill_uses_data_rows_and_exposes_current_row_context() {
        let table = Table {
            id: TableId::parse("costs").unwrap(),
            block: block(
                "A1",
                &[
                    &[
                        Value::Text("Cost".to_owned()),
                        Value::Text("Subtotal".to_owned()),
                    ],
                    &[Value::Number(2.0), Value::Blank],
                    &[Value::Number(3.0), Value::Blank],
                ],
            ),
            origin: None,
        };
        let prepared = PreparedWorkbook::build(
            &workbook(vec![
                SheetItem::Table(table),
                SheetItem::Fill(Fill {
                    target: FillTarget::TableColumn {
                        table: TableId::parse("costs").unwrap(),
                        header: "Subtotal".to_owned(),
                    },
                    formula: formula("=[@Cost]"),
                    origin: None,
                }),
            ]),
            PrepareLimits::default(),
        )
        .unwrap();
        let sheet = &prepared.sheets[0];
        assert!(sheet.virtual_cell(coordinate("B1")).is_none());
        assert_eq!(sheet.virtual_cells.len(), 2);
        assert_eq!(
            sheet.current_row_context(coordinate("B3")),
            Some(TableRowContext {
                table: TableId::parse("costs").unwrap(),
                data_row_index: 1,
            })
        );
    }

    #[test]
    fn coordinate_fill_inside_table_keeps_current_row_context() {
        let table = Table {
            id: TableId::parse("costs").unwrap(),
            block: block(
                "A1",
                &[
                    &[
                        Value::Text("Cost".to_owned()),
                        Value::Text("Subtotal".to_owned()),
                    ],
                    &[Value::Number(2.0), Value::Blank],
                    &[Value::Number(3.0), Value::Blank],
                ],
            ),
            origin: None,
        };
        let prepared = PreparedWorkbook::build(
            &workbook(vec![
                SheetItem::Table(table),
                SheetItem::Fill(Fill {
                    target: FillTarget::Range(Range::parse("B2:B3").unwrap()),
                    formula: formula("=[@Cost]"),
                    origin: None,
                }),
            ]),
            PrepareLimits::default(),
        )
        .unwrap();
        assert_eq!(
            prepared.sheets[0]
                .virtual_cell(coordinate("B3"))
                .unwrap()
                .table_row,
            Some(TableRowContext {
                table: TableId::parse("costs").unwrap(),
                data_row_index: 1,
            })
        );
    }

    #[test]
    fn rejects_header_only_table_fill_and_virtual_limit() {
        let header_only = Table {
            id: TableId::parse("costs").unwrap(),
            block: block("A1", &[&[Value::Text("Cost".to_owned())]]),
            origin: None,
        };
        let error = PreparedWorkbook::build(
            &workbook(vec![
                SheetItem::Table(header_only),
                SheetItem::Fill(Fill {
                    target: FillTarget::TableColumn {
                        table: TableId::parse("costs").unwrap(),
                        header: "Cost".to_owned(),
                    },
                    formula: formula("=1"),
                    origin: None,
                }),
            ]),
            PrepareLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error, PrepareError::HeaderOnlyTableFill { .. }));

        let error = PreparedWorkbook::build(
            &workbook(vec![
                SheetItem::Block(block("A1", &[&[Value::Blank, Value::Blank]])),
                SheetItem::Fill(Fill {
                    target: FillTarget::Range(Range::parse("A1:B1").unwrap()),
                    formula: formula("=1"),
                    origin: None,
                }),
            ]),
            PrepareLimits {
                max_virtual_cells: 1,
                ..PrepareLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PrepareError::VirtualCellLimitExceeded { .. }
        ));
    }
}
