//! A deliberately isolated feasibility probe for a Formualizer-backed
//! Marksheet calculator.
//!
//! This crate is not part of the Marksheet workspace.  It proves adapter-side
//! translation mechanics without exposing any Formualizer type in its public
//! API. The optional `calc-link` feature verifies coexistence with the public
//! `marksheet-calc` boundary; this spike intentionally does not claim to be a
//! complete implementation of that boundary.

#![forbid(unsafe_code)]

use marksheet_model::{Coordinate, NameId, SheetId};

// Compile-link the production boundary without exporting or adopting it.
#[cfg(feature = "calc-link")]
use marksheet_calc as _;

/// Excel's finite grid limits, also the documented Formualizer load limits.
pub const MAX_COLUMNS: u64 = 16_384;
/// Excel's finite grid limits, also the documented Formualizer load limits.
pub const MAX_ROWS: u64 = 1_048_576;

/// A checked coordinate accepted by the engine adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineCoordinate {
    /// One-based column index.
    pub column: u32,
    /// One-based row index.
    pub row: u32,
}

/// Why a Marksheet coordinate cannot be represented by Formualizer's Excel
/// grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinateLimitError {
    /// The column exceeds the engine's supported grid.
    Column { actual: u64, maximum: u64 },
    /// The row exceeds the engine's supported grid.
    Row { actual: u64, maximum: u64 },
}

impl TryFrom<Coordinate> for EngineCoordinate {
    type Error = CoordinateLimitError;

    fn try_from(value: Coordinate) -> Result<Self, Self::Error> {
        if value.column > MAX_COLUMNS {
            return Err(CoordinateLimitError::Column {
                actual: value.column,
                maximum: MAX_COLUMNS,
            });
        }
        if value.row > MAX_ROWS {
            return Err(CoordinateLimitError::Row {
                actual: value.row,
                maximum: MAX_ROWS,
            });
        }
        Ok(Self {
            column: u32::try_from(value.column).expect("checked Formualizer column limit"),
            row: u32::try_from(value.row).expect("checked Formualizer row limit"),
        })
    }
}

/// Maps a stable Marksheet sheet ID to an engine-private, formula-safe name.
///
/// The engine never receives a user-facing sheet label.  This preserves stable
/// cross-sheet references when the label changes and reserves a namespace
/// owned by the adapter.
#[must_use]
pub fn engine_sheet_name(id: &SheetId) -> String {
    format!("ms_sheet_{}", id.as_str())
}

/// Maps a Marksheet name ID to an engine-private defined-name spelling.
#[must_use]
pub fn engine_name(id: &NameId) -> String {
    format!("ms_name_{}", id.as_str())
}

/// A source location retained by the adapter, rather than delegated to the
/// engine's formula representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAnchor {
    /// Marksheet source identity (normally a path or document key).
    pub source_id: String,
    /// Inclusive byte start of the formula body in the Marksheet source.
    pub byte_start: u64,
    /// Exclusive byte end of the formula body in the Marksheet source.
    pub byte_end: u64,
}

/// A source-connected diagnostic that can wrap an engine error without
/// exposing an engine error/AST type in Marksheet's future public API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterDiagnostic {
    /// Stable Marksheet diagnostic code selected by the adapter.
    pub code: &'static str,
    /// Engine-independent message supplied by the adapter.
    pub message: String,
    /// Source formula location retained at translation time.
    pub source: SourceAnchor,
    /// Cell containing the formula.
    pub cell: EngineCoordinate,
}

/// Constructs the adapter's source-connected diagnostic envelope.
#[must_use]
pub fn source_diagnostic(
    code: &'static str,
    message: impl Into<String>,
    source: SourceAnchor,
    cell: EngineCoordinate,
) -> AdapterDiagnostic {
    AdapterDiagnostic {
        code,
        message: message.into(),
        source,
        cell,
    }
}

/// A table range already resolved from Marksheet semantic data.
///
/// Rows and columns are one-based and inclusive.  The layout intentionally
/// contains no Formualizer table metadata: structured references are lowered
/// before the engine sees a formula.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableLayout {
    /// Engine-private sheet name containing the table.
    pub engine_sheet: String,
    /// First data row (headers are excluded).
    pub first_data_row: u32,
    /// Last data row (headers and totals are excluded).
    pub last_data_row: u32,
    /// First table column.
    pub first_column: u32,
    /// Ordered, unescaped column headers.
    pub headers: Vec<String>,
}

/// The portable structured-reference forms that can safely lower to A1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StructuredSelector {
    /// A complete data-column reference such as `sales[amount]`.
    DataColumn,
    /// A current-row reference such as `[@amount]`.
    CurrentRow { formula_row: u32 },
}

/// A structured reference after Marksheet parses and resolves its table ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedStructuredReference {
    /// Header spelling after `]]` unescaping.
    pub header: String,
    /// The selector semantics to lower.
    pub selector: StructuredSelector,
}

/// Failure to lower a resolved structured reference without changing meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StructuredLoweringError {
    /// No matching table header was found.
    UnknownHeader(String),
    /// The table has no data row for a data-column selector.
    EmptyData,
    /// A current-row selector was placed outside the table's data rows.
    CurrentRowOutsideTable { row: u32 },
    /// A header's resolved column falls outside the engine's grid.
    ColumnOutOfRange { column: u32 },
    /// The host cannot represent the header position as an engine column.
    HeaderIndexOutOfRange,
}

/// Lowers a resolved structured reference to a finite absolute A1 expression.
///
/// The caller must parse and resolve table references first; textual rewrites
/// are intentionally not attempted.  Formualizer currently reports current
/// row table evaluation as unsupported, so this is the viable adapter route.
///
/// # Errors
///
/// Returns an error when the header is unknown, the table has no data rows, a
/// current-row reference has no table-row context, or the resolved engine
/// column would exceed its finite grid.
pub fn lower_structured_reference(
    layout: &TableLayout,
    reference: &ResolvedStructuredReference,
) -> Result<String, StructuredLoweringError> {
    let index = layout
        .headers
        .iter()
        .position(|header| header == &reference.header)
        .ok_or_else(|| StructuredLoweringError::UnknownHeader(reference.header.clone()))?;
    let header_index =
        u32::try_from(index).map_err(|_| StructuredLoweringError::HeaderIndexOutOfRange)?;
    let column = layout
        .first_column
        .checked_add(header_index)
        .ok_or(StructuredLoweringError::ColumnOutOfRange { column: u32::MAX })?;
    if u64::from(column) > MAX_COLUMNS {
        return Err(StructuredLoweringError::ColumnOutOfRange { column });
    }
    let column_name = column_name(column);
    let prefix = format!("{}!${column_name}$", layout.engine_sheet);

    match reference.selector {
        StructuredSelector::DataColumn => {
            if layout.first_data_row > layout.last_data_row {
                return Err(StructuredLoweringError::EmptyData);
            }
            Ok(format!(
                "{prefix}{}:${column_name}${}",
                layout.first_data_row, layout.last_data_row
            ))
        }
        StructuredSelector::CurrentRow { formula_row } => {
            if formula_row < layout.first_data_row || formula_row > layout.last_data_row {
                return Err(StructuredLoweringError::CurrentRowOutsideTable { row: formula_row });
            }
            Ok(format!("{prefix}{formula_row}"))
        }
    }
}

fn column_name(mut column: u32) -> String {
    let mut output = String::new();
    while column > 0 {
        let digit = ((column - 1) % 26) as u8;
        output.push(char::from(b'A' + digit));
        column = (column - 1) / 26;
    }
    output.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use formualizer::{
        common::{ExcelError, ExcelErrorKind, LiteralValue},
        eval::{
            engine::{
                Engine, EvalConfig,
                named_range::{NameScope, NamedDefinition},
            },
            function::Function,
            reference::{CellRef, Coord},
            timezone::TimeZoneSpec,
            traits::{
                EvaluationContext, FunctionProvider, NamedRangeResolver, Range, RangeResolver,
                ReferenceResolver, Resolver, SourceResolver, Table, TableResolver,
            },
        },
        parse::parser::{TableReference, parse},
    };
    use std::sync::Arc;

    #[derive(Debug)]
    struct NoExternalResolver;

    impl ReferenceResolver for NoExternalResolver {
        fn resolve_cell_reference(
            &self,
            _sheet: Option<&str>,
            _row: u32,
            _column: u32,
        ) -> Result<LiteralValue, ExcelError> {
            Err(ExcelError::new(ExcelErrorKind::NImpl))
        }
    }

    impl RangeResolver for NoExternalResolver {
        fn resolve_range_reference(
            &self,
            _sheet: Option<&str>,
            _start_row: Option<u32>,
            _start_column: Option<u32>,
            _end_row: Option<u32>,
            _end_column: Option<u32>,
        ) -> Result<Box<dyn Range>, ExcelError> {
            Err(ExcelError::new(ExcelErrorKind::NImpl))
        }
    }

    impl NamedRangeResolver for NoExternalResolver {
        fn resolve_named_range_reference(
            &self,
            _name: &str,
        ) -> Result<Vec<Vec<LiteralValue>>, ExcelError> {
            Err(ExcelError::new(ExcelErrorKind::Name))
        }
    }

    impl TableResolver for NoExternalResolver {
        fn resolve_table_reference(
            &self,
            _reference: &TableReference,
        ) -> Result<Box<dyn Table>, ExcelError> {
            Err(ExcelError::new(ExcelErrorKind::NImpl))
        }
    }

    impl SourceResolver for NoExternalResolver {}

    impl FunctionProvider for NoExternalResolver {
        fn get_function(&self, namespace: &str, name: &str) -> Option<Arc<dyn Function>> {
            formualizer::eval::function_registry::get(namespace, name)
        }

        fn get_function_for_planning(
            &self,
            namespace: &str,
            name: &str,
        ) -> Option<Arc<dyn Function>> {
            formualizer::eval::function_registry::get_for_planning(namespace, name)
        }

        fn planning_semantic_revision(&self) -> Option<u64> {
            Some(0)
        }
    }

    impl Resolver for NoExternalResolver {}
    impl EvaluationContext for NoExternalResolver {}

    fn deterministic_engine() -> Engine<NoExternalResolver> {
        let config = EvalConfig {
            enable_parallel: false,
            deterministic_mode: formualizer::eval::engine::DeterministicMode::Enabled {
                timestamp_utc: DateTime::<Utc>::UNIX_EPOCH,
                timezone: TimeZoneSpec::Utc,
            },
            ..EvalConfig::default()
        };
        Engine::new(NoExternalResolver, config)
    }

    fn assert_number(value: Option<LiteralValue>, expected: f64) {
        match value {
            Some(LiteralValue::Number(actual)) => {
                assert!(
                    (actual - expected).abs() < f64::EPSILON,
                    "{actual} != {expected}"
                );
            }
            other => panic!("expected number {expected}, got {other:?}"),
        }
    }

    #[test]
    fn coordinate_limits_are_checked_before_engine_calls() {
        assert_eq!(
            EngineCoordinate::try_from(Coordinate::new(16_384, 1_048_576).unwrap()).unwrap(),
            EngineCoordinate {
                column: 16_384,
                row: 1_048_576
            }
        );
        assert_eq!(
            EngineCoordinate::try_from(Coordinate::new(16_385, 1).unwrap()),
            Err(CoordinateLimitError::Column {
                actual: 16_385,
                maximum: 16_384
            })
        );
        assert_eq!(
            EngineCoordinate::try_from(Coordinate::new(1, 1_048_577).unwrap()),
            Err(CoordinateLimitError::Row {
                actual: 1_048_577,
                maximum: 1_048_576
            })
        );
    }

    #[test]
    fn stable_ids_map_to_private_engine_symbols() {
        let sheet = SheetId::parse("budget").unwrap();
        let name = NameId::parse("tax_rate").unwrap();
        assert_eq!(engine_sheet_name(&sheet), "ms_sheet_budget");
        assert_eq!(engine_name(&name), "ms_name_tax_rate");
    }

    #[test]
    fn evaluates_cross_sheet_and_named_range_mappings() {
        let inputs = engine_sheet_name(&SheetId::parse("inputs").unwrap());
        let model = engine_sheet_name(&SheetId::parse("model").unwrap());
        let tax_rate = engine_name(&NameId::parse("tax_rate").unwrap());
        let mut engine = deterministic_engine();
        engine
            .set_cell_value(&inputs, 1, 1, LiteralValue::Number(2.0))
            .unwrap();
        let inputs_sheet = engine.sheet_id(&inputs).unwrap();
        engine
            .define_name(
                &tax_rate,
                NamedDefinition::Cell(CellRef::new(
                    inputs_sheet,
                    Coord::from_excel(1, 1, true, true),
                )),
                NameScope::Workbook,
            )
            .unwrap();
        engine
            .set_cell_formula(
                &model,
                1,
                1,
                parse(format!("={inputs}!A1+{tax_rate}")).unwrap(),
            )
            .unwrap();
        assert_number(engine.evaluate_cell(&model, 1, 1).unwrap(), 4.0);
    }

    #[test]
    fn deterministic_mode_uses_a_fixed_clock_without_system_clock_feature() {
        let sheet = engine_sheet_name(&SheetId::parse("clock").unwrap());
        let mut first = deterministic_engine();
        first
            .set_cell_formula(&sheet, 1, 1, parse("=TODAY()").unwrap())
            .unwrap();
        let first_value = first.evaluate_cell(&sheet, 1, 1).unwrap();

        let mut second = deterministic_engine();
        second
            .set_cell_formula(&sheet, 1, 1, parse("=TODAY()").unwrap())
            .unwrap();
        let second_value = second.evaluate_cell(&sheet, 1, 1).unwrap();
        assert_eq!(first_value, second_value);
    }

    #[test]
    fn dirty_dependency_updates_are_demand_driven() {
        let sheet = engine_sheet_name(&SheetId::parse("incremental").unwrap());
        let mut engine = deterministic_engine();
        engine
            .set_cell_value(&sheet, 1, 1, LiteralValue::Number(2.0))
            .unwrap();
        engine
            .set_cell_formula(&sheet, 1, 2, parse("=A1+1").unwrap())
            .unwrap();
        assert_number(engine.evaluate_cell(&sheet, 1, 2).unwrap(), 3.0);
        engine
            .set_cell_value(&sheet, 1, 1, LiteralValue::Number(7.0))
            .unwrap();
        assert_number(engine.evaluate_cell(&sheet, 1, 2).unwrap(), 8.0);
    }

    #[test]
    fn structured_references_lower_before_engine_evaluation() {
        let layout = TableLayout {
            engine_sheet: engine_sheet_name(&SheetId::parse("sales").unwrap()),
            first_data_row: 2,
            last_data_row: 3,
            first_column: 1,
            headers: vec!["item".into(), "amount".into()],
        };
        let amount = ResolvedStructuredReference {
            header: "amount".into(),
            selector: StructuredSelector::DataColumn,
        };
        let current_amount = ResolvedStructuredReference {
            header: "amount".into(),
            selector: StructuredSelector::CurrentRow { formula_row: 3 },
        };
        assert_eq!(
            lower_structured_reference(&layout, &amount).unwrap(),
            "ms_sheet_sales!$B$2:$B$3"
        );
        assert_eq!(
            lower_structured_reference(&layout, &current_amount).unwrap(),
            "ms_sheet_sales!$B$3"
        );

        let mut engine = deterministic_engine();
        engine
            .set_cell_value(&layout.engine_sheet, 2, 2, LiteralValue::Number(4.0))
            .unwrap();
        engine
            .set_cell_value(&layout.engine_sheet, 3, 2, LiteralValue::Number(6.0))
            .unwrap();
        let lowered = lower_structured_reference(&layout, &amount).unwrap();
        engine
            .set_cell_formula(
                &layout.engine_sheet,
                4,
                2,
                parse(format!("=SUM({lowered})")).unwrap(),
            )
            .unwrap();
        assert_number(
            engine.evaluate_cell(&layout.engine_sheet, 4, 2).unwrap(),
            10.0,
        );
    }

    #[test]
    fn structured_lowering_rejects_ambiguous_or_out_of_table_current_rows() {
        let layout = TableLayout {
            engine_sheet: "ms_sheet_data".into(),
            first_data_row: 2,
            last_data_row: 3,
            first_column: 1,
            headers: vec!["amount".into()],
        };
        assert_eq!(
            lower_structured_reference(
                &layout,
                &ResolvedStructuredReference {
                    header: "amount".into(),
                    selector: StructuredSelector::CurrentRow { formula_row: 4 },
                },
            ),
            Err(StructuredLoweringError::CurrentRowOutsideTable { row: 4 })
        );
    }

    #[test]
    fn engine_diagnostics_can_be_connected_to_retained_source_locations() {
        let diagnostic = source_diagnostic(
            "MS2202",
            "formula parser rejected a translated expression",
            SourceAnchor {
                source_id: "book.ms".into(),
                byte_start: 42,
                byte_end: 54,
            },
            EngineCoordinate { column: 3, row: 8 },
        );
        assert_eq!(diagnostic.source.byte_start, 42);
        assert_eq!(diagnostic.cell, EngineCoordinate { column: 3, row: 8 });
    }
}
