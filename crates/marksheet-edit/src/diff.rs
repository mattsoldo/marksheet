//! Semantic workbook comparison independent of source spelling.
//!
//! This module deliberately compares the workbook projection rather than CST
//! nodes or source spans.  Consequently comments, whitespace, CSV quoting,
//! line endings, and equivalent formula spelling do not create changes.  It
//! also deliberately does *not* treat a `@block` boundary as an object: core
//! semantics assign values to sparse coordinates, and a valid document can
//! express the same cells with different block boundaries.  Tables remain
//! first-class because their identity, header row, and row order are semantic.

use std::collections::{BTreeMap, BTreeSet};

use marksheet_calc::formula::{ParseLimits, format_formula, parse};
use marksheet_model::{
    Apply, ApplyTarget, Cell, ColumnGeometry, ColumnRange, Coordinate, Extension, ExtensionId,
    Fill, FillTarget, FormulaSource, NameId, NameTarget, NumberFormat, Range, RowGeometry,
    RowRange, Sheet, SheetId, SheetItem, StyleId, StyleProperties, Table, TableId, TableRegion,
    Value, VerticalAlignment, Workbook, WorkbookSettings,
};
use time::{Date, OffsetDateTime};

/// Bound sparse boundary work before a pathological collection of tiny,
/// disjoint applications can turn a comparison into a dense calculation.
const MAX_STYLE_EFFECT_REGIONS: u128 = 20_000;
const MAX_STYLE_EFFECT_APPLICATIONS: u128 = 1_000_000;

/// A deterministic, source-independent diff between two workbook projections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticDiff {
    /// Changes in stable, deterministic order.
    pub changes: Vec<SemanticChange>,
}

impl SemanticDiff {
    /// Compares two already-valid workbook projections.
    #[must_use]
    pub fn between(left: &Workbook, right: &Workbook) -> Self {
        semantic_diff(left, right)
    }

    /// Returns whether the projections are core-semantically equivalent.
    ///
    /// An [`SemanticChange::UnsupportedComparison`] means the answer is
    /// false: callers must not claim equivalence if a formula could not be
    /// parsed under the portable profile or an input violates an assumed
    /// stable-identity invariant.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// A source-independent change to a workbook object.
#[derive(Clone, Debug, PartialEq)]
pub enum SemanticChange {
    SettingsChanged {
        before: WorkbookSettings,
        after: WorkbookSettings,
    },
    SheetOrderChanged {
        before: Vec<SheetId>,
        after: Vec<SheetId>,
    },
    SheetAdded {
        sheet: SheetId,
        label: String,
    },
    SheetRemoved {
        sheet: SheetId,
        label: String,
    },
    SheetLabelChanged {
        sheet: SheetId,
        before: String,
        after: String,
    },
    /// Sparse authored cell changes.  A missing value is distinct from an
    /// authored [`SemanticValue::Blank`].
    CellsChanged {
        sheet: SheetId,
        cells: Vec<CellChange>,
    },
    /// Ordered table, fill, apply, geometry, and sheet-extension definitions.
    /// Blocks are represented by [`Self::CellsChanged`] instead of source
    /// boundaries; see this module's documentation.
    SheetItemsChanged {
        sheet: SheetId,
        before: Vec<SemanticSheetItem>,
        after: Vec<SemanticSheetItem>,
    },
    /// The resolved sparse style effects for a sheet. Equivalent split and
    /// grouped directives, and reorderings of disjoint targets, normalize to
    /// the same projection. Precedence within overlapping targets is kept.
    StyleEffectsChanged {
        sheet: SheetId,
        before: Vec<SemanticStyleEffectComponent>,
        after: Vec<SemanticStyleEffectComponent>,
    },
    StyleAdded {
        style: SemanticStyle,
    },
    StyleRemoved {
        style: SemanticStyle,
    },
    StyleChanged {
        id: StyleId,
        before: SemanticStyleProperties,
        after: SemanticStyleProperties,
    },
    NameAdded {
        name: SemanticName,
    },
    NameRemoved {
        name: SemanticName,
    },
    NameChanged {
        id: NameId,
        before: NameTarget,
        after: NameTarget,
    },
    ExtensionDeclarationsChanged {
        before: Vec<SemanticExtensionDeclaration>,
        after: Vec<SemanticExtensionDeclaration>,
    },
    /// Opaque workbook-scoped extension instances.  Payload bytes and order
    /// are intentionally preserved in the comparison.
    WorkbookExtensionsChanged {
        before: Vec<SemanticWorkbookExtension>,
        after: Vec<SemanticWorkbookExtension>,
    },
    /// A comparison could not safely establish equivalence.  This is a diff
    /// result rather than a silent fallback to source text.
    UnsupportedComparison(UnsupportedComparison),
}

/// A changed sparse authored coordinate.
#[derive(Clone, Debug, PartialEq)]
pub struct CellChange {
    pub coordinate: Coordinate,
    pub before: Option<SemanticValue>,
    pub after: Option<SemanticValue>,
}

/// A value stripped of source origin and normalized for semantic comparison.
#[derive(Clone, Debug, PartialEq)]
pub enum SemanticValue {
    Blank,
    Text(String),
    Number(SemanticNumber),
    Boolean(bool),
    Date(Date),
    /// The stored offset is retained because it is part of Marksheet datetime
    /// equality, even for values that represent the same instant.
    DateTime(SemanticDateTime),
    /// A canonical `portable-a1@1` formula spelling, produced from its AST.
    Formula(String),
    Error(marksheet_model::CellError),
}

/// A binary64 value compared by IEEE-754 representation rather than host
/// numeric equality, so `-0` remains observably different from `0`.
#[derive(Clone, Copy, Debug)]
pub struct SemanticNumber {
    pub value: f64,
}

impl PartialEq for SemanticNumber {
    fn eq(&self, other: &Self) -> bool {
        self.value.to_bits() == other.value.to_bits()
    }
}

/// A datetime compared by both instant and stored offset.
///
/// `time::OffsetDateTime` compares instants by default. Marksheet deliberately
/// has stricter equality: `2026-01-01T00:00:00Z` and
/// `2025-12-31T19:00:00-05:00` are distinct authored values even though they
/// represent the same instant.
#[derive(Clone, Copy, Debug)]
pub struct SemanticDateTime {
    pub value: OffsetDateTime,
}

impl PartialEq for SemanticDateTime {
    fn eq(&self, other: &Self) -> bool {
        self.value.unix_timestamp_nanos() == other.value.unix_timestamp_nanos()
            && self.value.offset() == other.value.offset()
    }
}

/// A table definition, including its ordered rows and normalized cell values.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticTable {
    pub id: TableId,
    pub anchor: Coordinate,
    /// The table's ordered header row. Data values are represented solely by
    /// coordinate-level [`SemanticChange::CellsChanged`] entries.
    pub headers: Vec<SemanticValue>,
    /// Number of rows below the header. Row order is represented by their
    /// coordinates, and changing this count is a table-definition change.
    pub data_row_count: u64,
}

/// A resolved range-and-properties style effect.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticStyleEffect {
    pub range: Range,
    pub properties: SemanticStyleProperties,
}

/// A canonical sparse collection of resolved style rectangles. Effects are
/// sorted row-major and do not retain directive order.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticStyleEffectComponent {
    pub effects: Vec<SemanticStyleEffect>,
}

/// A formula fill with a canonical formula spelling.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticFill {
    pub target: FillTarget,
    pub formula: String,
}

/// A source-origin-free apply definition.  Style list order is preserved because
/// it determines precedence.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticApply {
    pub target: ApplyTarget,
    pub styles: Vec<StyleId>,
}

/// A source-origin-free column geometry definition.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticColumnGeometry {
    pub columns: ColumnRange,
    pub width: SemanticNumber,
}

/// A source-origin-free row geometry definition.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticRowGeometry {
    pub rows: RowRange,
    pub height: SemanticNumber,
}

/// A semantic item that remains meaningful after source origins are removed.
/// Its vector position is significant.
#[derive(Clone, Debug, PartialEq)]
pub enum SemanticSheetItem {
    Table(SemanticTable),
    Fill(SemanticFill),
    ColumnGeometry(SemanticColumnGeometry),
    RowGeometry(SemanticRowGeometry),
    Extension(SemanticSheetExtension),
}

/// An opaque sheet extension and its position among all source sheet items.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticSheetExtension {
    pub item_ordinal: usize,
    pub extension: SemanticExtension,
}

/// A source-origin-free style definition.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticStyle {
    pub id: StyleId,
    pub properties: SemanticStyleProperties,
}

/// Style properties with exact binary64 font-size comparison.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticStyleProperties {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub wrap: Option<bool>,
    pub text_color: Option<marksheet_model::Color>,
    pub fill: Option<marksheet_model::Color>,
    pub font_size: Option<SemanticNumber>,
    pub align: Option<marksheet_model::HorizontalAlignment>,
    pub valign: Option<VerticalAlignment>,
    pub number: Option<NumberFormat>,
    pub decimals: Option<u8>,
    pub currency: Option<String>,
}

impl SemanticStyleProperties {
    /// Applies only explicitly set properties, matching `@apply` cascade rules.
    fn apply(&mut self, next: &Self) {
        macro_rules! override_if_some {
            ($field:ident) => {
                if next.$field.is_some() {
                    self.$field = next.$field.clone();
                }
            };
        }
        override_if_some!(bold);
        override_if_some!(italic);
        override_if_some!(wrap);
        override_if_some!(text_color);
        override_if_some!(fill);
        override_if_some!(font_size);
        override_if_some!(align);
        override_if_some!(valign);
        override_if_some!(number);
        override_if_some!(decimals);
        override_if_some!(currency);
    }

    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// A source-origin-free named target.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticName {
    pub id: NameId,
    pub target: NameTarget,
}

/// An opaque extension declaration, excluding source span information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticExtensionDeclaration {
    pub capability: ExtensionId,
    pub required: bool,
    /// Cross-kind root declaration order, recovered from source origins.
    pub declaration_ordinal: usize,
}

/// An opaque extension instance, including payload bytes and placement order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticExtension {
    pub capability: ExtensionId,
    pub name: String,
    pub payload: String,
}

/// An opaque workbook extension and its order among root-level declarations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticWorkbookExtension {
    pub declaration_ordinal: usize,
    pub extension: SemanticExtension,
}

/// A stable location for a comparison that cannot be completed safely.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ComparisonScope {
    Workbook,
    Settings,
    Styles,
    Names,
    ExtensionDeclarations,
    WorkbookExtensions,
    Sheet(SheetId),
    Cell {
        sheet: SheetId,
        coordinate: Coordinate,
    },
    SheetItem {
        sheet: SheetId,
        index: usize,
    },
}

/// A reason a semantic comparison intentionally did not claim equivalence.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UnsupportedComparison {
    pub scope: ComparisonScope,
    pub explanation: String,
}

/// Compares two already-valid workbook projections.
///
/// The result is stable across source-only differences.  Formula values are
/// parsed and formatted through `portable-a1@1`; malformed formulas yield an
/// [`SemanticChange::UnsupportedComparison`] rather than being compared as
/// arbitrary text.
#[must_use]
pub fn semantic_diff(left: &Workbook, right: &Workbook) -> SemanticDiff {
    let mut unsupported = BTreeSet::new();
    let left_projection = WorkbookProjection::from_workbook(left, &mut unsupported);
    let right_projection = WorkbookProjection::from_workbook(right, &mut unsupported);
    let mut changes = Vec::new();

    if left_projection.settings != right_projection.settings {
        changes.push(SemanticChange::SettingsChanged {
            before: left_projection.settings,
            after: right_projection.settings,
        });
    }
    if left_projection.sheet_order != right_projection.sheet_order {
        changes.push(SemanticChange::SheetOrderChanged {
            before: left_projection.sheet_order,
            after: right_projection.sheet_order,
        });
    }

    diff_styles(
        &left_projection.styles,
        &right_projection.styles,
        &mut changes,
    );
    diff_names(
        &left_projection.names,
        &right_projection.names,
        &mut changes,
    );
    if left_projection.extension_declarations != right_projection.extension_declarations {
        changes.push(SemanticChange::ExtensionDeclarationsChanged {
            before: left_projection.extension_declarations,
            after: right_projection.extension_declarations,
        });
    }
    if left_projection.workbook_extensions != right_projection.workbook_extensions {
        changes.push(SemanticChange::WorkbookExtensionsChanged {
            before: left_projection.workbook_extensions,
            after: right_projection.workbook_extensions,
        });
    }

    diff_sheets(
        &left_projection.sheets,
        &right_projection.sheets,
        &mut changes,
    );

    changes.extend(
        unsupported
            .into_iter()
            .map(SemanticChange::UnsupportedComparison),
    );
    SemanticDiff { changes }
}

fn diff_sheets(
    left: &BTreeMap<SheetId, SheetProjection>,
    right: &BTreeMap<SheetId, SheetProjection>,
    changes: &mut Vec<SemanticChange>,
) {
    let sheet_ids: BTreeSet<_> = left.keys().chain(right.keys()).cloned().collect();
    for sheet_id in sheet_ids {
        match (left.get(&sheet_id), right.get(&sheet_id)) {
            (None, Some(after)) => changes.push(SemanticChange::SheetAdded {
                sheet: sheet_id.clone(),
                label: after.label.clone(),
            }),
            (Some(before), None) => changes.push(SemanticChange::SheetRemoved {
                sheet: sheet_id.clone(),
                label: before.label.clone(),
            }),
            (Some(before), Some(after)) => {
                if before.label != after.label {
                    changes.push(SemanticChange::SheetLabelChanged {
                        sheet: sheet_id.clone(),
                        before: before.label.clone(),
                        after: after.label.clone(),
                    });
                }
            }
            (None, None) => unreachable!("sheet ids originate from the maps"),
        }

        let before = left.get(&sheet_id);
        let after = right.get(&sheet_id);
        let before_cells = before.map_or_else(BTreeMap::new, |sheet| sheet.cells.clone());
        let after_cells = after.map_or_else(BTreeMap::new, |sheet| sheet.cells.clone());
        let cell_changes = diff_cells(&before_cells, &after_cells);
        if !cell_changes.is_empty() {
            changes.push(SemanticChange::CellsChanged {
                sheet: sheet_id.clone(),
                cells: cell_changes,
            });
        }

        let before_effects = before.map_or_else(Vec::new, |sheet| sheet.style_effects.clone());
        let after_effects = after.map_or_else(Vec::new, |sheet| sheet.style_effects.clone());
        if before_effects != after_effects {
            changes.push(SemanticChange::StyleEffectsChanged {
                sheet: sheet_id.clone(),
                before: before_effects,
                after: after_effects,
            });
        }

        let before_items = before.map_or_else(Vec::new, |sheet| sheet.items.clone());
        let after_items = after.map_or_else(Vec::new, |sheet| sheet.items.clone());
        if before_items != after_items {
            changes.push(SemanticChange::SheetItemsChanged {
                sheet: sheet_id,
                before: before_items,
                after: after_items,
            });
        }
    }
}

#[derive(Clone)]
struct WorkbookProjection {
    settings: WorkbookSettings,
    sheet_order: Vec<SheetId>,
    styles: BTreeMap<StyleId, SemanticStyleProperties>,
    names: BTreeMap<NameId, NameTarget>,
    extension_declarations: Vec<SemanticExtensionDeclaration>,
    workbook_extensions: Vec<SemanticWorkbookExtension>,
    sheets: BTreeMap<SheetId, SheetProjection>,
}

impl WorkbookProjection {
    fn from_workbook(
        workbook: &Workbook,
        unsupported: &mut BTreeSet<UnsupportedComparison>,
    ) -> Self {
        let mut styles = BTreeMap::new();
        for style in &workbook.styles {
            if styles
                .insert(
                    style.id.clone(),
                    semantic_style_properties(&style.properties),
                )
                .is_some()
            {
                unsupported.insert(UnsupportedComparison {
                    scope: ComparisonScope::Styles,
                    explanation: format!("duplicate style identifier {}", style.id),
                });
            }
        }
        let mut names = BTreeMap::new();
        for name in &workbook.names {
            if names.insert(name.id.clone(), name.target.clone()).is_some() {
                unsupported.insert(UnsupportedComparison {
                    scope: ComparisonScope::Names,
                    explanation: format!("duplicate name identifier {}", name.id),
                });
            }
        }
        let mut sheets = BTreeMap::new();
        for sheet in &workbook.sheets {
            if sheets
                .insert(
                    sheet.id.clone(),
                    SheetProjection::from_sheet(sheet, &styles, unsupported),
                )
                .is_some()
            {
                unsupported.insert(UnsupportedComparison {
                    scope: ComparisonScope::Workbook,
                    explanation: format!("duplicate sheet identifier {}", sheet.id),
                });
            }
        }
        let root_placement = root_extension_placements(workbook, unsupported);
        Self {
            settings: workbook.settings.clone(),
            sheet_order: workbook
                .sheets
                .iter()
                .map(|sheet| sheet.id.clone())
                .collect(),
            styles,
            names,
            extension_declarations: semantic_extension_declarations(workbook, &root_placement),
            workbook_extensions: semantic_workbook_extensions(workbook, &root_placement),
            sheets,
        }
    }
}

#[derive(Clone)]
struct SheetProjection {
    label: String,
    cells: BTreeMap<Coordinate, SemanticValue>,
    items: Vec<SemanticSheetItem>,
    style_effects: Vec<SemanticStyleEffectComponent>,
}

#[derive(Clone)]
struct TableStyleTarget {
    anchor: Coordinate,
    width: u64,
    data_rows: u64,
    headers: Vec<String>,
}

impl SheetProjection {
    fn from_sheet(
        sheet: &Sheet,
        styles: &BTreeMap<StyleId, SemanticStyleProperties>,
        unsupported: &mut BTreeSet<UnsupportedComparison>,
    ) -> Self {
        let mut cells = BTreeMap::new();
        let mut items = Vec::new();
        let mut table_targets = BTreeMap::new();
        let mut applies = Vec::new();
        for (index, item) in sheet.items.iter().enumerate() {
            let scope = ComparisonScope::SheetItem {
                sheet: sheet.id.clone(),
                index,
            };
            match item {
                SheetItem::Block(block) => {
                    insert_cells(
                        &mut cells,
                        &block.cells,
                        block.anchor,
                        &sheet.id,
                        unsupported,
                    );
                }
                SheetItem::Table(table) => {
                    insert_cells(
                        &mut cells,
                        &table.block.cells,
                        table.block.anchor,
                        &sheet.id,
                        unsupported,
                    );
                    items.push(SemanticSheetItem::Table(semantic_table(
                        table,
                        &sheet.id,
                        unsupported,
                    )));
                    insert_table_style_target(table, &mut table_targets, &sheet.id, unsupported);
                }
                SheetItem::Fill(fill) => items.push(SemanticSheetItem::Fill(semantic_fill(
                    fill,
                    scope,
                    unsupported,
                ))),
                SheetItem::Apply(apply) => applies.push((index, apply)),
                SheetItem::ColumnGeometry(geometry) => items.push(
                    SemanticSheetItem::ColumnGeometry(semantic_column_geometry(geometry)),
                ),
                SheetItem::RowGeometry(geometry) => items.push(SemanticSheetItem::RowGeometry(
                    semantic_row_geometry(geometry),
                )),
                SheetItem::Extension(extension) => {
                    items.push(SemanticSheetItem::Extension(SemanticSheetExtension {
                        item_ordinal: index,
                        extension: semantic_extension(extension),
                    }));
                }
            }
        }
        Self {
            label: sheet.label.clone(),
            cells,
            items,
            style_effects: resolve_style_effects(
                &sheet.id,
                &applies,
                styles,
                &table_targets,
                unsupported,
            ),
        }
    }
}

fn insert_cells(
    target: &mut BTreeMap<Coordinate, SemanticValue>,
    rows: &[Vec<Cell>],
    anchor: Coordinate,
    sheet: &SheetId,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) {
    for (row_offset, row) in rows.iter().enumerate() {
        for (column_offset, cell) in row.iter().enumerate() {
            let (Ok(column_offset), Ok(row_offset)) =
                (u64::try_from(column_offset), u64::try_from(row_offset))
            else {
                unsupported.insert(UnsupportedComparison {
                    scope: ComparisonScope::Sheet(sheet.clone()),
                    explanation: "cell offset cannot be represented as u64".to_owned(),
                });
                continue;
            };
            let Ok(coordinate) = anchor.offset(column_offset, row_offset) else {
                unsupported.insert(UnsupportedComparison {
                    scope: ComparisonScope::Sheet(sheet.clone()),
                    explanation: "cell coordinate overflows workbook bounds".to_owned(),
                });
                continue;
            };
            let scope = ComparisonScope::Cell {
                sheet: sheet.clone(),
                coordinate,
            };
            let value = semantic_value(&cell.value, scope.clone(), unsupported);
            if target.insert(coordinate, value).is_some() {
                unsupported.insert(UnsupportedComparison {
                    scope,
                    explanation: "multiple authored cells occupy the same coordinate".to_owned(),
                });
            }
        }
    }
}

fn insert_table_style_target(
    table: &Table,
    targets: &mut BTreeMap<TableId, TableStyleTarget>,
    sheet: &SheetId,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) {
    let Some(header_row) = table.block.cells.first() else {
        unsupported.insert(UnsupportedComparison {
            scope: ComparisonScope::Sheet(sheet.clone()),
            explanation: format!("table {} has no header row", table.id),
        });
        return;
    };
    let mut headers = Vec::with_capacity(header_row.len());
    for cell in header_row {
        let Value::Text(header) = &cell.value else {
            unsupported.insert(UnsupportedComparison {
                scope: ComparisonScope::Sheet(sheet.clone()),
                explanation: format!("table {} has a non-text header", table.id),
            });
            return;
        };
        headers.push(header.clone());
    }
    let Ok(width) = u64::try_from(header_row.len()) else {
        unsupported.insert(UnsupportedComparison {
            scope: ComparisonScope::Sheet(sheet.clone()),
            explanation: format!("table {} width cannot be represented", table.id),
        });
        return;
    };
    let Ok(data_rows) = u64::try_from(table.block.cells.len().saturating_sub(1)) else {
        unsupported.insert(UnsupportedComparison {
            scope: ComparisonScope::Sheet(sheet.clone()),
            explanation: format!("table {} height cannot be represented", table.id),
        });
        return;
    };
    if targets
        .insert(
            table.id.clone(),
            TableStyleTarget {
                anchor: table.block.anchor,
                width,
                data_rows,
                headers,
            },
        )
        .is_some()
    {
        unsupported.insert(UnsupportedComparison {
            scope: ComparisonScope::Sheet(sheet.clone()),
            explanation: format!("duplicate table identifier {}", table.id),
        });
    }
}

fn resolve_style_effects(
    sheet: &SheetId,
    applies: &[(usize, &Apply)],
    styles: &BTreeMap<StyleId, SemanticStyleProperties>,
    tables: &BTreeMap<TableId, TableStyleTarget>,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> Vec<SemanticStyleEffectComponent> {
    let mut effects = Vec::new();
    for (item_index, apply) in applies {
        let scope = ComparisonScope::SheetItem {
            sheet: sheet.clone(),
            index: *item_index,
        };
        let Some(range) = resolve_apply_range(&apply.target, tables, scope.clone(), unsupported)
        else {
            continue;
        };
        let Some(properties) = merge_style_ids(&apply.styles, styles, scope, unsupported) else {
            continue;
        };
        effects.push(SemanticStyleEffect { range, properties });
    }
    resolved_style_effects(&effects, sheet, unsupported)
}

fn resolve_apply_range(
    target: &ApplyTarget,
    tables: &BTreeMap<TableId, TableStyleTarget>,
    scope: ComparisonScope,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> Option<Range> {
    match target {
        ApplyTarget::Range(range) => Some(*range),
        ApplyTarget::Table { table, region } => {
            let Some(table_target) = tables.get(table) else {
                unsupported.insert(UnsupportedComparison {
                    scope,
                    explanation: format!("table application target {table} cannot be resolved"),
                });
                return None;
            };
            let Some(last_column_offset) = table_target.width.checked_sub(1) else {
                unsupported.insert(UnsupportedComparison {
                    scope,
                    explanation: format!("table {table} has no columns"),
                });
                return None;
            };
            let range = match region {
                TableRegion::Headers => {
                    range_from_offsets(table_target.anchor, 0, 0, last_column_offset, 0)
                }
                TableRegion::Data => {
                    if table_target.data_rows == 0 {
                        return None;
                    }
                    range_from_offsets(
                        table_target.anchor,
                        0,
                        1,
                        last_column_offset,
                        table_target.data_rows,
                    )
                }
                TableRegion::Column { header } => {
                    if table_target.data_rows == 0 {
                        return None;
                    }
                    let Some(column) = table_target
                        .headers
                        .iter()
                        .position(|value| value == header)
                    else {
                        unsupported.insert(UnsupportedComparison {
                            scope,
                            explanation: format!(
                                "table application column {header:?} cannot be resolved in {table}"
                            ),
                        });
                        return None;
                    };
                    let Ok(column) = u64::try_from(column) else {
                        unsupported.insert(UnsupportedComparison {
                            scope,
                            explanation: format!("table {table} column cannot be represented"),
                        });
                        return None;
                    };
                    range_from_offsets(
                        table_target.anchor,
                        column,
                        1,
                        column,
                        table_target.data_rows,
                    )
                }
            };
            if let Some(range) = range {
                Some(range)
            } else {
                unsupported.insert(UnsupportedComparison {
                    scope,
                    explanation: format!(
                        "table application target {table} overflows workbook bounds"
                    ),
                });
                None
            }
        }
    }
}

fn range_from_offsets(
    anchor: Coordinate,
    start_column: u64,
    start_row: u64,
    end_column: u64,
    end_row: u64,
) -> Option<Range> {
    Some(Range::new(
        anchor.offset(start_column, start_row).ok()?,
        anchor.offset(end_column, end_row).ok()?,
    ))
}

fn merge_style_ids(
    style_ids: &[StyleId],
    styles: &BTreeMap<StyleId, SemanticStyleProperties>,
    scope: ComparisonScope,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> Option<SemanticStyleProperties> {
    let mut merged = SemanticStyleProperties::default();
    for style_id in style_ids {
        let Some(properties) = styles.get(style_id) else {
            unsupported.insert(UnsupportedComparison {
                scope,
                explanation: format!("style application references missing style {style_id}"),
            });
            return None;
        };
        merged.apply(properties);
    }
    Some(merged)
}

fn resolved_style_effects(
    effects: &[SemanticStyleEffect],
    sheet: &SheetId,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> Vec<SemanticStyleEffectComponent> {
    if effects.is_empty() {
        return Vec::new();
    }

    // This partitions only directive boundaries, never every coordinate in a
    // range. `u128` admits the exclusive endpoint one past `u64::MAX`.
    let mut columns = BTreeSet::new();
    let mut rows = BTreeSet::new();
    for effect in effects {
        columns.insert(u128::from(effect.range.start.column));
        columns.insert(u128::from(effect.range.end.column) + 1);
        rows.insert(u128::from(effect.range.start.row));
        rows.insert(u128::from(effect.range.end.row) + 1);
    }
    let columns: Vec<_> = columns.into_iter().collect();
    let rows: Vec<_> = rows.into_iter().collect();
    let region_count = u128::try_from(columns.len().saturating_sub(1))
        .unwrap_or(u128::MAX)
        .saturating_mul(u128::try_from(rows.len().saturating_sub(1)).unwrap_or(u128::MAX));
    let application_count =
        region_count.saturating_mul(u128::try_from(effects.len()).unwrap_or(u128::MAX));
    if region_count > MAX_STYLE_EFFECT_REGIONS || application_count > MAX_STYLE_EFFECT_APPLICATIONS
    {
        unsupported.insert(UnsupportedComparison {
            scope: ComparisonScope::Sheet(sheet.clone()),
            explanation: format!(
                "style-effect comparison exceeds bounded work ({region_count} regions, {application_count} applications)"
            ),
        });
        return Vec::new();
    }
    let mut resolved = Vec::new();
    for row_window in rows.windows(2) {
        for column_window in columns.windows(2) {
            let coordinate = Coordinate {
                column: u64::try_from(column_window[0]).expect("style boundary is a u64 start"),
                row: u64::try_from(row_window[0]).expect("style boundary is a u64 start"),
            };
            let mut properties = SemanticStyleProperties::default();
            for effect in effects {
                if effect.range.contains(coordinate) {
                    properties.apply(&effect.properties);
                }
            }
            if properties.is_empty() {
                continue;
            }
            let end = Coordinate {
                column: u64::try_from(column_window[1] - 1)
                    .expect("style boundary end is within u64"),
                row: u64::try_from(row_window[1] - 1).expect("style boundary end is within u64"),
            };
            resolved.push(SemanticStyleEffect {
                range: Range::new(coordinate, end),
                properties,
            });
        }
    }
    coalesce_style_rectangles(&mut resolved);
    (!resolved.is_empty())
        .then_some(SemanticStyleEffectComponent { effects: resolved })
        .into_iter()
        .collect()
}

fn coalesce_style_rectangles(effects: &mut Vec<SemanticStyleEffect>) {
    effects.sort_by_key(|effect| (effect.range.start.row, effect.range.start.column));
    let mut horizontal: Vec<SemanticStyleEffect> = Vec::new();
    for effect in effects.drain(..) {
        if let Some(previous) = horizontal.last_mut() {
            if previous.properties == effect.properties
                && previous.range.start.row == effect.range.start.row
                && previous.range.end.row == effect.range.end.row
                && u128::from(previous.range.end.column) + 1
                    == u128::from(effect.range.start.column)
            {
                previous.range.end.column = effect.range.end.column;
                continue;
            }
        }
        horizontal.push(effect);
    }

    let mut active: BTreeMap<(u64, u64, String), usize> = BTreeMap::new();
    let mut vertical: Vec<SemanticStyleEffect> = Vec::new();
    for effect in horizontal {
        let key = (
            effect.range.start.column,
            effect.range.end.column,
            format!("{:?}", effect.properties),
        );
        if let Some(previous_index) = active.get(&key).copied() {
            if u128::from(vertical[previous_index].range.end.row) + 1
                == u128::from(effect.range.start.row)
            {
                vertical[previous_index].range.end.row = effect.range.end.row;
                continue;
            }
        }
        active.insert(key, vertical.len());
        vertical.push(effect);
    }
    vertical.sort_by_key(|effect| (effect.range.start.row, effect.range.start.column));
    *effects = vertical;
}

fn semantic_table(
    table: &Table,
    sheet: &SheetId,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> SemanticTable {
    let mut headers = Vec::new();
    if let Some(row) = table.block.cells.first() {
        for (column_offset, cell) in row.iter().enumerate() {
            let coordinate = if let Some(coordinate) = u64::try_from(column_offset)
                .ok()
                .and_then(|offset| table.block.anchor.offset(offset, 0).ok())
            {
                coordinate
            } else {
                unsupported.insert(UnsupportedComparison {
                    scope: ComparisonScope::Sheet(sheet.clone()),
                    explanation: "table header coordinate overflows workbook bounds".to_owned(),
                });
                table.block.anchor
            };
            headers.push(semantic_value(
                &cell.value,
                ComparisonScope::Cell {
                    sheet: sheet.clone(),
                    coordinate,
                },
                unsupported,
            ));
        }
    }
    SemanticTable {
        id: table.id.clone(),
        anchor: table.block.anchor,
        headers,
        data_row_count: u64::try_from(table.block.cells.len().saturating_sub(1))
            .unwrap_or(u64::MAX),
    }
}

fn semantic_fill(
    fill: &Fill,
    scope: ComparisonScope,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> SemanticFill {
    SemanticFill {
        target: fill.target.clone(),
        formula: canonical_formula(&fill.formula, scope, unsupported),
    }
}

fn semantic_column_geometry(geometry: &ColumnGeometry) -> SemanticColumnGeometry {
    SemanticColumnGeometry {
        columns: geometry.columns,
        width: SemanticNumber {
            value: geometry.width,
        },
    }
}

fn semantic_row_geometry(geometry: &RowGeometry) -> SemanticRowGeometry {
    SemanticRowGeometry {
        rows: geometry.rows,
        height: SemanticNumber {
            value: geometry.height,
        },
    }
}

fn semantic_style_properties(properties: &StyleProperties) -> SemanticStyleProperties {
    SemanticStyleProperties {
        bold: properties.bold,
        italic: properties.italic,
        wrap: properties.wrap,
        text_color: properties.text_color.clone(),
        fill: properties.fill.clone(),
        font_size: properties.font_size.map(|value| SemanticNumber { value }),
        align: properties.align,
        valign: properties.valign,
        number: properties.number,
        decimals: properties.decimals,
        currency: properties.currency.clone(),
    }
}

#[derive(Clone, Copy)]
enum RootDeclaration {
    Other,
    ExtensionDeclaration(usize),
    ExtensionInstance(usize),
}

struct RootExtensionPlacements {
    declarations: Vec<usize>,
    instances: Vec<usize>,
}

fn root_extension_placements(
    workbook: &Workbook,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> RootExtensionPlacements {
    let mut roots = Vec::new();
    let mut incomplete = false;
    if let Some(origin) = workbook.book_origin {
        roots.push((origin.span.start, RootDeclaration::Other));
    }
    for style in &workbook.styles {
        match style.origin {
            Some(origin) => roots.push((origin.span.start, RootDeclaration::Other)),
            None => incomplete = true,
        }
    }
    for name in &workbook.names {
        match name.origin {
            Some(origin) => roots.push((origin.span.start, RootDeclaration::Other)),
            None => incomplete = true,
        }
    }
    for (index, declaration) in workbook.extensions.iter().enumerate() {
        match declaration.origin {
            Some(origin) => roots.push((
                origin.span.start,
                RootDeclaration::ExtensionDeclaration(index),
            )),
            None => incomplete = true,
        }
    }
    for (index, extension) in workbook.extension_instances.iter().enumerate() {
        match extension.origin {
            Some(origin) => {
                roots.push((origin.span.start, RootDeclaration::ExtensionInstance(index)));
            }
            None => incomplete = true,
        }
    }
    for sheet in &workbook.sheets {
        match sheet.origin {
            Some(origin) => roots.push((origin.span.start, RootDeclaration::Other)),
            None => incomplete = true,
        }
    }

    if incomplete {
        if !workbook.extensions.is_empty() {
            unsupported.insert(UnsupportedComparison {
                scope: ComparisonScope::ExtensionDeclarations,
                explanation: "root extension declaration placement cannot be established without source origins".to_owned(),
            });
        }
        if !workbook.extension_instances.is_empty() {
            unsupported.insert(UnsupportedComparison {
                scope: ComparisonScope::WorkbookExtensions,
                explanation:
                    "root extension placement cannot be established without source origins"
                        .to_owned(),
            });
        }
        return RootExtensionPlacements {
            declarations: (0..workbook.extensions.len()).collect(),
            instances: (0..workbook.extension_instances.len()).collect(),
        };
    }

    roots.sort_unstable_by_key(|(start, _)| *start);
    if roots.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        if !workbook.extensions.is_empty() {
            unsupported.insert(UnsupportedComparison {
                scope: ComparisonScope::ExtensionDeclarations,
                explanation: "root extension declaration placement is ambiguous because source origins overlap".to_owned(),
            });
        }
        if !workbook.extension_instances.is_empty() {
            unsupported.insert(UnsupportedComparison {
                scope: ComparisonScope::WorkbookExtensions,
                explanation: "root extension placement is ambiguous because source origins overlap"
                    .to_owned(),
            });
        }
    }
    let mut declarations = vec![0; workbook.extensions.len()];
    let mut instances = vec![0; workbook.extension_instances.len()];
    for (ordinal, (_, declaration)) in roots.into_iter().enumerate() {
        match declaration {
            RootDeclaration::Other => {}
            RootDeclaration::ExtensionDeclaration(index) => declarations[index] = ordinal,
            RootDeclaration::ExtensionInstance(index) => instances[index] = ordinal,
        }
    }
    RootExtensionPlacements {
        declarations,
        instances,
    }
}

fn semantic_extension_declarations(
    workbook: &Workbook,
    placements: &RootExtensionPlacements,
) -> Vec<SemanticExtensionDeclaration> {
    workbook
        .extensions
        .iter()
        .enumerate()
        .map(|(index, declaration)| SemanticExtensionDeclaration {
            capability: declaration.capability.clone(),
            required: declaration.required,
            declaration_ordinal: placements.declarations[index],
        })
        .collect()
}

fn semantic_extension(extension: &Extension) -> SemanticExtension {
    SemanticExtension {
        capability: extension.capability.clone(),
        name: extension.name.clone(),
        payload: extension.payload.clone(),
    }
}

fn semantic_workbook_extensions(
    workbook: &Workbook,
    placements: &RootExtensionPlacements,
) -> Vec<SemanticWorkbookExtension> {
    workbook
        .extension_instances
        .iter()
        .enumerate()
        .map(|(index, extension)| SemanticWorkbookExtension {
            declaration_ordinal: placements.instances[index],
            extension: semantic_extension(extension),
        })
        .collect()
}

fn semantic_value(
    value: &Value,
    scope: ComparisonScope,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> SemanticValue {
    match value {
        Value::Blank => SemanticValue::Blank,
        Value::Text(value) => SemanticValue::Text(value.clone()),
        Value::Number(value) => SemanticValue::Number(SemanticNumber { value: *value }),
        Value::Boolean(value) => SemanticValue::Boolean(*value),
        Value::Date(value) => SemanticValue::Date(*value),
        Value::DateTime(value) => SemanticValue::DateTime(SemanticDateTime { value: *value }),
        Value::Formula(formula) => {
            SemanticValue::Formula(canonical_formula(formula, scope, unsupported))
        }
        Value::Error(value) => SemanticValue::Error(*value),
    }
}

fn canonical_formula(
    formula: &FormulaSource,
    scope: ComparisonScope,
    unsupported: &mut BTreeSet<UnsupportedComparison>,
) -> String {
    match parse(formula.as_str(), &ParseLimits::default()) {
        Ok(parsed) => match format_formula(&parsed) {
            Ok(canonical) => canonical,
            Err(error) => {
                unsupported.insert(UnsupportedComparison {
                    scope,
                    explanation: format!(
                        "formula cannot be normalized with portable-a1@1: {error}"
                    ),
                });
                formula.as_str().to_owned()
            }
        },
        Err(error) => {
            unsupported.insert(UnsupportedComparison {
                scope,
                explanation: format!(
                    "formula cannot be normalized with portable-a1@1: {}",
                    error.message
                ),
            });
            // Keep the source as a diagnostic aid, but unsupported comparison
            // makes it impossible for this fallback to claim equivalence.
            formula.as_str().to_owned()
        }
    }
}

fn diff_cells(
    before: &BTreeMap<Coordinate, SemanticValue>,
    after: &BTreeMap<Coordinate, SemanticValue>,
) -> Vec<CellChange> {
    before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|coordinate| {
            let before_value = before.get(&coordinate).cloned();
            let after_value = after.get(&coordinate).cloned();
            (before_value != after_value).then_some(CellChange {
                coordinate,
                before: before_value,
                after: after_value,
            })
        })
        .collect()
}

fn diff_styles(
    before: &BTreeMap<StyleId, SemanticStyleProperties>,
    after: &BTreeMap<StyleId, SemanticStyleProperties>,
    changes: &mut Vec<SemanticChange>,
) {
    let ids: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
    for id in ids {
        match (before.get(&id), after.get(&id)) {
            (None, Some(properties)) => changes.push(SemanticChange::StyleAdded {
                style: SemanticStyle {
                    id,
                    properties: properties.clone(),
                },
            }),
            (Some(properties), None) => changes.push(SemanticChange::StyleRemoved {
                style: SemanticStyle {
                    id,
                    properties: properties.clone(),
                },
            }),
            (Some(before_properties), Some(after_properties))
                if before_properties != after_properties =>
            {
                changes.push(SemanticChange::StyleChanged {
                    id,
                    before: before_properties.clone(),
                    after: after_properties.clone(),
                });
            }
            (Some(_), Some(_)) | (None, None) => {}
        }
    }
}

fn diff_names(
    before: &BTreeMap<NameId, NameTarget>,
    after: &BTreeMap<NameId, NameTarget>,
    changes: &mut Vec<SemanticChange>,
) {
    let ids: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
    for id in ids {
        match (before.get(&id), after.get(&id)) {
            (None, Some(target)) => changes.push(SemanticChange::NameAdded {
                name: SemanticName {
                    id,
                    target: target.clone(),
                },
            }),
            (Some(target), None) => changes.push(SemanticChange::NameRemoved {
                name: SemanticName {
                    id,
                    target: target.clone(),
                },
            }),
            (Some(before_target), Some(after_target)) if before_target != after_target => {
                changes.push(SemanticChange::NameChanged {
                    id,
                    before: before_target.clone(),
                    after: after_target.clone(),
                });
            }
            (Some(_), Some(_)) | (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::{
        Apply, Block, Cell, Color, ExtensionDeclaration, Footprint, HorizontalAlignment, Origin,
        Range, Style, Table,
    };
    use time::UtcOffset;

    fn id<T: std::str::FromStr>(value: &str) -> T
    where
        T::Err: std::fmt::Debug,
    {
        value.parse().expect("valid identifier")
    }

    fn coordinate(value: &str) -> Coordinate {
        value.parse().expect("valid coordinate")
    }

    fn formula(value: &str) -> Value {
        Value::Formula(FormulaSource::new(value).expect("formula marker"))
    }

    fn block(anchor: &str, rows: Vec<Vec<Value>>) -> Block {
        Block::new(
            coordinate(anchor),
            rows.into_iter()
                .map(|row| row.into_iter().map(Cell::new).collect())
                .collect(),
        )
        .expect("rectangular block")
    }

    fn workbook(items: Vec<SheetItem>) -> Workbook {
        Workbook {
            sheets: vec![Sheet {
                id: id("sheet"),
                label: "Sheet".to_owned(),
                items,
                origin: None,
            }],
            ..Workbook::default()
        }
    }

    #[test]
    fn source_only_formula_spelling_and_origins_are_equivalent() {
        let mut left = workbook(vec![SheetItem::Block(block(
            "A1",
            vec![vec![formula("= sum ( 1.0, a1 ) ")]],
        ))]);
        let mut right = workbook(vec![SheetItem::Block(block(
            "A1",
            vec![vec![formula("=SUM(1,A1)")]],
        ))]);
        left.origin = Some(marksheet_model::Origin {
            span: marksheet_model::ByteSpan::empty(1),
        });
        right.sheets[0].origin = Some(marksheet_model::Origin {
            span: marksheet_model::ByteSpan::empty(99),
        });
        assert!(semantic_diff(&left, &right).is_empty());
    }

    #[test]
    fn reports_sparse_cell_changes_and_preserves_absence_vs_blank() {
        let left = workbook(vec![SheetItem::Block(block(
            "ZZ1000000",
            vec![vec![Value::Blank]],
        ))]);
        let right = workbook(vec![SheetItem::Block(block(
            "ZZ1000000",
            vec![vec![Value::Text("present".to_owned())]],
        ))]);
        let diff = semantic_diff(&left, &right);
        assert_eq!(diff.changes.len(), 1);
        let SemanticChange::CellsChanged { cells, .. } = &diff.changes[0] else {
            panic!("expected cell diff");
        };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].coordinate, coordinate("ZZ1000000"));
        assert_eq!(cells[0].before, Some(SemanticValue::Blank));
        assert_eq!(
            cells[0].after,
            Some(SemanticValue::Text("present".to_owned()))
        );
    }

    #[test]
    fn datetime_comparison_includes_the_stored_offset() {
        let instant = OffsetDateTime::from_unix_timestamp(0).expect("valid instant");
        let same_instant_different_offset =
            instant.to_offset(UtcOffset::from_hms(-5, 0, 0).expect("valid offset"));
        assert_eq!(
            instant, same_instant_different_offset,
            "time compares instants"
        );

        let left = workbook(vec![SheetItem::Block(block(
            "A1",
            vec![vec![Value::DateTime(instant)]],
        ))]);
        let right = workbook(vec![SheetItem::Block(block(
            "A1",
            vec![vec![Value::DateTime(same_instant_different_offset)]],
        ))]);
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::CellsChanged { .. }))
        );
    }

    #[test]
    fn sheet_label_and_id_have_distinct_changes() {
        let left = workbook(Vec::new());
        let mut renamed_label = left.clone();
        renamed_label.sheets[0].label = "Renamed".to_owned();
        assert!(matches!(
            semantic_diff(&left, &renamed_label).changes.as_slice(),
            [SemanticChange::SheetLabelChanged { .. }]
        ));

        let mut renamed_id = left;
        renamed_id.sheets[0].id = id("renamed");
        let changes = semantic_diff(&workbook(Vec::new()), &renamed_id).changes;
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, SemanticChange::SheetAdded { .. }))
        );
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, SemanticChange::SheetRemoved { .. }))
        );
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, SemanticChange::SheetOrderChanged { .. }))
        );
    }

    #[test]
    fn table_fill_style_and_extension_changes_are_visible() {
        let table = Table {
            id: id("costs"),
            block: block(
                "A1",
                vec![
                    vec![
                        Value::Text("Item".to_owned()),
                        Value::Text("Cost".to_owned()),
                    ],
                    vec![Value::Text("Rent".to_owned()), Value::Number(10.0)],
                ],
            ),
            origin: None,
        };
        let fill = Fill {
            target: FillTarget::Range(Range::parse("C2:C3").expect("range")),
            formula: FormulaSource::new("=A2").expect("formula"),
            origin: None,
        };
        let mut left = workbook(vec![SheetItem::Table(table), SheetItem::Fill(fill)]);
        left.styles.push(Style {
            id: id("money"),
            properties: StyleProperties {
                font_size: Some(12.0),
                align: Some(HorizontalAlignment::Right),
                ..StyleProperties::default()
            },
            origin: None,
        });
        left.extension_instances.push(Extension {
            capability: ExtensionId::parse("charts@1").expect("capability"),
            name: "chart".to_owned(),
            payload: "{\"type\":\"bar\"}".to_owned(),
            origin: None,
            payload_origin: None,
        });
        let mut right = left.clone();
        let SheetItem::Table(table) = &mut right.sheets[0].items[0] else {
            panic!("table item");
        };
        table.block.cells[1][1] = Cell::new(Value::Number(20.0));
        let SheetItem::Fill(fill) = &mut right.sheets[0].items[1] else {
            panic!("fill item");
        };
        fill.formula = FormulaSource::new("=A2*2").expect("formula");
        right.styles[0].properties.font_size = Some(13.0);
        right.extension_instances[0].payload = "{\"type\":\"line\"}".to_owned();

        let changes = semantic_diff(&left, &right).changes;
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, SemanticChange::CellsChanged { .. }))
        );
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, SemanticChange::SheetItemsChanged { .. }))
        );
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, SemanticChange::StyleChanged { .. }))
        );
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, SemanticChange::WorkbookExtensionsChanged { .. }))
        );
    }

    fn style(id_value: &str, properties: StyleProperties) -> Style {
        Style {
            id: id(id_value),
            properties,
            origin: None,
        }
    }

    fn apply(range: &str, styles: &[&str]) -> SheetItem {
        SheetItem::Apply(Apply {
            target: ApplyTarget::Range(Range::parse(range).expect("range")),
            styles: styles.iter().map(|style| id(style)).collect(),
            origin: None,
        })
    }

    #[test]
    fn split_and_grouped_styles_on_one_target_are_equivalent() {
        let mut left = workbook(vec![apply("A1:B2", &["bold", "wrap"])]);
        left.styles = vec![
            style(
                "bold",
                StyleProperties {
                    bold: Some(true),
                    ..StyleProperties::default()
                },
            ),
            style(
                "wrap",
                StyleProperties {
                    wrap: Some(true),
                    ..StyleProperties::default()
                },
            ),
        ];
        let mut right = left.clone();
        right.sheets[0].items = vec![apply("A1:B2", &["bold"]), apply("A1:B2", &["wrap"])];
        assert!(semantic_diff(&left, &right).is_empty());
    }

    #[test]
    fn equivalent_style_coverage_is_coalesced_across_directive_boundaries() {
        let mut left = workbook(vec![apply("A1:B1", &["bold"])]);
        left.styles.push(style(
            "bold",
            StyleProperties {
                bold: Some(true),
                ..StyleProperties::default()
            },
        ));
        let mut right = left.clone();
        right.sheets[0].items = vec![apply("A1", &["bold"]), apply("B1", &["bold"])];
        assert!(semantic_diff(&left, &right).is_empty());
    }

    #[test]
    fn disjoint_style_effects_are_order_independent() {
        let mut left = workbook(vec![apply("A1", &["bold"]), apply("C1", &["wrap"])]);
        left.styles = vec![
            style(
                "bold",
                StyleProperties {
                    bold: Some(true),
                    ..StyleProperties::default()
                },
            ),
            style(
                "wrap",
                StyleProperties {
                    wrap: Some(true),
                    ..StyleProperties::default()
                },
            ),
        ];
        let mut right = left.clone();
        right.sheets[0].items.reverse();
        assert!(semantic_diff(&left, &right).is_empty());
    }

    #[test]
    fn overlapping_style_precedence_remains_observable() {
        let mut left = workbook(vec![apply("A1:B1", &["on"]), apply("B1:C1", &["off"])]);
        left.styles = vec![
            style(
                "on",
                StyleProperties {
                    bold: Some(true),
                    ..StyleProperties::default()
                },
            ),
            style(
                "off",
                StyleProperties {
                    bold: Some(false),
                    ..StyleProperties::default()
                },
            ),
        ];
        let mut right = left.clone();
        right.sheets[0].items.reverse();
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::StyleEffectsChanged { .. }))
        );
    }

    #[test]
    fn sheet_extension_placement_includes_omitted_block_items() {
        let extension = Extension {
            capability: ExtensionId::parse("charts@1").expect("capability"),
            name: "chart".to_owned(),
            payload: "{}".to_owned(),
            origin: None,
            payload_origin: None,
        };
        let left = workbook(vec![
            SheetItem::Block(block("A1", vec![vec![Value::Number(1.0)]])),
            SheetItem::Extension(extension.clone()),
        ]);
        let right = workbook(vec![
            SheetItem::Extension(extension),
            SheetItem::Block(block("A1", vec![vec![Value::Number(1.0)]])),
        ]);
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::SheetItemsChanged { .. }))
        );
    }

    #[test]
    fn workbook_extension_placement_uses_relative_origin_order() {
        let mut left = workbook(Vec::new());
        left.sheets[0].origin = Some(Origin {
            span: marksheet_model::ByteSpan::empty(30),
        });
        left.styles.push(Style {
            id: id("bold"),
            properties: StyleProperties::default(),
            origin: Some(Origin {
                span: marksheet_model::ByteSpan::empty(20),
            }),
        });
        left.extension_instances.push(Extension {
            capability: ExtensionId::parse("charts@1").expect("capability"),
            name: "chart".to_owned(),
            payload: "{}".to_owned(),
            origin: Some(Origin {
                span: marksheet_model::ByteSpan::empty(10),
            }),
            payload_origin: None,
        });
        let mut right = left.clone();
        right.extension_instances[0].origin = Some(Origin {
            span: marksheet_model::ByteSpan::empty(25),
        });
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::WorkbookExtensionsChanged { .. }))
        );
    }

    #[test]
    fn workbook_extension_placement_includes_explicit_book_order() {
        let mut left = workbook(Vec::new());
        left.book_origin = Some(Origin {
            span: marksheet_model::ByteSpan::empty(20),
        });
        left.sheets[0].origin = Some(Origin {
            span: marksheet_model::ByteSpan::empty(30),
        });
        left.extension_instances.push(Extension {
            capability: ExtensionId::parse("charts@1").expect("capability"),
            name: "chart".to_owned(),
            payload: "{}".to_owned(),
            origin: Some(Origin {
                span: marksheet_model::ByteSpan::empty(10),
            }),
            payload_origin: None,
        });
        let mut right = left.clone();
        right.book_origin = Some(Origin {
            span: marksheet_model::ByteSpan::empty(5),
        });
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::WorkbookExtensionsChanged { .. }))
        );
    }

    #[test]
    fn extension_declaration_placement_includes_other_root_declarations() {
        let mut left = workbook(Vec::new());
        left.sheets[0].origin = Some(Origin {
            span: marksheet_model::ByteSpan::empty(30),
        });
        left.styles.push(Style {
            id: id("bold"),
            properties: StyleProperties::default(),
            origin: Some(Origin {
                span: marksheet_model::ByteSpan::empty(20),
            }),
        });
        left.extensions.push(ExtensionDeclaration {
            capability: ExtensionId::parse("charts@1").expect("capability"),
            required: false,
            origin: Some(Origin {
                span: marksheet_model::ByteSpan::empty(10),
            }),
        });
        let mut right = left.clone();
        right.extensions[0].origin = Some(Origin {
            span: marksheet_model::ByteSpan::empty(25),
        });
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(
                    change,
                    SemanticChange::ExtensionDeclarationsChanged { .. }
                ))
        );
    }

    #[test]
    fn table_column_style_targets_only_data_cells() {
        let table = Table {
            id: id("costs"),
            block: block(
                "A1",
                vec![
                    vec![Value::Text("Cost".to_owned())],
                    vec![Value::Number(10.0)],
                    vec![Value::Number(20.0)],
                ],
            ),
            origin: None,
        };
        let mut left = workbook(vec![
            SheetItem::Table(table.clone()),
            SheetItem::Apply(Apply {
                target: ApplyTarget::Table {
                    table: id("costs"),
                    region: TableRegion::Column {
                        header: "Cost".to_owned(),
                    },
                },
                styles: vec![id("bold")],
                origin: None,
            }),
        ]);
        left.styles.push(style(
            "bold",
            StyleProperties {
                bold: Some(true),
                ..StyleProperties::default()
            },
        ));
        let mut right = left.clone();
        right.sheets[0].items[1] = apply("A2:A3", &["bold"]);
        assert!(semantic_diff(&left, &right).is_empty());

        let header_only = Table {
            id: id("header_only"),
            block: block("A1", vec![vec![Value::Text("Cost".to_owned())]]),
            origin: None,
        };
        let mut empty_column = workbook(vec![
            SheetItem::Table(header_only),
            SheetItem::Apply(Apply {
                target: ApplyTarget::Table {
                    table: id("header_only"),
                    region: TableRegion::Column {
                        header: "Cost".to_owned(),
                    },
                },
                styles: vec![id("bold")],
                origin: None,
            }),
        ]);
        empty_column.styles.push(style(
            "bold",
            StyleProperties {
                bold: Some(true),
                ..StyleProperties::default()
            },
        ));
        let mut no_effect = workbook(vec![SheetItem::Table(Table {
            id: id("header_only"),
            block: block("A1", vec![vec![Value::Text("Cost".to_owned())]]),
            origin: None,
        })]);
        no_effect.styles.push(style(
            "bold",
            StyleProperties {
                bold: Some(true),
                ..StyleProperties::default()
            },
        ));
        assert!(semantic_diff(&empty_column, &no_effect).is_empty());
    }

    #[test]
    fn adversarial_style_boundary_work_returns_unsupported() {
        let mut items = Vec::new();
        for value in 1..=200 {
            let coordinate = Coordinate::new(value, value).expect("coordinate");
            items.push(SheetItem::Apply(Apply {
                target: ApplyTarget::Range(Range::single(coordinate)),
                styles: vec![id("bold")],
                origin: None,
            }));
        }
        let mut workbook = workbook(items);
        workbook.styles.push(style(
            "bold",
            StyleProperties {
                bold: Some(true),
                ..StyleProperties::default()
            },
        ));
        assert!(
            semantic_diff(&workbook, &workbook)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::UnsupportedComparison(_)))
        );
    }

    #[test]
    fn table_data_changes_are_reported_once_at_the_coordinate() {
        let left = workbook(vec![SheetItem::Table(Table {
            id: id("costs"),
            block: block(
                "A1",
                vec![
                    vec![
                        Value::Text("Item".to_owned()),
                        Value::Text("Cost".to_owned()),
                    ],
                    vec![Value::Text("Rent".to_owned()), Value::Number(10.0)],
                ],
            ),
            origin: None,
        })]);
        let mut right = left.clone();
        let SheetItem::Table(table) = &mut right.sheets[0].items[0] else {
            panic!("table item");
        };
        table.block.cells[1][1] = Cell::new(Value::Number(20.0));

        let changes = semantic_diff(&left, &right).changes;
        assert!(matches!(
            changes.as_slice(),
            [SemanticChange::CellsChanged { cells, .. }] if cells.len() == 1
        ));
    }

    #[test]
    fn table_metadata_changes_remain_structural_changes() {
        let left = workbook(vec![SheetItem::Table(Table {
            id: id("costs"),
            block: block("A1", vec![vec![Value::Text("Item".to_owned())]]),
            origin: None,
        })]);
        let mut right = left.clone();
        let SheetItem::Table(table) = &mut right.sheets[0].items[0] else {
            panic!("table item");
        };
        table.id = id("expenses");

        assert!(matches!(
            semantic_diff(&left, &right).changes.as_slice(),
            [SemanticChange::SheetItemsChanged { .. }]
        ));
    }

    #[test]
    fn malformed_formula_is_never_silently_equal() {
        let left = workbook(vec![SheetItem::Block(block(
            "A1",
            vec![vec![formula("=")]],
        ))]);
        let right = left.clone();
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::UnsupportedComparison(_)))
        );
    }

    #[test]
    fn style_properties_ignore_origins_and_retain_exact_numbers() {
        let mut left = workbook(Vec::new());
        left.styles.push(Style {
            id: id("text"),
            properties: StyleProperties {
                text_color: Some(Color::parse("#123456").expect("color")),
                font_size: Some(-0.0),
                ..StyleProperties::default()
            },
            origin: None,
        });
        let mut right = left.clone();
        right.styles[0].properties.font_size = Some(0.0);
        assert!(
            semantic_diff(&left, &right)
                .changes
                .iter()
                .any(|change| matches!(change, SemanticChange::StyleChanged { .. }))
        );
    }

    #[test]
    fn no_dense_footprint_expansion_is_needed_for_distant_blocks() {
        let left = workbook(vec![SheetItem::Block(block(
            "A1",
            vec![vec![Value::Number(1.0)]],
        ))]);
        let right = workbook(vec![SheetItem::Block(block(
            "XFD1048576",
            vec![vec![Value::Number(1.0)]],
        ))]);
        let diff = semantic_diff(&left, &right);
        let SemanticChange::CellsChanged { cells, .. } = &diff.changes[0] else {
            panic!("expected sparse cells");
        };
        assert_eq!(cells.len(), 2);
        let _ = Footprint::new(coordinate("XFD1048576"), 1, 1).expect("footprint");
    }
}
