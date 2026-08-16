//! Source-preserving workbook preparation for calculation.
//!
//! Preparation turns the source-oriented model into deterministic sparse
//! indexes.  It deliberately does not parse formulas: the formula layer
//! consumes the stable authored/virtual cell view exposed here.

mod fill;
mod index;
mod resolve;

pub use index::{
    AuthoredCell, FillIndex, FootprintIndex, FootprintKind, NameIndex, PreparedSheet,
    PreparedWorkbook, TableIndex, TableRowContext, VirtualCell,
};
pub use resolve::{
    CompileIssue, CompileIssueKind, CompileLimits, CompiledFormula, FormulaProgram, PORTABLE_A1_V1,
    ResolvedArea, ResolvedReference, ResolvedReferenceAt, ResourceLimitKind,
    UNRESOLVED_REFERENCE_DIAGNOSTIC, UnresolvedReferenceKind, UnsupportedFormulaProfile,
    compile_formulas,
};

use std::fmt;

use marksheet_model::{CoordinateError, Origin, Range, SheetId, TableId};

/// Explicit resource bounds for preparation.
///
/// A Marksheet file has no format-level coordinate limit.  These limits bound
/// work performed by one calculator invocation; exceeding one is an error,
/// never a partial expansion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrepareLimits {
    /// Maximum number of cells in a range that preparation needs to enumerate
    /// or retain as a concrete range target.
    pub max_range_cells: u64,
    /// Maximum total virtual cells created by `@fill` directives per sheet.
    pub max_virtual_cells: u64,
}

impl Default for PrepareLimits {
    fn default() -> Self {
        Self {
            max_range_cells: 1_000_000,
            max_virtual_cells: 1_000_000,
        }
    }
}

/// Failure while deriving calculation indexes.
///
/// The builder is atomic: a [`PreparedWorkbook`] is returned only if all
/// sheets, names, and virtual fill destinations have been indexed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrepareError {
    Coordinate {
        source: CoordinateError,
    },
    DuplicateSheet {
        sheet: SheetId,
        origin: Option<Origin>,
    },
    DuplicateTable {
        table: TableId,
        origin: Option<Origin>,
    },
    DuplicateName {
        name: marksheet_model::NameId,
        origin: Option<Origin>,
    },
    TableNameConflict {
        identifier: String,
        origin: Option<Origin>,
    },
    OverlappingFootprints {
        sheet: SheetId,
        first_origin: Option<Origin>,
        second_origin: Option<Origin>,
    },
    MalformedBlock {
        sheet: SheetId,
        origin: Option<Origin>,
    },
    InvalidTableHeader {
        table: TableId,
        origin: Option<Origin>,
    },
    DuplicateTableHeader {
        table: TableId,
        header: String,
        origin: Option<Origin>,
    },
    UnresolvedSheet {
        sheet: SheetId,
        origin: Option<Origin>,
    },
    UnresolvedTable {
        table: TableId,
        origin: Option<Origin>,
    },
    UnresolvedTableHeader {
        table: TableId,
        header: String,
        origin: Option<Origin>,
    },
    FillHasNoOwner {
        sheet: SheetId,
        target: Range,
        origin: Option<Origin>,
    },
    FillHasMultipleOwners {
        sheet: SheetId,
        target: Range,
        origin: Option<Origin>,
    },
    FillMustFollowOwner {
        sheet: SheetId,
        table: TableId,
        origin: Option<Origin>,
    },
    HeaderOnlyTableFill {
        sheet: SheetId,
        table: TableId,
        header: String,
        origin: Option<Origin>,
    },
    FillTargetsNonBlankCell {
        sheet: SheetId,
        coordinate: marksheet_model::Coordinate,
        origin: Option<Origin>,
    },
    FillTargetsAbsentCell {
        sheet: SheetId,
        coordinate: marksheet_model::Coordinate,
        origin: Option<Origin>,
    },
    OverlappingFills {
        sheet: SheetId,
        coordinate: marksheet_model::Coordinate,
        first_origin: Option<Origin>,
        second_origin: Option<Origin>,
    },
    RangeLimitExceeded {
        range: Range,
        limit: u64,
        origin: Option<Origin>,
    },
    VirtualCellLimitExceeded {
        sheet: SheetId,
        limit: u64,
        origin: Option<Origin>,
    },
    SourceOrderOverflow,
}

impl From<CoordinateError> for PrepareError {
    fn from(source: CoordinateError) -> Self {
        Self::Coordinate { source }
    }
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinate { source } => source.fmt(f),
            Self::DuplicateSheet { sheet, .. } => write!(f, "duplicate sheet {sheet}"),
            Self::DuplicateTable { table, .. } => write!(f, "duplicate table {table}"),
            Self::DuplicateName { name, .. } => write!(f, "duplicate name {name}"),
            Self::TableNameConflict { identifier, .. } => {
                write!(f, "table and name share identifier {identifier:?}")
            }
            Self::OverlappingFootprints { sheet, .. } => {
                write!(f, "overlapping block footprints on sheet {sheet}")
            }
            Self::MalformedBlock { sheet, .. } => {
                write!(f, "non-rectangular or empty block on sheet {sheet}")
            }
            Self::InvalidTableHeader { table, .. } => write!(f, "invalid header in table {table}"),
            Self::DuplicateTableHeader { table, header, .. } => {
                write!(f, "duplicate header {header:?} in table {table}")
            }
            Self::UnresolvedSheet { sheet, .. } => write!(f, "unresolved sheet {sheet}"),
            Self::UnresolvedTable { table, .. } => write!(f, "unresolved table {table}"),
            Self::UnresolvedTableHeader { table, header, .. } => {
                write!(f, "unresolved header {header:?} in table {table}")
            }
            Self::FillHasNoOwner { sheet, target, .. } => {
                write!(
                    f,
                    "fill target {target} has no preceding owner on sheet {sheet}"
                )
            }
            Self::FillHasMultipleOwners { sheet, target, .. } => {
                write!(
                    f,
                    "fill target {target} has multiple owners on sheet {sheet}"
                )
            }
            Self::FillMustFollowOwner { sheet, table, .. } => {
                write!(f, "fill for table {table} must follow it on sheet {sheet}")
            }
            Self::HeaderOnlyTableFill { table, header, .. } => {
                write!(f, "cannot fill header-only table {table} column {header:?}")
            }
            Self::FillTargetsNonBlankCell {
                sheet, coordinate, ..
            } => {
                write!(f, "fill targets nonblank cell {sheet}:{coordinate}")
            }
            Self::FillTargetsAbsentCell {
                sheet, coordinate, ..
            } => {
                write!(f, "fill targets absent cell {sheet}:{coordinate}")
            }
            Self::OverlappingFills {
                sheet, coordinate, ..
            } => {
                write!(f, "fills overlap at {sheet}:{coordinate}")
            }
            Self::RangeLimitExceeded { range, limit, .. } => {
                write!(f, "range {range} exceeds configured {limit}-cell limit")
            }
            Self::VirtualCellLimitExceeded { sheet, limit, .. } => {
                write!(
                    f,
                    "virtual fills on sheet {sheet} exceed configured {limit}-cell limit"
                )
            }
            Self::SourceOrderOverflow => f.write_str("source order exceeds platform limits"),
        }
    }
}

impl std::error::Error for PrepareError {}
