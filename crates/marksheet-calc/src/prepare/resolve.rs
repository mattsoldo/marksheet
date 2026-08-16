//! Formula compilation against a fully prepared workbook.
//!
//! Parsing and symbol resolution intentionally happen after preparation so
//! forward references to sheets, names, and tables behave exactly like
//! backward references.  The compiler retains the Marksheet-owned syntax tree
//! and records resolved references alongside it; evaluators therefore do not
//! need to inherit an external engine's reference model.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use marksheet_model::{
    ByteSpan, CellError, Coordinate, Diagnostic, DiagnosticCode, DiagnosticContext, FormulaSource,
    LabeledSpan, NameId, NameTarget, Origin, Range, Severity, SheetId, TableId, Value, Workbook,
};

use crate::formula::{
    CopyOffset, Expr, ExprKind, Formula, FormulaError, FormulaErrorKind, FunctionCall, Literal,
    ParseLimits, Reference, StructuredReference, TableRegion, adjust_references, parse,
};
use crate::graph::CellKey;

use super::{PreparedSheet, PreparedWorkbook, TableRowContext};

/// Formula profile implemented by this compiler.
pub const PORTABLE_A1_V1: &str = "portable-a1@1";
/// Stable diagnostic code for references that cannot be resolved.
pub const UNRESOLVED_REFERENCE_DIAGNOSTIC: &str = "MS2103";

/// Bounds for formula compilation and finite dependency expansion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileLimits {
    pub parse: ParseLimits,
    /// Maximum cells represented by any one range reference.
    pub max_range_cells: u64,
    /// Maximum unique cells read by any one formula.
    pub max_dependencies_per_formula: u64,
    /// Maximum unique `(formula, referenced cell)` edges in the program.
    pub max_total_dependencies: u64,
}

impl Default for CompileLimits {
    fn default() -> Self {
        Self {
            parse: ParseLimits::default(),
            max_range_cells: 1_000_000,
            max_dependencies_per_formula: 1_000_000,
            max_total_dependencies: 10_000_000,
        }
    }
}

/// The requested workbook profile is not implemented by this compiler.
///
/// This is an integration failure rather than a cell issue: compiling the
/// source under different formula semantics would silently produce incorrect
/// results.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedFormulaProfile {
    pub profile: String,
    pub origin: Option<Origin>,
}

impl fmt::Display for UnsupportedFormulaProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported formula profile {:?}; expected {PORTABLE_A1_V1:?}",
            self.profile
        )
    }
}

impl std::error::Error for UnsupportedFormulaProfile {}

/// A concrete, possibly empty, rectangular target.
///
/// Empty ranges occur for `table[#Data]` and table-column references when a
/// valid table contains only its header row.  Keeping that state explicit
/// avoids inventing an invalid zero coordinate or conflating it with Blank.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedArea {
    pub sheet: SheetId,
    pub range: Option<Range>,
}

/// An evaluator-facing reference with its source-level provenance retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedReference {
    Cell {
        cell: CellKey,
    },
    Range {
        area: ResolvedArea,
    },
    Name {
        name: NameId,
        area: ResolvedArea,
    },
    /// A name whose authored target is exactly one sheet-qualified cell.
    ///
    /// This is intentionally distinct from [`Self::Name`]: a source target
    /// such as `sheet!A1:A1` remains range-shaped and keeps range semantics.
    NamedCell {
        name: NameId,
        cell: CellKey,
    },
    TableColumn {
        table: TableId,
        header: String,
        area: ResolvedArea,
    },
    TableRegion {
        table: TableId,
        region: TableRegion,
        area: ResolvedArea,
    },
    CurrentRow {
        table: TableId,
        header: String,
        cell: CellKey,
    },
    /// A syntactically valid reference whose symbol or context was invalid.
    /// Evaluation returns this error only if execution reaches the reference.
    Error {
        error: CellError,
    },
}

impl ResolvedReference {
    /// Returns the cell read by a scalar reference, if this is one.
    #[must_use]
    pub fn cell(&self) -> Option<&CellKey> {
        match self {
            Self::Cell { cell } | Self::NamedCell { cell, .. } | Self::CurrentRow { cell, .. } => {
                Some(cell)
            }
            _ => None,
        }
    }

    /// Returns the rectangular target of a range-like reference.
    #[must_use]
    pub fn area(&self) -> Option<&ResolvedArea> {
        match self {
            Self::Range { area }
            | Self::Name { area, .. }
            | Self::TableColumn { area, .. }
            | Self::TableRegion { area, .. } => Some(area),
            _ => None,
        }
    }

    /// Returns the runtime error for an invalid reference.
    #[must_use]
    pub const fn error(&self) -> Option<CellError> {
        match self {
            Self::Error { error } => Some(*error),
            _ => None,
        }
    }
}

/// One reference occurrence in an adjusted formula.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedReferenceAt {
    /// Byte offsets relative to the decoded formula, including its leading `=`.
    pub span: ByteSpan,
    pub reference: ResolvedReference,
}

/// One parsed, fill-adjusted, and symbol-resolved formula.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledFormula {
    pub cell: CellKey,
    pub source: FormulaSource,
    pub origin: Option<Origin>,
    /// Syntax after conventional A1 copy adjustment for a virtual fill cell.
    pub formula: Formula,
    /// Preorder reference occurrences, matching traversal of `formula`.
    pub references: Vec<ResolvedReferenceAt>,
    /// Every cell referenced in any syntactic branch, sorted and deduplicated.
    pub dependencies: BTreeSet<CellKey>,
}

impl CompiledFormula {
    /// Finds the resolved occurrence for a reference-expression span.
    #[must_use]
    pub fn reference_at(&self, span: ByteSpan) -> Option<&ResolvedReference> {
        self.references
            .iter()
            .find(|occurrence| occurrence.span == span)
            .map(|occurrence| &occurrence.reference)
    }
}

/// All successfully parsed formulas plus deterministic compile issues.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FormulaProgram {
    pub formulas: BTreeMap<CellKey, CompiledFormula>,
    pub issues: Vec<CompileIssue>,
    /// Cells that have no executable AST because parsing or a whole-formula
    /// resource limit failed. These are invalid source, not runtime values.
    pub uncompiled_cells: BTreeSet<CellKey>,
    pub dependency_edges: u64,
}

impl FormulaProgram {
    #[must_use]
    pub fn formula(&self, cell: &CellKey) -> Option<&CompiledFormula> {
        self.formulas.get(cell)
    }

    #[must_use]
    pub fn is_uncompiled(&self, cell: &CellKey) -> bool {
        self.uncompiled_cells.contains(cell)
    }
}

/// Stable categories for formula compile issues.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileIssueKind {
    Syntax(FormulaErrorKind),
    Unresolved(UnresolvedReferenceKind),
    ResourceLimit(ResourceLimitKind),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnresolvedReferenceKind {
    Sheet(SheetId),
    Name(NameId),
    Table(TableId),
    TableHeader { table: TableId, header: String },
    CurrentRowOutsideTable,
    CurrentRowWrongTable { expected: TableId, actual: TableId },
    AdjustedReferenceOutOfBounds,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceLimitKind {
    RangeCells { actual: u128, limit: u64 },
    FormulaDependencies { actual_at_least: u64, limit: u64 },
    ProgramDependencies { actual: u64, limit: u64 },
}

/// Source-connected issue for one concrete formula cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileIssue {
    pub kind: CompileIssueKind,
    pub message: String,
    pub cell: CellKey,
    pub origin: Option<Origin>,
    /// Byte offsets into the decoded formula.  These are deliberately not
    /// added to `origin`: CSV quoting can make that arithmetic incorrect.
    pub formula_span: Option<ByteSpan>,
    /// Runtime value for a valid-but-unresolved reference. Syntax and resource
    /// failures have no spreadsheet value because the document is invalid.
    pub runtime_error: Option<CellError>,
}

impl CompileIssue {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match &self.kind {
            CompileIssueKind::Unresolved(_) => UNRESOLVED_REFERENCE_DIAGNOSTIC,
            CompileIssueKind::Syntax(_) | CompileIssueKind::ResourceLimit(_) => {
                crate::formula::FORMULA_SYNTAX_DIAGNOSTIC
            }
        }
    }

    /// Converts the issue to the model's source diagnostic without pretending
    /// decoded formula offsets are raw file offsets.
    ///
    /// # Errors
    ///
    /// Returns an error only if an internal compile diagnostic constant stops
    /// satisfying the model's diagnostic-code grammar.
    pub fn to_diagnostic(&self) -> Result<Diagnostic, marksheet_model::DiagnosticCodeError> {
        let span = self
            .origin
            .map_or_else(ByteSpan::default, |origin| origin.span);
        Ok(Diagnostic {
            code: DiagnosticCode::new(self.diagnostic_code())?,
            severity: Severity::Error,
            message: self.message.clone(),
            primary: LabeledSpan {
                span,
                label: Some(self.cell.to_string()),
            },
            related: Vec::new(),
            context: Some(DiagnosticContext {
                sheet: Some(self.cell.sheet.clone()),
                cell: Some(self.cell.coordinate),
            }),
            suggestion: None,
        })
    }
}

/// Compiles every authored and fill-derived formula in deterministic source
/// order.  Unoccupied coordinates on existing sheets are valid and become
/// Blank when the evaluator reads them.
///
/// # Errors
///
/// Returns [`UnsupportedFormulaProfile`] before compiling any cells when the
/// workbook requests formula semantics other than `portable-a1@1`.
pub fn compile_formulas(
    workbook: &Workbook,
    prepared: &PreparedWorkbook,
    limits: &CompileLimits,
) -> Result<FormulaProgram, UnsupportedFormulaProfile> {
    if workbook.settings.formula_profile != PORTABLE_A1_V1 {
        return Err(UnsupportedFormulaProfile {
            profile: workbook.settings.formula_profile.clone(),
            origin: workbook.origin,
        });
    }

    let mut program = FormulaProgram::default();
    for sheet in &prepared.sheets {
        compile_sheet(prepared, sheet, limits, &mut program);
    }
    Ok(program)
}

fn compile_sheet(
    prepared: &PreparedWorkbook,
    sheet: &PreparedSheet,
    limits: &CompileLimits,
    program: &mut FormulaProgram,
) {
    let mut inputs = formula_inputs(sheet);
    inputs.sort_by_key(|input| {
        let coordinate = input.coordinate();
        (
            input.source_order(),
            coordinate.row,
            coordinate.column,
            input.is_virtual(),
        )
    });

    // One fill may create many destinations. Parse its authored template once,
    // then bind the typed tree at each destination.
    let mut fill_parse_cache: BTreeMap<u64, Result<Formula, FormulaError>> = BTreeMap::new();
    for input in inputs {
        let key = CellKey::new(sheet.id.clone(), input.coordinate());
        let source = input.source().clone();
        let origin = input.origin();
        let prepared_formula =
            match prepare_formula(input, key.coordinate, limits, &mut fill_parse_cache) {
                Ok(formula) => formula,
                Err(error) => {
                    program.issues.push(syntax_issue(&key, origin, &error));
                    program.uncompiled_cells.insert(key);
                    continue;
                }
            };
        let PreparedFormula {
            formula,
            adjustment_errors,
        } = prepared_formula;
        for span in adjustment_errors {
            program.issues.push(CompileIssue {
                kind: CompileIssueKind::Unresolved(
                    UnresolvedReferenceKind::AdjustedReferenceOutOfBounds,
                ),
                message: unresolved_message(&UnresolvedReferenceKind::AdjustedReferenceOutOfBounds),
                cell: key.clone(),
                origin,
                formula_span: Some(span),
                runtime_error: Some(CellError::Reference),
            });
        }
        let row_context = input
            .table_row()
            .or_else(|| sheet.current_row_context(key.coordinate));
        let compiled = compile_one(
            prepared,
            sheet,
            key.clone(),
            source,
            origin,
            formula,
            row_context.as_ref(),
            limits,
            program.dependency_edges,
            &mut program.issues,
        );
        match compiled {
            Ok(compiled) => insert_compiled(program, key, compiled),
            Err(issue) => {
                program.issues.push(*issue);
                program.uncompiled_cells.insert(key);
            }
        }
    }
}

struct PreparedFormula {
    formula: Formula,
    adjustment_errors: Vec<ByteSpan>,
}

fn prepare_formula(
    input: FormulaInput<'_>,
    destination: Coordinate,
    limits: &CompileLimits,
    fill_parse_cache: &mut BTreeMap<u64, Result<Formula, FormulaError>>,
) -> Result<PreparedFormula, FormulaError> {
    let source = input.source();
    let parsed = if input.is_virtual() {
        fill_parse_cache
            .entry(input.source_order())
            .or_insert_with(|| parse(source.as_str(), &limits.parse))
            .clone()
    } else {
        parse(source.as_str(), &limits.parse)
    }?;
    let Some(anchor) = input.fill_anchor() else {
        return Ok(PreparedFormula {
            formula: parsed,
            adjustment_errors: Vec::new(),
        });
    };
    let mut adjustment_errors = Vec::new();
    let expression = bind_expression(
        &parsed.expression,
        CopyOffset::between(anchor, destination),
        &mut adjustment_errors,
    );
    Ok(PreparedFormula {
        formula: Formula { expression },
        adjustment_errors,
    })
}

fn insert_compiled(program: &mut FormulaProgram, key: CellKey, compiled: CompiledFormula) {
    let compiled_edges = u64::try_from(compiled.dependencies.len()).unwrap_or(u64::MAX);
    program.dependency_edges = program.dependency_edges.saturating_add(compiled_edges);
    program.formulas.insert(key, compiled);
}

fn bind_expression(expression: &Expr, offset: CopyOffset, errors: &mut Vec<ByteSpan>) -> Expr {
    let kind = match &expression.kind {
        ExprKind::Literal { value } => ExprKind::Literal {
            value: value.clone(),
        },
        ExprKind::Reference { .. } => {
            if let Ok(adjusted) = adjust_references(expression, offset) {
                return adjusted;
            }
            errors.push(expression.span);
            ExprKind::Literal {
                value: Literal::Error(CellError::Reference),
            }
        }
        ExprKind::Unary { operator, operand } => ExprKind::Unary {
            operator: *operator,
            operand: Box::new(bind_expression(operand, offset, errors)),
        },
        ExprKind::Binary {
            operator,
            left,
            right,
        } => ExprKind::Binary {
            operator: *operator,
            left: Box::new(bind_expression(left, offset, errors)),
            right: Box::new(bind_expression(right, offset, errors)),
        },
        ExprKind::Call { call } => ExprKind::Call {
            call: FunctionCall {
                name: call.name.clone(),
                arguments: call
                    .arguments
                    .iter()
                    .map(|argument| bind_expression(argument, offset, errors))
                    .collect(),
            },
        },
    };
    Expr {
        kind,
        span: expression.span,
    }
}

#[derive(Clone, Copy)]
enum FormulaInput<'a> {
    Authored {
        coordinate: Coordinate,
        source_order: u64,
        source: &'a FormulaSource,
        origin: Option<Origin>,
    },
    Virtual {
        coordinate: Coordinate,
        source_order: u64,
        source: &'a FormulaSource,
        origin: Option<Origin>,
        fill_anchor: Coordinate,
        table_row: Option<&'a TableRowContext>,
    },
}

impl<'a> FormulaInput<'a> {
    const fn coordinate(self) -> Coordinate {
        match self {
            Self::Authored { coordinate, .. } | Self::Virtual { coordinate, .. } => coordinate,
        }
    }

    const fn source_order(self) -> u64 {
        match self {
            Self::Authored { source_order, .. } | Self::Virtual { source_order, .. } => {
                source_order
            }
        }
    }

    const fn source(self) -> &'a FormulaSource {
        match self {
            Self::Authored { source, .. } | Self::Virtual { source, .. } => source,
        }
    }

    const fn origin(self) -> Option<Origin> {
        match self {
            Self::Authored { origin, .. } | Self::Virtual { origin, .. } => origin,
        }
    }

    const fn fill_anchor(self) -> Option<Coordinate> {
        match self {
            Self::Authored { .. } => None,
            Self::Virtual { fill_anchor, .. } => Some(fill_anchor),
        }
    }

    fn table_row(self) -> Option<TableRowContext> {
        match self {
            Self::Authored { .. } => None,
            Self::Virtual { table_row, .. } => table_row.cloned(),
        }
    }

    const fn is_virtual(self) -> bool {
        matches!(self, Self::Virtual { .. })
    }
}

fn formula_inputs(sheet: &PreparedSheet) -> Vec<FormulaInput<'_>> {
    let mut inputs = Vec::new();
    for (&coordinate, authored) in &sheet.authored_cells {
        if let Value::Formula(source) = &authored.cell.value {
            inputs.push(FormulaInput::Authored {
                coordinate,
                source_order: authored.source_order,
                source,
                origin: authored.cell.origin,
            });
        }
    }
    for (&coordinate, virtual_cell) in &sheet.virtual_cells {
        inputs.push(FormulaInput::Virtual {
            coordinate,
            source_order: virtual_cell.source_order,
            source: &virtual_cell.formula,
            origin: virtual_cell.fill_origin,
            fill_anchor: virtual_cell.fill_anchor,
            table_row: virtual_cell.table_row.as_ref(),
        });
    }
    inputs
}

#[allow(clippy::too_many_arguments)]
fn compile_one(
    workbook: &PreparedWorkbook,
    current_sheet: &PreparedSheet,
    cell: CellKey,
    source: FormulaSource,
    origin: Option<Origin>,
    formula: Formula,
    row_context: Option<&TableRowContext>,
    limits: &CompileLimits,
    program_dependencies: u64,
    issues: &mut Vec<CompileIssue>,
) -> Result<CompiledFormula, Box<CompileIssue>> {
    let mut references = Vec::new();
    let mut dependencies = BTreeSet::new();
    let budget = DependencyBudget {
        max_range_cells: limits.max_range_cells,
        max_formula_dependencies: limits.max_dependencies_per_formula,
        program_dependencies,
        max_program_dependencies: limits.max_total_dependencies,
    };
    resolve_expression_references(
        workbook,
        current_sheet,
        &formula.expression,
        row_context,
        &cell,
        origin,
        budget,
        issues,
        &mut references,
        &mut dependencies,
    )?;
    Ok(CompiledFormula {
        cell,
        source,
        origin,
        formula,
        references,
        dependencies,
    })
}

#[derive(Clone, Copy)]
struct DependencyBudget {
    max_range_cells: u64,
    max_formula_dependencies: u64,
    program_dependencies: u64,
    max_program_dependencies: u64,
}

#[allow(clippy::too_many_arguments)]
fn resolve_expression_references(
    workbook: &PreparedWorkbook,
    current_sheet: &PreparedSheet,
    expression: &Expr,
    row_context: Option<&TableRowContext>,
    formula_cell: &CellKey,
    origin: Option<Origin>,
    budget: DependencyBudget,
    issues: &mut Vec<CompileIssue>,
    references: &mut Vec<ResolvedReferenceAt>,
    dependencies: &mut BTreeSet<CellKey>,
) -> Result<(), Box<CompileIssue>> {
    match &expression.kind {
        ExprKind::Literal { .. } => Ok(()),
        ExprKind::Reference { reference } => {
            let resolved = resolve_reference(
                workbook,
                current_sheet,
                reference,
                row_context,
                formula_cell,
                origin,
                expression.span,
                issues,
            );
            add_dependencies(&resolved, dependencies, budget).map_err(|kind| {
                Box::new(CompileIssue {
                    message: resource_limit_message(&kind),
                    kind: CompileIssueKind::ResourceLimit(kind),
                    cell: formula_cell.clone(),
                    origin,
                    formula_span: Some(expression.span),
                    runtime_error: None,
                })
            })?;
            references.push(ResolvedReferenceAt {
                span: expression.span,
                reference: resolved,
            });
            Ok(())
        }
        ExprKind::Unary { operand, .. } => resolve_expression_references(
            workbook,
            current_sheet,
            operand,
            row_context,
            formula_cell,
            origin,
            budget,
            issues,
            references,
            dependencies,
        ),
        ExprKind::Binary { left, right, .. } => {
            resolve_expression_references(
                workbook,
                current_sheet,
                left,
                row_context,
                formula_cell,
                origin,
                budget,
                issues,
                references,
                dependencies,
            )?;
            resolve_expression_references(
                workbook,
                current_sheet,
                right,
                row_context,
                formula_cell,
                origin,
                budget,
                issues,
                references,
                dependencies,
            )
        }
        ExprKind::Call { call } => {
            for argument in &call.arguments {
                resolve_expression_references(
                    workbook,
                    current_sheet,
                    argument,
                    row_context,
                    formula_cell,
                    origin,
                    budget,
                    issues,
                    references,
                    dependencies,
                )?;
            }
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_reference(
    workbook: &PreparedWorkbook,
    current_sheet: &PreparedSheet,
    reference: &Reference,
    row_context: Option<&TableRowContext>,
    formula_cell: &CellKey,
    origin: Option<Origin>,
    span: ByteSpan,
    issues: &mut Vec<CompileIssue>,
) -> ResolvedReference {
    let result = match reference {
        Reference::Cell { sheet, address } => {
            let sheet = sheet.as_ref().unwrap_or(&current_sheet.id);
            require_sheet(workbook, sheet).map(|()| ResolvedReference::Cell {
                cell: CellKey::new(sheet.clone(), address.coordinate),
            })
        }
        Reference::Range(range) => {
            let sheet = range.sheet.as_ref().unwrap_or(&current_sheet.id);
            require_sheet(workbook, sheet).map(|()| ResolvedReference::Range {
                area: ResolvedArea {
                    sheet: sheet.clone(),
                    range: Some(Range::new(range.start.coordinate, range.end.coordinate)),
                },
            })
        }
        Reference::Name { name } => resolve_name(workbook, name),
        Reference::Structured(structured) => resolve_structured(workbook, structured, row_context),
    };

    match result {
        Ok(reference) => reference,
        Err(kind) => {
            let runtime_error = unresolved_runtime_error(&kind);
            issues.push(CompileIssue {
                message: unresolved_message(&kind),
                kind: CompileIssueKind::Unresolved(kind),
                cell: formula_cell.clone(),
                origin,
                formula_span: Some(span),
                runtime_error: Some(runtime_error),
            });
            ResolvedReference::Error {
                error: runtime_error,
            }
        }
    }
}

fn require_sheet(
    workbook: &PreparedWorkbook,
    sheet: &SheetId,
) -> Result<(), UnresolvedReferenceKind> {
    workbook
        .sheet(sheet)
        .map(|_| ())
        .ok_or_else(|| UnresolvedReferenceKind::Sheet(sheet.clone()))
}

fn resolve_name(
    workbook: &PreparedWorkbook,
    name: &NameId,
) -> Result<ResolvedReference, UnresolvedReferenceKind> {
    let resolved = workbook
        .names
        .get(name)
        .ok_or_else(|| UnresolvedReferenceKind::Name(name.clone()))?;
    let area = match &resolved.target {
        NameTarget::Cell(target) => {
            require_sheet(workbook, &target.sheet)?;
            return Ok(ResolvedReference::NamedCell {
                name: name.clone(),
                cell: CellKey::new(target.sheet.clone(), target.coordinate),
            });
        }
        NameTarget::Range(target) => ResolvedArea {
            sheet: target.sheet.clone(),
            range: Some(target.range),
        },
        NameTarget::TableColumn { table, header } => {
            let table_index = workbook
                .table(table)
                .ok_or_else(|| UnresolvedReferenceKind::Table(table.clone()))?;
            if !table_index.headers.contains_key(header) {
                return Err(UnresolvedReferenceKind::TableHeader {
                    table: table.clone(),
                    header: header.clone(),
                });
            }
            ResolvedArea {
                sheet: table_index.sheet.clone(),
                range: table_index.data_column(header),
            }
        }
    };
    Ok(ResolvedReference::Name {
        name: name.clone(),
        area,
    })
}

fn resolve_structured(
    workbook: &PreparedWorkbook,
    reference: &StructuredReference,
    row_context: Option<&TableRowContext>,
) -> Result<ResolvedReference, UnresolvedReferenceKind> {
    match reference {
        StructuredReference::Column { table, header } => {
            let table_index = workbook
                .table(table)
                .ok_or_else(|| UnresolvedReferenceKind::Table(table.clone()))?;
            if !table_index.headers.contains_key(header) {
                return Err(UnresolvedReferenceKind::TableHeader {
                    table: table.clone(),
                    header: header.clone(),
                });
            }
            Ok(ResolvedReference::TableColumn {
                table: table.clone(),
                header: header.clone(),
                area: ResolvedArea {
                    sheet: table_index.sheet.clone(),
                    range: table_index.data_column(header),
                },
            })
        }
        StructuredReference::Region { table, region } => {
            let table_index = workbook
                .table(table)
                .ok_or_else(|| UnresolvedReferenceKind::Table(table.clone()))?;
            let range = match region {
                TableRegion::Headers => Some(Range {
                    start: table_index.footprint.start,
                    end: Coordinate {
                        column: table_index.footprint.end.column,
                        row: table_index.footprint.start.row,
                    },
                }),
                TableRegion::Data => table_index.data_range,
            };
            Ok(ResolvedReference::TableRegion {
                table: table.clone(),
                region: *region,
                area: ResolvedArea {
                    sheet: table_index.sheet.clone(),
                    range,
                },
            })
        }
        StructuredReference::CurrentRow { table, header } => {
            let context = row_context.ok_or(UnresolvedReferenceKind::CurrentRowOutsideTable)?;
            if let Some(qualified) = table {
                if qualified != &context.table {
                    return Err(UnresolvedReferenceKind::CurrentRowWrongTable {
                        expected: context.table.clone(),
                        actual: qualified.clone(),
                    });
                }
            }
            let table_index = workbook
                .table(&context.table)
                .ok_or_else(|| UnresolvedReferenceKind::Table(context.table.clone()))?;
            let header_cell = table_index.headers.get(header).ok_or_else(|| {
                UnresolvedReferenceKind::TableHeader {
                    table: context.table.clone(),
                    header: header.clone(),
                }
            })?;
            let data = table_index
                .data_range
                .ok_or(UnresolvedReferenceKind::CurrentRowOutsideTable)?;
            let row = data
                .start
                .row
                .checked_add(context.data_row_index)
                .filter(|row| *row <= data.end.row)
                .ok_or(UnresolvedReferenceKind::CurrentRowOutsideTable)?;
            Ok(ResolvedReference::CurrentRow {
                table: context.table.clone(),
                header: header.clone(),
                cell: CellKey::new(
                    table_index.sheet.clone(),
                    Coordinate {
                        column: header_cell.column,
                        row,
                    },
                ),
            })
        }
    }
}

fn add_dependencies(
    reference: &ResolvedReference,
    dependencies: &mut BTreeSet<CellKey>,
    budget: DependencyBudget,
) -> Result<(), ResourceLimitKind> {
    if let Some(cell) = reference.cell() {
        return insert_dependency(cell.clone(), dependencies, budget);
    }
    let Some(area) = reference.area() else {
        return Ok(());
    };
    let Some(range) = area.range else {
        return Ok(());
    };
    let actual = range_cell_count(range);
    if actual > u128::from(budget.max_range_cells) {
        return Err(ResourceLimitKind::RangeCells {
            actual,
            limit: budget.max_range_cells,
        });
    }
    let mut row = range.start.row;
    loop {
        let mut column = range.start.column;
        loop {
            insert_dependency(
                CellKey::new(area.sheet.clone(), Coordinate { column, row }),
                dependencies,
                budget,
            )?;
            if column == range.end.column {
                break;
            }
            column = column.checked_add(1).expect("bounded range column");
        }
        if row == range.end.row {
            break;
        }
        row = row.checked_add(1).expect("bounded range row");
    }
    Ok(())
}

fn insert_dependency(
    cell: CellKey,
    dependencies: &mut BTreeSet<CellKey>,
    budget: DependencyBudget,
) -> Result<(), ResourceLimitKind> {
    if dependencies.contains(&cell) {
        return Ok(());
    }
    let current = u64::try_from(dependencies.len()).unwrap_or(u64::MAX);
    let next = current.saturating_add(1);
    if next > budget.max_formula_dependencies {
        return Err(ResourceLimitKind::FormulaDependencies {
            actual_at_least: next,
            limit: budget.max_formula_dependencies,
        });
    }
    let program_total = budget.program_dependencies.saturating_add(next);
    if program_total > budget.max_program_dependencies {
        return Err(ResourceLimitKind::ProgramDependencies {
            actual: program_total,
            limit: budget.max_program_dependencies,
        });
    }
    dependencies.insert(cell);
    Ok(())
}

fn range_cell_count(range: Range) -> u128 {
    u128::from(range.width().unwrap_or(u64::MAX)) * u128::from(range.height().unwrap_or(u64::MAX))
}

fn syntax_issue(cell: &CellKey, origin: Option<Origin>, error: &FormulaError) -> CompileIssue {
    CompileIssue {
        kind: CompileIssueKind::Syntax(error.kind.clone()),
        message: error.message.clone(),
        cell: cell.clone(),
        origin,
        formula_span: Some(error.span),
        runtime_error: None,
    }
}

const fn unresolved_runtime_error(kind: &UnresolvedReferenceKind) -> CellError {
    match kind {
        UnresolvedReferenceKind::Name(_) => CellError::Name,
        _ => CellError::Reference,
    }
}

fn unresolved_message(kind: &UnresolvedReferenceKind) -> String {
    match kind {
        UnresolvedReferenceKind::Sheet(sheet) => format!("unresolved sheet {sheet}"),
        UnresolvedReferenceKind::Name(name) => format!("unresolved workbook name {name}"),
        UnresolvedReferenceKind::Table(table) => format!("unresolved table {table}"),
        UnresolvedReferenceKind::TableHeader { table, header } => {
            format!("unresolved header {header:?} in table {table}")
        }
        UnresolvedReferenceKind::CurrentRowOutsideTable => {
            "current-row reference is outside a table data row".to_owned()
        }
        UnresolvedReferenceKind::CurrentRowWrongTable { expected, actual } => format!(
            "current-row reference names table {actual}, but the formula is in table {expected}"
        ),
        UnresolvedReferenceKind::AdjustedReferenceOutOfBounds => {
            "copied formula reference is outside the coordinate space".to_owned()
        }
    }
}

fn resource_limit_message(kind: &ResourceLimitKind) -> String {
    match kind {
        ResourceLimitKind::RangeCells { actual, limit } => {
            format!("reference contains {actual} cells; the configured range limit is {limit}")
        }
        ResourceLimitKind::FormulaDependencies {
            actual_at_least,
            limit,
        } => format!(
            "formula has at least {actual_at_least} unique dependencies; the configured limit is {limit}"
        ),
        ResourceLimitKind::ProgramDependencies { actual, limit } => {
            format!("program has {actual} dependency edges; the configured limit is {limit}")
        }
    }
}

#[cfg(test)]
mod tests {
    use marksheet_model::{
        Block, Cell, Fill, FillTarget, FormulaSource, Name, Sheet, SheetCoordinate, SheetItem,
        SheetRange, Table, Workbook,
    };

    use super::*;
    use crate::prepare::{PrepareLimits, PreparedWorkbook};

    fn coordinate(value: &str) -> Coordinate {
        Coordinate::parse(value).unwrap()
    }

    fn formula(value: &str) -> FormulaSource {
        FormulaSource::new(value).unwrap()
    }

    fn block(anchor: &str, rows: Vec<Vec<Value>>) -> Block {
        Block::new(
            coordinate(anchor),
            rows.into_iter()
                .map(|row| row.into_iter().map(Cell::new).collect())
                .collect(),
        )
        .unwrap()
    }

    fn sheet(id: &str, items: Vec<SheetItem>) -> Sheet {
        Sheet {
            id: SheetId::parse(id).unwrap(),
            label: id.to_owned(),
            items,
            origin: None,
        }
    }

    fn compile(workbook: &Workbook) -> FormulaProgram {
        let prepared = PreparedWorkbook::build(workbook, PrepareLimits::default()).unwrap();
        compile_formulas(workbook, &prepared, &CompileLimits::default()).unwrap()
    }

    fn key(sheet: &str, cell: &str) -> CellKey {
        CellKey::new(SheetId::parse(sheet).unwrap(), coordinate(cell))
    }

    #[test]
    fn resolves_forward_names_tables_and_cross_sheet_cells() {
        let table_id = TableId::parse("costs").unwrap();
        let name_id = NameId::parse("cost_values").unwrap();
        let workbook = Workbook {
            names: vec![Name {
                id: name_id,
                target: NameTarget::TableColumn {
                    table: table_id.clone(),
                    header: "Cost".to_owned(),
                },
                origin: None,
            }],
            sheets: vec![
                sheet(
                    "summary",
                    vec![SheetItem::Block(block(
                        "A1",
                        vec![vec![Value::Formula(formula(
                            "=SUM(cost_values)+inputs!B2+costs[Cost]",
                        ))]],
                    ))],
                ),
                sheet(
                    "inputs",
                    vec![SheetItem::Table(Table {
                        id: table_id,
                        block: block(
                            "A1",
                            vec![
                                vec![
                                    Value::Text("Cost".to_owned()),
                                    Value::Text("Quantity".to_owned()),
                                ],
                                vec![Value::Number(4.0), Value::Number(5.0)],
                            ],
                        ),
                        origin: None,
                    })],
                ),
            ],
            ..Workbook::default()
        };
        let program = compile(&workbook);
        assert!(program.issues.is_empty());
        assert_eq!(
            program.formula(&key("summary", "A1")).unwrap().dependencies,
            BTreeSet::from([key("inputs", "A2"), key("inputs", "B2"),])
        );
    }

    #[test]
    fn named_cell_is_scalar_and_one_cell_named_range_is_still_range_shaped() {
        let scalar_name = NameId::parse("scalar").unwrap();
        let range_name = NameId::parse("single_range").unwrap();
        let workbook = Workbook {
            names: vec![
                Name {
                    id: scalar_name.clone(),
                    target: NameTarget::Cell(SheetCoordinate {
                        sheet: SheetId::parse("data").unwrap(),
                        coordinate: coordinate("A1"),
                    }),
                    origin: None,
                },
                Name {
                    id: range_name.clone(),
                    target: NameTarget::Range(SheetRange {
                        sheet: SheetId::parse("data").unwrap(),
                        range: Range::single(coordinate("A1")),
                    }),
                    origin: None,
                },
            ],
            sheets: vec![
                sheet(
                    "data",
                    vec![SheetItem::Block(block(
                        "A1",
                        vec![vec![Value::Number(4.0)]],
                    ))],
                ),
                sheet(
                    "summary",
                    vec![SheetItem::Block(block(
                        "A1",
                        vec![vec![
                            Value::Formula(formula("=scalar")),
                            Value::Formula(formula("=single_range")),
                        ]],
                    ))],
                ),
            ],
            ..Workbook::default()
        };

        let program = compile(&workbook);
        let scalar = program.formula(&key("summary", "A1")).unwrap();
        assert_eq!(scalar.dependencies, BTreeSet::from([key("data", "A1")]));
        assert!(matches!(
            scalar.references[0].reference,
            ResolvedReference::NamedCell { ref name, ref cell }
                if name == &scalar_name && cell == &key("data", "A1")
        ));

        let one_cell_range = program.formula(&key("summary", "B1")).unwrap();
        assert_eq!(
            one_cell_range.dependencies,
            BTreeSet::from([key("data", "A1")])
        );
        assert!(matches!(
            one_cell_range.references[0].reference,
            ResolvedReference::Name { ref name, ref area }
                if name == &range_name
                    && area.sheet == SheetId::parse("data").unwrap()
                    && area.range == Some(Range::single(coordinate("A1")))
        ));
    }

    #[test]
    fn resolves_current_row_and_escaped_headers_exactly() {
        let workbook = Workbook {
            sheets: vec![sheet(
                "main",
                vec![SheetItem::Table(Table {
                    id: TableId::parse("costs").unwrap(),
                    block: block(
                        "B2",
                        vec![
                            vec![
                                Value::Text("Cost] USD".to_owned()),
                                Value::Text("Total".to_owned()),
                            ],
                            vec![
                                Value::Number(3.0),
                                Value::Formula(formula("=[@Cost]] USD]")),
                            ],
                        ],
                    ),
                    origin: None,
                })],
            )],
            ..Workbook::default()
        };
        let program = compile(&workbook);
        assert!(program.issues.is_empty());
        let formula = program.formula(&key("main", "C3")).unwrap();
        assert_eq!(formula.dependencies, BTreeSet::from([key("main", "B3")]));
        assert!(matches!(
            formula.references[0].reference,
            ResolvedReference::CurrentRow { ref header, .. } if header == "Cost] USD"
        ));
    }

    #[test]
    fn fill_binding_honors_mixed_absolute_references() {
        let workbook = Workbook {
            sheets: vec![sheet(
                "main",
                vec![
                    SheetItem::Block(block(
                        "C2",
                        vec![
                            vec![Value::Blank, Value::Blank],
                            vec![Value::Blank, Value::Blank],
                        ],
                    )),
                    SheetItem::Fill(Fill {
                        target: FillTarget::Range(Range::parse("C2:D3").unwrap()),
                        formula: formula("=A1+$B1+A$1+$B$1"),
                        origin: None,
                    }),
                ],
            )],
            ..Workbook::default()
        };
        let program = compile(&workbook);
        let bound = program.formula(&key("main", "D3")).unwrap();
        assert_eq!(
            bound.dependencies,
            BTreeSet::from([key("main", "B1"), key("main", "B2")])
        );
    }

    #[test]
    fn fill_adjustment_overflow_compiles_to_a_local_reference_error() {
        let maximum = Coordinate {
            column: u64::MAX,
            row: 1,
        };
        let far_column = maximum.column_name();
        let workbook = Workbook {
            sheets: vec![sheet(
                "main",
                vec![
                    SheetItem::Block(block("B1", vec![vec![Value::Blank, Value::Blank]])),
                    SheetItem::Fill(Fill {
                        target: FillTarget::Range(Range::parse("B1:C1").unwrap()),
                        formula: formula(&format!("={far_column}1")),
                        origin: None,
                    }),
                ],
            )],
            ..Workbook::default()
        };
        let program = compile(&workbook);
        let destination_key = key("main", "C1");
        assert!(!program.is_uncompiled(&destination_key));
        assert!(program.issues.iter().any(|issue| {
            issue.cell == destination_key
                && matches!(
                    issue.kind,
                    CompileIssueKind::Unresolved(
                        UnresolvedReferenceKind::AdjustedReferenceOutOfBounds
                    )
                )
                && issue.runtime_error == Some(CellError::Reference)
        }));
        let bound = program.formula(&destination_key).unwrap();
        assert!(bound.dependencies.is_empty());
        assert!(matches!(
            bound.formula.expression.kind,
            ExprKind::Literal {
                value: Literal::Error(CellError::Reference)
            }
        ));
        assert_eq!(
            program.formula(&key("main", "B1")).unwrap().dependencies,
            BTreeSet::from([CellKey::new(SheetId::parse("main").unwrap(), maximum)])
        );
    }

    #[test]
    fn absent_coordinates_are_valid_blank_dependencies() {
        let workbook = Workbook {
            sheets: vec![sheet(
                "main",
                vec![SheetItem::Block(block(
                    "Z99",
                    vec![vec![Value::Formula(formula("=A1"))]],
                ))],
            )],
            ..Workbook::default()
        };
        let program = compile(&workbook);
        assert!(program.issues.is_empty());
        assert_eq!(
            program.formula(&key("main", "Z99")).unwrap().dependencies,
            BTreeSet::from([key("main", "A1")])
        );
    }

    #[test]
    fn unresolved_references_keep_distinct_runtime_errors() {
        let workbook = Workbook {
            sheets: vec![sheet(
                "main",
                vec![SheetItem::Block(block(
                    "A1",
                    vec![vec![
                        Value::Formula(formula("=missing")),
                        Value::Formula(formula("=other!A1")),
                        Value::Formula(formula("=[@Cost]")),
                    ]],
                ))],
            )],
            ..Workbook::default()
        };
        let program = compile(&workbook);
        assert_eq!(program.issues.len(), 3);
        assert!(
            program
                .issues
                .iter()
                .all(|issue| issue.diagnostic_code() == "MS2103")
        );
        assert_eq!(program.issues[0].runtime_error, Some(CellError::Name));
        assert_eq!(program.issues[1].runtime_error, Some(CellError::Reference));
        assert_eq!(program.issues[2].runtime_error, Some(CellError::Reference));
        assert_eq!(program.formulas.len(), 3);
    }

    #[test]
    fn syntax_and_dependency_limits_are_structured() {
        let workbook = Workbook {
            names: vec![Name {
                id: NameId::parse("many").unwrap(),
                target: NameTarget::Range(SheetRange {
                    sheet: SheetId::parse("main").unwrap(),
                    range: Range::parse("A1:C1").unwrap(),
                }),
                origin: None,
            }],
            sheets: vec![sheet(
                "main",
                vec![SheetItem::Block(block(
                    "D1",
                    vec![vec![
                        Value::Formula(formula("=many")),
                        Value::Formula(formula("=1+")),
                    ]],
                ))],
            )],
            ..Workbook::default()
        };
        let prepared = PreparedWorkbook::build(&workbook, PrepareLimits::default()).unwrap();
        let range_limits = CompileLimits {
            max_range_cells: 2,
            ..CompileLimits::default()
        };
        let program = compile_formulas(&workbook, &prepared, &range_limits).unwrap();
        assert!(program.issues.iter().any(|issue| matches!(
            issue.kind,
            CompileIssueKind::ResourceLimit(ResourceLimitKind::RangeCells { .. })
        )));
        assert!(
            program
                .issues
                .iter()
                .any(|issue| matches!(issue.kind, CompileIssueKind::Syntax(_)))
        );
        assert!(
            program
                .issues
                .iter()
                .all(|issue| issue.diagnostic_code() == "MS2202")
        );

        let dependency_limits = CompileLimits {
            max_range_cells: 3,
            max_dependencies_per_formula: 2,
            ..CompileLimits::default()
        };
        let program = compile_formulas(&workbook, &prepared, &dependency_limits).unwrap();
        assert!(program.issues.iter().any(|issue| matches!(
            issue.kind,
            CompileIssueKind::ResourceLimit(ResourceLimitKind::FormulaDependencies { .. })
        )));
        assert!(program.is_uncompiled(&key("main", "D1")));

        let program_limits = CompileLimits {
            max_range_cells: 3,
            max_dependencies_per_formula: 3,
            max_total_dependencies: 2,
            ..CompileLimits::default()
        };
        let program = compile_formulas(&workbook, &prepared, &program_limits).unwrap();
        assert!(program.issues.iter().any(|issue| matches!(
            issue.kind,
            CompileIssueKind::ResourceLimit(ResourceLimitKind::ProgramDependencies { .. })
        )));
    }

    #[test]
    fn dependency_budgets_stop_expansion_and_range_counts_do_not_overflow() {
        let workbook = Workbook {
            sheets: vec![sheet(
                "main",
                vec![SheetItem::Block(block(
                    "B1",
                    vec![vec![Value::Formula(formula("=A1:A1000000"))]],
                ))],
            )],
            ..Workbook::default()
        };
        let prepared = PreparedWorkbook::build(&workbook, PrepareLimits::default()).unwrap();
        let limits = CompileLimits {
            max_range_cells: 1_000_000,
            max_dependencies_per_formula: 1,
            ..CompileLimits::default()
        };
        let program = compile_formulas(&workbook, &prepared, &limits).unwrap();
        assert!(program.issues.iter().any(|issue| matches!(
            issue.kind,
            CompileIssueKind::ResourceLimit(ResourceLimitKind::FormulaDependencies {
                actual_at_least: 2,
                limit: 1,
            })
        )));
        assert_eq!(program.dependency_edges, 0);

        let maximum = Coordinate {
            column: u64::MAX,
            row: u64::MAX,
        };
        let enormous_formula = format!("=A1:{}{}", maximum.column_name(), maximum.row);
        let workbook = Workbook {
            sheets: vec![sheet(
                "main",
                vec![SheetItem::Block(block(
                    "A1",
                    vec![vec![Value::Formula(formula(&enormous_formula))]],
                ))],
            )],
            ..Workbook::default()
        };
        let prepared = PreparedWorkbook::build(&workbook, PrepareLimits::default()).unwrap();
        let limits = CompileLimits {
            max_range_cells: u64::MAX,
            max_dependencies_per_formula: u64::MAX,
            max_total_dependencies: u64::MAX,
            ..CompileLimits::default()
        };
        let program = compile_formulas(&workbook, &prepared, &limits).unwrap();
        assert!(program.issues.iter().any(|issue| matches!(
            issue.kind,
            CompileIssueKind::ResourceLimit(ResourceLimitKind::RangeCells { actual, .. })
                if actual > u128::from(u64::MAX)
        )));
    }

    #[test]
    fn rejects_an_unsupported_profile_before_compilation() {
        let mut workbook = Workbook {
            sheets: vec![sheet("main", vec![])],
            ..Workbook::default()
        };
        workbook.settings.formula_profile = "excel@1".to_owned();
        let prepared = PreparedWorkbook::build(&workbook, PrepareLimits::default()).unwrap();
        let error = compile_formulas(&workbook, &prepared, &CompileLimits::default()).unwrap_err();
        assert_eq!(error.profile, "excel@1");
    }
}
