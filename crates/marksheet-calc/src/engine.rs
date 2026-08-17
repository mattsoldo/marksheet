//! Engine-neutral workbook calculation and the deterministic reference engine.
//!
//! This module is the integration boundary between Marksheet's source model
//! and any calculation backend.  [`PreparedCalculation`] deliberately keeps
//! its representation private: alternate engines can implement [`CalcEngine`]
//! without exposing their own workbook, graph, or syntax-tree types.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use marksheet_model::{
    ByteSpan, CellError, Coordinate, Diagnostic, DiagnosticCode, DiagnosticContext, LabeledSpan,
    Origin, Range, RelatedDiagnostic, Severity, SheetId, Workbook,
};
use serde::{Deserialize, Serialize};

use crate::eval::{
    CalcValue, EvaluationContext, EvaluationError, EvaluationLimits, EvaluationStats,
    RectangularRange, ResolveError, ResolvedValue, evaluate,
};
use crate::formula::{FORMULA_SYNTAX_DIAGNOSTIC, Reference};
use crate::graph::{CellKey, DependencyGraph, EvaluationStep};
use crate::prepare::{
    CompileLimits, CompiledFormula, FormulaProgram, PrepareError, PrepareLimits, PreparedWorkbook,
    ResolvedArea, ResolvedReference, UNRESOLVED_REFERENCE_DIAGNOSTIC, compile_formulas,
};

/// Stable diagnostic code for a circular dependency component.
pub const FORMULA_CYCLE_DIAGNOSTIC: &str = "MS2303";
/// Stable diagnostic code for calculator resource-limit failures.
pub const CALC_RESOURCE_LIMIT_DIAGNOSTIC: &str = "MS2901";

/// Whole-calculation work bounds that are independent of formula semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkLimits {
    /// Maximum cells retained by the dependency graph, including absent cells
    /// referenced by formulas.
    pub max_graph_nodes: usize,
    /// Maximum invalidation closure accepted by one atomic change set.
    pub max_dirty_cells: usize,
    /// Maximum formula or cycle cells updated by one calculation.
    pub max_evaluated_cells: usize,
    /// Maximum cells returned by one explicit selection.
    pub max_output_cells: u64,
}

impl Default for WorkLimits {
    fn default() -> Self {
        Self {
            max_graph_nodes: 10_000_000,
            max_dirty_cells: 10_000_000,
            max_evaluated_cells: 10_000_000,
            max_output_cells: 1_000_000,
        }
    }
}

/// All limits needed to prepare, compile, and evaluate a workbook.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct CalcLimits {
    pub prepare: PrepareLimits,
    pub compile: CompileLimits,
    pub evaluation: EvaluationLimits,
    pub work: WorkLimits,
}

/// Result of an atomic workbook load.
///
/// Compile issues remain executable spreadsheet errors and therefore do not
/// suppress `calculation`. Preparation, profile, and graph-limit failures do.
#[derive(Debug)]
pub struct PrepareReport {
    pub calculation: Option<PreparedCalculation>,
    pub diagnostics: Vec<Diagnostic>,
}

/// An explicit rectangular result selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CalculationRequest {
    pub sheet: SheetId,
    pub range: Range,
}

impl CalculationRequest {
    #[must_use]
    pub const fn new(sheet: SheetId, range: Range) -> Self {
        Self { sheet, range }
    }
}

/// One selected cell and its typed calculated value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalculatedCell {
    pub cell: CellKey,
    pub value: CalcValue,
}

/// Exact work performed for one calculation request.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CalcStats {
    /// Pending cells considered by this run, including changed literals.
    pub dirty_cells: BTreeSet<CellKey>,
    /// Formula and circular cells successfully computed during this attempt.
    /// On an operational failure these candidate values are not committed.
    pub evaluated_cells: BTreeSet<CellKey>,
    pub dirty_cell_count: usize,
    pub evaluated_cell_count: usize,
    pub evaluation_steps: usize,
    pub range_cells: usize,
    pub text_bytes: usize,
}

/// A row-major selected result plus all persistent and runtime diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalculationResult {
    pub cells: Vec<CalculatedCell>,
    pub diagnostics: Vec<Diagnostic>,
    pub revision: u64,
    pub stats: CalcStats,
}

/// Literal/input overrides applied without modifying source or formulas.
///
/// Formula and structural changes intentionally require a fresh preparation in
/// Milestone 2. Using [`CalcValue`] prevents formula source from crossing this
/// incremental boundary accidentally.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChangeSet {
    pub literal_overrides: BTreeMap<CellKey, CalcValue>,
}

impl ChangeSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, cell: CellKey, value: CalcValue) -> Option<CalcValue> {
        self.literal_overrides.insert(cell, value)
    }

    #[must_use]
    pub fn with(mut self, cell: CellKey, value: CalcValue) -> Self {
        self.set(cell, value);
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.literal_overrides.is_empty()
    }
}

/// The exact invalidation closure produced by an accepted change set.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirtySet {
    pub cells: BTreeSet<CellKey>,
}

impl DirtySet {
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// An incremental change rejected before calculation state was modified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChangeError {
    UnknownSheet(SheetId),
    FormulaRequiresReload(CellKey),
    NonFiniteNumber(CellKey),
    GraphNodeLimitExceeded { actual: usize, limit: usize },
    DirtyCellLimitExceeded { actual: usize, limit: usize },
}

impl fmt::Display for ChangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSheet(sheet) => write!(formatter, "unknown sheet {sheet}"),
            Self::FormulaRequiresReload(cell) => {
                write!(
                    formatter,
                    "formula or structural edit at {cell} requires reload"
                )
            }
            Self::NonFiniteNumber(cell) => {
                write!(formatter, "non-finite numeric override at {cell}")
            }
            Self::GraphNodeLimitExceeded { actual, limit } => write!(
                formatter,
                "change would create {actual} graph nodes; the configured limit is {limit}"
            ),
            Self::DirtyCellLimitExceeded { actual, limit } => write!(
                formatter,
                "change would dirty {actual} cells; the configured limit is {limit}"
            ),
        }
    }
}

impl std::error::Error for ChangeError {}

/// Engine-neutral load/change/evaluate boundary.
pub trait CalcEngine {
    fn prepare(&self, workbook: &Workbook, limits: CalcLimits) -> PrepareReport;

    /// Applies all overrides or none of them.
    ///
    /// # Errors
    ///
    /// Returns [`ChangeError`] without modifying `calculation` when a change
    /// is invalid or its complete dirty closure exceeds configured limits.
    fn apply_changes(
        &self,
        calculation: &mut PreparedCalculation,
        changes: ChangeSet,
    ) -> Result<DirtySet, ChangeError>;

    fn calculate(
        &self,
        calculation: &mut PreparedCalculation,
        request: &CalculationRequest,
    ) -> CalculationResult;
}

/// Deterministic, dependency-aware implementation of `portable-a1@1`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceCalcEngine;

/// Opaque prepared state shared through the engine-neutral API.
#[derive(Debug)]
pub struct PreparedCalculation {
    prepared: PreparedWorkbook,
    program: FormulaProgram,
    graph: DependencyGraph,
    plan: EvaluationPlan,
    values: BTreeMap<CellKey, CalcValue>,
    overrides: BTreeMap<CellKey, CalcValue>,
    dirty: PendingDirty,
    diagnostics: Vec<Diagnostic>,
    limits: CalcLimits,
    revision: u64,
}

/// Cells whose values are still pending recalculation.
///
/// Preparation marks every cell pending, and a caller that only ever
/// calculates part of a workbook leaves the rest pending forever, so this set
/// tracks the workbook rather than the current edit. Every operation here is
/// therefore written to iterate only the cells a caller actually touches; a
/// whole-set scan such as `BTreeSet::union` or `BTreeSet::retain` would make
/// each edit cost as much as the never-calculated remainder of the workbook.
///
/// `visits` records how many pending entries each operation iterated so that
/// tests can assert that property directly instead of timing calculations.
/// Any new operation must record the entries it iterates.
#[derive(Debug, Default)]
struct PendingDirty {
    cells: BTreeSet<CellKey>,
    #[cfg(test)]
    visits: std::cell::Cell<usize>,
}

impl PendingDirty {
    fn new(cells: BTreeSet<CellKey>) -> Self {
        Self {
            cells,
            #[cfg(test)]
            visits: std::cell::Cell::new(0),
        }
    }

    /// Records that `count` pending entries were iterated.
    #[allow(unused_variables, clippy::unused_self)]
    fn visit(&self, count: usize) {
        #[cfg(test)]
        self.visits.set(self.visits.get().saturating_add(count));
    }

    /// Copies the whole set, for callers that explicitly want all of it.
    fn snapshot(&self) -> BTreeSet<CellKey> {
        self.visit(self.cells.len());
        self.cells.clone()
    }

    /// Returns how large the set would become once `added` is merged in.
    ///
    /// Only `added` is iterated, so a limit check costs the change, not the
    /// backlog it is added to.
    fn len_after_adding(&self, added: &BTreeSet<CellKey>) -> usize {
        self.visit(added.len());
        let new = added
            .iter()
            .filter(|cell| !self.cells.contains(cell))
            .count();
        self.cells.len().saturating_add(new)
    }

    fn add_all(&mut self, added: &BTreeSet<CellKey>) {
        self.visit(added.len());
        self.cells.extend(added.iter().cloned());
    }

    /// Returns the pending cells inside `scope`, iterating the smaller side.
    fn within(&self, scope: &BTreeSet<CellKey>) -> BTreeSet<CellKey> {
        if scope.len() <= self.cells.len() {
            self.visit(scope.len());
            return scope
                .iter()
                .filter(|cell| self.cells.contains(cell))
                .cloned()
                .collect();
        }
        self.visit(self.cells.len());
        self.cells
            .iter()
            .filter(|cell| scope.contains(cell))
            .cloned()
            .collect()
    }

    /// Drops `settled` from the pending set, iterating only `settled`.
    fn settle(&mut self, settled: &BTreeSet<CellKey>) {
        self.visit(settled.len());
        for cell in settled {
            self.cells.remove(cell);
        }
    }
}

/// A whole-graph evaluation order retained for one graph revision.
///
/// Ordering the graph costs a strongly-connected-component pass plus a
/// topological pass over every node, so it is done once during preparation,
/// which has to visit the whole workbook anyway, and cached until
/// [`DependencyGraph::revision`] changes. `step_for_cell` then lets one
/// calculation visit only the steps its dirty set reaches instead of scanning
/// the whole workbook order.
///
/// Only registering a cell the workbook never mentioned changes the graph
/// after preparation, so an editing session normally reuses this plan for
/// every calculation.
#[derive(Debug, Default)]
struct EvaluationPlan {
    order: Vec<EvaluationStep>,
    step_for_cell: BTreeMap<CellKey, usize>,
    revision: Option<u64>,
    /// Orderings computed so far, which proves cache reuse in tests.
    #[cfg(test)]
    computations: usize,
}

impl EvaluationPlan {
    /// Rebuilds the plan only when `graph` changed since it was last built.
    fn refresh(&mut self, graph: &DependencyGraph) {
        if self.revision == Some(graph.revision()) {
            return;
        }
        self.order = graph.evaluation_order();
        self.step_for_cell.clear();
        for (index, step) in self.order.iter().enumerate() {
            match step {
                EvaluationStep::Cell(cell) => {
                    self.step_for_cell.insert(cell.clone(), index);
                }
                EvaluationStep::Cycle(component) => {
                    for cell in component {
                        self.step_for_cell.insert(cell.clone(), index);
                    }
                }
            }
        }
        self.revision = Some(graph.revision());
        #[cfg(test)]
        {
            self.computations += 1;
        }
    }

    /// Returns the circular components in key order.
    ///
    /// Ordering already separated every strongly connected component, so
    /// reporting cycles from the plan spares preparation a second such pass.
    /// The key ordering matches [`DependencyGraph::cyclic_components`].
    fn cyclic_components(&self) -> Vec<&BTreeSet<CellKey>> {
        let mut components: Vec<_> = self
            .order
            .iter()
            .filter_map(|step| match step {
                EvaluationStep::Cycle(component) => Some(component),
                EvaluationStep::Cell(_) => None,
            })
            .collect();
        components.sort_by(|left, right| left.first().cmp(&right.first()));
        components
    }

    /// Returns exactly the steps `dirty` reaches, in evaluation order.
    fn dirty_steps(&self, dirty: &BTreeSet<CellKey>) -> Vec<&EvaluationStep> {
        let touched: BTreeSet<usize> = dirty
            .iter()
            .filter_map(|cell| self.step_for_cell.get(cell).copied())
            .collect();
        touched
            .into_iter()
            .map(|index| &self.order[index])
            .collect()
    }
}

impl PreparedCalculation {
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn pending_dirty(&self) -> DirtySet {
        DirtySet {
            cells: self.dirty.snapshot(),
        }
    }
}

impl CalcEngine for ReferenceCalcEngine {
    fn prepare(&self, workbook: &Workbook, limits: CalcLimits) -> PrepareReport {
        let prepared = match PreparedWorkbook::build(workbook, limits.prepare) {
            Ok(prepared) => prepared,
            Err(error) => {
                return PrepareReport {
                    calculation: None,
                    diagnostics: vec![prepare_error_diagnostic(&error)],
                };
            }
        };
        let program = match compile_formulas(workbook, &prepared, &limits.compile) {
            Ok(program) => program,
            Err(error) => {
                return PrepareReport {
                    calculation: None,
                    diagnostics: vec![simple_diagnostic(
                        "MS2104",
                        error.to_string(),
                        error.origin,
                        None,
                        None,
                    )],
                };
            }
        };

        let mut diagnostics = Vec::with_capacity(program.issues.len());
        for issue in &program.issues {
            match issue.to_diagnostic() {
                Ok(diagnostic) => diagnostics.push(diagnostic),
                Err(error) => {
                    return PrepareReport {
                        calculation: None,
                        diagnostics: vec![limit_diagnostic(format!(
                            "calculator produced an invalid diagnostic code: {error}"
                        ))],
                    };
                }
            }
        }
        if !program.uncompiled_cells.is_empty() {
            return PrepareReport {
                calculation: None,
                diagnostics,
            };
        }
        let graph = build_graph(&prepared, &program);
        let graph_nodes = graph.node_count();
        if graph_nodes > limits.work.max_graph_nodes {
            diagnostics.push(limit_diagnostic(format!(
                "dependency graph has {graph_nodes} cells; the configured limit is {}",
                limits.work.max_graph_nodes
            )));
            return PrepareReport {
                calculation: None,
                diagnostics,
            };
        }
        // Preparation is the one place that already visits the whole workbook,
        // so it also orders the graph. Later calculations then cost their own
        // dirty scope instead of paying for the cells they never touch.
        let mut plan = EvaluationPlan::default();
        plan.refresh(&graph);
        for component in plan.cyclic_components() {
            diagnostics.push(cycle_diagnostic(component, &program));
        }

        let values = authored_scalar_values(&prepared);
        let dirty = PendingDirty::new(graph.cells().cloned().collect());

        let calculation = PreparedCalculation {
            prepared,
            program,
            graph,
            plan,
            values,
            overrides: BTreeMap::new(),
            dirty,
            diagnostics: diagnostics.clone(),
            limits,
            revision: 0,
        };
        PrepareReport {
            calculation: Some(calculation),
            diagnostics,
        }
    }

    fn apply_changes(
        &self,
        calculation: &mut PreparedCalculation,
        changes: ChangeSet,
    ) -> Result<DirtySet, ChangeError> {
        if changes.is_empty() {
            return Ok(DirtySet::default());
        }

        for (cell, value) in &changes.literal_overrides {
            if calculation.prepared.sheet(&cell.sheet).is_none() {
                return Err(ChangeError::UnknownSheet(cell.sheet.clone()));
            }
            if calculation.program.formulas.contains_key(cell)
                || calculation.program.uncompiled_cells.contains(cell)
                || calculation
                    .prepared
                    .sheet(&cell.sheet)
                    .and_then(|sheet| sheet.virtual_cell(cell.coordinate))
                    .is_some()
            {
                return Err(ChangeError::FormulaRequiresReload(cell.clone()));
            }
            if matches!(value, CalcValue::Number(number) if !number.is_finite()) {
                return Err(ChangeError::NonFiniteNumber(cell.clone()));
            }
        }
        let effective: BTreeMap<_, _> = changes
            .literal_overrides
            .into_iter()
            .filter(|(cell, value)| {
                !calculation
                    .values
                    .get(cell)
                    .is_some_and(|existing| calc_values_observably_equal(existing, value))
            })
            .collect();
        if effective.is_empty() {
            return Ok(DirtySet::default());
        }

        // Registration of a previously absent input is described rather than
        // performed so that it commits atomically with the override values.
        // An absent cell has no dependents, so registering it cannot change
        // any dirty closure, and both limits are therefore decidable against
        // the current graph.
        let registered = effective
            .keys()
            .filter(|cell| !calculation.graph.contains_cell(cell))
            .count();
        let graph_nodes = calculation.graph.node_count().saturating_add(registered);
        if graph_nodes > calculation.limits.work.max_graph_nodes {
            return Err(ChangeError::GraphNodeLimitExceeded {
                actual: graph_nodes,
                limit: calculation.limits.work.max_graph_nodes,
            });
        }
        let changed_roots: BTreeSet<_> = effective.keys().cloned().collect();
        let changed_dirty = calculation.graph.dirty_closure(changed_roots);
        let pending_dirty = calculation.dirty.len_after_adding(&changed_dirty);
        if pending_dirty > calculation.limits.work.max_dirty_cells {
            return Err(ChangeError::DirtyCellLimitExceeded {
                actual: pending_dirty,
                limit: calculation.limits.work.max_dirty_cells,
            });
        }

        for cell in effective.keys() {
            calculation.graph.ensure_cell(cell.clone());
        }
        for (cell, value) in effective {
            calculation.overrides.insert(cell.clone(), value.clone());
            calculation.values.insert(cell, value);
        }
        calculation.dirty.add_all(&changed_dirty);
        calculation.revision = calculation.revision.wrapping_add(1);
        Ok(DirtySet {
            cells: changed_dirty,
        })
    }

    fn calculate(
        &self,
        calculation: &mut PreparedCalculation,
        request: &CalculationRequest,
    ) -> CalculationResult {
        let mut diagnostics = calculation.diagnostics.clone();
        if let Err(diagnostic) = validate_request(calculation, request) {
            diagnostics.push(*diagnostic);
            return empty_result(calculation, diagnostics);
        }

        let scope = request_dependency_scope(&calculation.graph, request);
        retain_relevant_calculation_diagnostics(&mut diagnostics, &scope);
        let dirty = calculation.dirty.within(&scope);
        calculation.plan.refresh(&calculation.graph);
        let attempt = match evaluate_pending(calculation, &dirty) {
            Ok(attempt) => attempt,
            Err(abort) => {
                let abort = *abort;
                diagnostics.push(abort.diagnostic);
                return CalculationResult {
                    cells: Vec::new(),
                    diagnostics,
                    revision: calculation.revision,
                    stats: calc_stats(dirty, abort.evaluated_cells, abort.evaluation),
                };
            }
        };

        calculation.values.extend(attempt.updates);
        calculation.dirty.settle(&dirty);
        let cells = selected_cells(calculation, request);
        CalculationResult {
            cells,
            diagnostics,
            revision: calculation.revision,
            stats: calc_stats(dirty, attempt.evaluated_cells, attempt.evaluation),
        }
    }
}

/// Compares two calculated values the way an override no-op check must: a
/// change is only a no-op if it cannot be observed. `PartialEq` on
/// `CalcValue::Number` inherits IEEE 754 equality, under which `-0.0 == 0.0`,
/// but the sign is observable downstream (for example `CONCAT` renders `-0`
/// and `0` differently through `canonical_number`). Numbers are therefore
/// compared bitwise; every other variant keeps ordinary equality.
fn calc_values_observably_equal(left: &CalcValue, right: &CalcValue) -> bool {
    match (left, right) {
        (CalcValue::Number(left), CalcValue::Number(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

fn request_dependency_scope(
    graph: &DependencyGraph,
    request: &CalculationRequest,
) -> BTreeSet<CellKey> {
    let mut scope = BTreeSet::new();
    for_each_coordinate(request.range, |coordinate| {
        scope.insert(CellKey::new(request.sheet.clone(), coordinate));
    });
    let mut pending = scope.clone();
    while let Some(cell) = pending.pop_first() {
        for dependency in graph.dependencies_of(&cell) {
            if scope.insert(dependency.clone()) {
                pending.insert(dependency.clone());
            }
        }
    }
    scope
}

fn retain_relevant_calculation_diagnostics(
    diagnostics: &mut Vec<Diagnostic>,
    scope: &BTreeSet<CellKey>,
) {
    diagnostics.retain(|diagnostic| {
        if !matches!(
            diagnostic.code.as_str(),
            UNRESOLVED_REFERENCE_DIAGNOSTIC | FORMULA_SYNTAX_DIAGNOSTIC | FORMULA_CYCLE_DIAGNOSTIC
        ) {
            return true;
        }
        let cell = diagnostic
            .context
            .as_ref()
            .and_then(|context| context.sheet.as_ref().zip(context.cell))
            .map(|(sheet, coordinate)| CellKey::new(sheet.clone(), coordinate));
        cell.as_ref().is_none_or(|cell| scope.contains(cell))
    });
}

struct EvaluationAttempt {
    /// Only the cells this attempt computed, committed as one delta.
    updates: BTreeMap<CellKey, CalcValue>,
    evaluated_cells: BTreeSet<CellKey>,
    evaluation: EvaluationStats,
}

struct EvaluationAbort {
    diagnostic: Diagnostic,
    evaluated_cells: BTreeSet<CellKey>,
    evaluation: EvaluationStats,
}

fn validate_request(
    calculation: &PreparedCalculation,
    request: &CalculationRequest,
) -> Result<(), Box<Diagnostic>> {
    if calculation.prepared.sheet(&request.sheet).is_none() {
        return Err(Box::new(simple_diagnostic(
            "MS2102",
            format!("unknown calculation sheet {}", request.sheet),
            None,
            Some(request.sheet.clone()),
            None,
        )));
    }
    let output_cells = range_cell_count(request.range).ok_or_else(|| {
        Box::new(limit_diagnostic(
            "calculation selection dimensions overflow".to_owned(),
        ))
    })?;
    if output_cells > calculation.limits.work.max_output_cells {
        return Err(Box::new(limit_diagnostic(format!(
            "calculation selection has {output_cells} cells; the configured limit is {}",
            calculation.limits.work.max_output_cells
        ))));
    }
    Ok(())
}

fn evaluate_pending(
    calculation: &PreparedCalculation,
    dirty: &BTreeSet<CellKey>,
) -> Result<EvaluationAttempt, Box<EvaluationAbort>> {
    debug_assert_eq!(
        calculation.plan.revision,
        Some(calculation.graph.revision()),
        "the caller refreshes the evaluation plan before evaluating"
    );
    let steps = calculation.plan.dirty_steps(dirty);
    let planned = planned_evaluated_cells(&steps, &calculation.program, dirty);
    if planned.len() > calculation.limits.work.max_evaluated_cells {
        return Err(Box::new(EvaluationAbort {
            diagnostic: limit_diagnostic(format!(
                "calculation would update {} formula cells; the configured limit is {}",
                planned.len(),
                calculation.limits.work.max_evaluated_cells
            )),
            evaluated_cells: BTreeSet::new(),
            evaluation: EvaluationStats::default(),
        }));
    }

    let mut updates = BTreeMap::new();
    let mut evaluated_cells = BTreeSet::new();
    let mut evaluation = EvaluationStats::default();
    for step in steps {
        match step {
            EvaluationStep::Cycle(component) if !component.is_disjoint(dirty) => {
                for cell in component {
                    updates.insert(cell.clone(), CalcValue::Error(CellError::Circular));
                    evaluated_cells.insert(cell.clone());
                }
            }
            EvaluationStep::Cell(cell) if dirty.contains(cell) => {
                let Some(formula) = calculation.program.formula(cell) else {
                    continue;
                };
                let context = ReferenceContext {
                    formula,
                    prepared: &calculation.prepared,
                    values: &calculation.values,
                    updates: &updates,
                    overrides: &calculation.overrides,
                };
                let outcome = evaluate(&formula.formula, &context, &calculation.limits.evaluation)
                    .map_err(|error| {
                        let mut attempted_evaluation = evaluation;
                        add_evaluation_stats(&mut attempted_evaluation, error.stats());
                        Box::new(EvaluationAbort {
                            diagnostic: evaluation_error_diagnostic(formula, &error),
                            evaluated_cells: evaluated_cells.clone(),
                            evaluation: attempted_evaluation,
                        })
                    })?;
                add_evaluation_stats(&mut evaluation, outcome.stats);
                updates.insert(cell.clone(), outcome.value);
                evaluated_cells.insert(cell.clone());
            }
            EvaluationStep::Cell(_) | EvaluationStep::Cycle(_) => {}
        }
    }
    Ok(EvaluationAttempt {
        updates,
        evaluated_cells,
        evaluation,
    })
}

fn calc_stats(
    dirty_cells: BTreeSet<CellKey>,
    evaluated_cells: BTreeSet<CellKey>,
    evaluation: EvaluationStats,
) -> CalcStats {
    CalcStats {
        dirty_cell_count: dirty_cells.len(),
        evaluated_cell_count: evaluated_cells.len(),
        dirty_cells,
        evaluated_cells,
        evaluation_steps: evaluation.steps,
        range_cells: evaluation.range_cells,
        text_bytes: evaluation.text_bytes,
    }
}

impl ReferenceCalcEngine {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

fn build_graph(prepared: &PreparedWorkbook, program: &FormulaProgram) -> DependencyGraph {
    let mut graph = DependencyGraph::new();
    for sheet in &prepared.sheets {
        for coordinate in sheet
            .authored_cells
            .keys()
            .chain(sheet.virtual_cells.keys())
        {
            graph.ensure_cell(CellKey::new(sheet.id.clone(), *coordinate));
        }
    }
    for cell in &program.uncompiled_cells {
        graph.ensure_cell(cell.clone());
    }
    for (cell, formula) in &program.formulas {
        graph.set_dependencies(cell.clone(), formula.dependencies.iter().cloned());
    }
    graph
}

fn authored_scalar_values(prepared: &PreparedWorkbook) -> BTreeMap<CellKey, CalcValue> {
    let mut values = BTreeMap::new();
    for sheet in &prepared.sheets {
        for (&coordinate, authored) in &sheet.authored_cells {
            if let Ok(value) = CalcValue::try_from(&authored.cell.value) {
                values.insert(CellKey::new(sheet.id.clone(), coordinate), value);
            }
        }
    }
    values
}

fn planned_evaluated_cells(
    steps: &[&EvaluationStep],
    program: &FormulaProgram,
    dirty: &BTreeSet<CellKey>,
) -> BTreeSet<CellKey> {
    let mut planned = BTreeSet::new();
    for step in steps {
        match step {
            EvaluationStep::Cycle(component) if !component.is_disjoint(dirty) => {
                planned.extend(component.iter().cloned());
            }
            EvaluationStep::Cell(cell)
                if dirty.contains(cell) && program.formulas.contains_key(cell) =>
            {
                planned.insert(cell.clone());
            }
            EvaluationStep::Cell(_) | EvaluationStep::Cycle(_) => {}
        }
    }
    planned
}

struct ReferenceContext<'a> {
    formula: &'a CompiledFormula,
    prepared: &'a PreparedWorkbook,
    values: &'a BTreeMap<CellKey, CalcValue>,
    /// Uncommitted results of this attempt, which shadow [`Self::values`].
    updates: &'a BTreeMap<CellKey, CalcValue>,
    overrides: &'a BTreeMap<CellKey, CalcValue>,
}

/// Bounds that cannot reject anything, used by the compatibility
/// [`EvaluationContext::resolve`] entry point. `saturating_add` can never
/// exceed [`usize::MAX`], so no area check can fire under these limits.
const UNBOUNDED_LIMITS: EvaluationLimits = EvaluationLimits {
    max_steps: usize::MAX,
    max_range_cells: usize::MAX,
    max_text_bytes: usize::MAX,
};

impl EvaluationContext for ReferenceContext<'_> {
    /// The published, budget-free entry point. Kept behaviorally identical to
    /// what it was before the eval-time range bound existed: it resolves under
    /// [`UNBOUNDED_LIMITS`], so it can only ever produce a spreadsheet error.
    /// The evaluator itself calls
    /// [`EvaluationContext::resolve_within_limits`] below.
    fn resolve(&self, reference: &Reference, span: ByteSpan) -> Result<ResolvedValue, CellError> {
        match self.resolve_within_limits(
            reference,
            span,
            &UNBOUNDED_LIMITS,
            EvaluationStats::default(),
        ) {
            Ok(value) => Ok(value),
            Err(ResolveError::Cell(error)) => Err(error),
            // Unreachable: no cell count can exceed `usize::MAX`.
            Err(ResolveError::Limit(_)) => Err(CellError::Reference),
        }
    }

    fn resolve_within_limits(
        &self,
        _reference: &Reference,
        span: ByteSpan,
        limits: &EvaluationLimits,
        stats: EvaluationStats,
    ) -> Result<ResolvedValue, ResolveError> {
        let resolved = self
            .formula
            .reference_at(span)
            .ok_or(CellError::Reference)?;
        match resolved {
            ResolvedReference::Cell { cell }
            | ResolvedReference::NamedCell { cell, .. }
            | ResolvedReference::CurrentRow { cell, .. } => {
                Ok(ResolvedValue::Scalar(self.scalar(cell)))
            }
            ResolvedReference::Range { area } => self.area(area, None, limits, stats),
            ResolvedReference::Name { name, area } => {
                let empty_columns = self.prepared.names.get(name).and_then(|resolved| {
                    matches!(
                        &resolved.target,
                        marksheet_model::NameTarget::TableColumn { .. }
                    )
                    .then_some(1)
                });
                self.area(area, empty_columns, limits, stats)
            }
            ResolvedReference::TableColumn { area, .. } => self.area(area, Some(1), limits, stats),
            ResolvedReference::TableRegion {
                table,
                region,
                area,
                ..
            } => {
                let empty_columns = matches!(region, crate::formula::TableRegion::Data)
                    .then_some(())
                    .and_then(|()| {
                        self.prepared
                            .table(table)
                            .and_then(|table| table.footprint.width().ok())
                            .and_then(|width| usize::try_from(width).ok())
                    });
                self.area(area, empty_columns, limits, stats)
            }
            ResolvedReference::Error { error } => Err(ResolveError::Cell(*error)),
        }
    }
}

impl ReferenceContext<'_> {
    fn scalar(&self, cell: &CellKey) -> CalcValue {
        if let Some(value) = self
            .overrides
            .get(cell)
            .or_else(|| self.updates.get(cell))
            .or_else(|| self.values.get(cell))
        {
            return value.clone();
        }
        self.prepared
            .sheet(&cell.sheet)
            .and_then(|sheet| sheet.authored_cell(cell.coordinate))
            .and_then(|authored| CalcValue::try_from(&authored.cell.value).ok())
            .unwrap_or(CalcValue::Blank)
    }

    /// Resolves a rectangular area to its calculated values.
    ///
    /// `limits.max_range_cells` is enforced against `stats.range_cells` plus
    /// this area's own cell count *before* any cell is materialized, so a
    /// single oversized range cannot be fully cloned into memory before the
    /// eval-time limit gets a chance to reject it. This mirrors, and does not
    /// replace, the evaluator's own per-cell accounting as a range's values
    /// are subsequently consumed.
    fn area(
        &self,
        area: &ResolvedArea,
        empty_columns: Option<usize>,
        limits: &EvaluationLimits,
        stats: EvaluationStats,
    ) -> Result<ResolvedValue, ResolveError> {
        let Some(range) = area.range else {
            let columns = empty_columns.unwrap_or(1);
            let range =
                RectangularRange::new(0, columns, Vec::new()).map_err(|_| CellError::Reference)?;
            return Ok(ResolvedValue::Range(range));
        };
        let rows = usize::try_from(range.height().map_err(|_| CellError::Reference)?)
            .map_err(|_| CellError::Reference)?;
        let columns = usize::try_from(range.width().map_err(|_| CellError::Reference)?)
            .map_err(|_| CellError::Reference)?;
        let capacity = rows.checked_mul(columns).ok_or(CellError::Reference)?;
        let projected_range_cells = stats.range_cells.saturating_add(capacity);
        if projected_range_cells > limits.max_range_cells {
            return Err(ResolveError::Limit(
                EvaluationError::RangeCellLimitExceeded {
                    limit: limits.max_range_cells,
                    stats: EvaluationStats {
                        range_cells: projected_range_cells,
                        ..stats
                    },
                },
            ));
        }
        let mut values = Vec::with_capacity(capacity);
        for_each_coordinate(range, |coordinate| {
            values.push(self.scalar(&CellKey::new(area.sheet.clone(), coordinate)));
        });
        RectangularRange::new(rows, columns, values)
            .map(ResolvedValue::Range)
            .map_err(|_| ResolveError::Cell(CellError::Reference))
    }
}

fn selected_cells(
    calculation: &PreparedCalculation,
    request: &CalculationRequest,
) -> Vec<CalculatedCell> {
    let mut selected = Vec::new();
    for_each_coordinate(request.range, |coordinate| {
        let cell = CellKey::new(request.sheet.clone(), coordinate);
        let value = calculation
            .overrides
            .get(&cell)
            .or_else(|| calculation.values.get(&cell))
            .cloned()
            .unwrap_or(CalcValue::Blank);
        selected.push(CalculatedCell { cell, value });
    });
    selected
}

fn for_each_coordinate(range: Range, mut visit: impl FnMut(Coordinate)) {
    let mut row = range.start.row;
    loop {
        let mut column = range.start.column;
        loop {
            visit(Coordinate { column, row });
            if column == range.end.column {
                break;
            }
            column = column.checked_add(1).expect("validated range column");
        }
        if row == range.end.row {
            break;
        }
        row = row.checked_add(1).expect("validated range row");
    }
}

fn range_cell_count(range: Range) -> Option<u64> {
    range.width().ok()?.checked_mul(range.height().ok()?)
}

fn add_evaluation_stats(total: &mut EvaluationStats, next: EvaluationStats) {
    total.steps = total.steps.saturating_add(next.steps);
    total.range_cells = total.range_cells.saturating_add(next.range_cells);
    total.text_bytes = total.text_bytes.saturating_add(next.text_bytes);
}

fn cycle_diagnostic(component: &BTreeSet<CellKey>, program: &FormulaProgram) -> Diagnostic {
    let first = component.first().expect("cycle components are nonempty");
    let first_origin = program.formula(first).and_then(|formula| formula.origin);
    let related = component
        .iter()
        .skip(1)
        .map(|cell| RelatedDiagnostic {
            message: format!("also participates in this cycle: {cell}"),
            span: LabeledSpan {
                span: program
                    .formula(cell)
                    .and_then(|formula| formula.origin)
                    .map_or_else(ByteSpan::default, |origin| origin.span),
                label: Some(cell.to_string()),
            },
        })
        .collect();
    Diagnostic {
        code: diagnostic_code(FORMULA_CYCLE_DIAGNOSTIC),
        severity: Severity::Error,
        message: format!(
            "circular formula dependency: {}",
            component
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" -> ")
        ),
        primary: LabeledSpan {
            span: first_origin.map_or_else(ByteSpan::default, |origin| origin.span),
            label: Some(first.to_string()),
        },
        related,
        context: Some(DiagnosticContext {
            sheet: Some(first.sheet.clone()),
            cell: Some(first.coordinate),
        }),
        suggestion: None,
    }
}

fn prepare_error_diagnostic(error: &PrepareError) -> Diagnostic {
    let (origin, sheet, cell) = prepare_error_location(error);
    simple_diagnostic("MS2201", error.to_string(), origin, sheet, cell)
}

fn prepare_error_location(
    error: &PrepareError,
) -> (Option<Origin>, Option<SheetId>, Option<Coordinate>) {
    match error {
        PrepareError::DuplicateSheet { sheet, origin }
        | PrepareError::OverlappingFootprints {
            sheet,
            second_origin: origin,
            ..
        }
        | PrepareError::MalformedBlock { sheet, origin }
        | PrepareError::UnresolvedSheet { sheet, origin }
        | PrepareError::FillHasNoOwner { sheet, origin, .. }
        | PrepareError::FillHasMultipleOwners { sheet, origin, .. }
        | PrepareError::FillMustFollowOwner { sheet, origin, .. }
        | PrepareError::HeaderOnlyTableFill { sheet, origin, .. }
        | PrepareError::VirtualCellLimitExceeded { sheet, origin, .. } => {
            (*origin, Some(sheet.clone()), None)
        }
        PrepareError::FillTargetsNonBlankCell {
            sheet,
            coordinate,
            origin,
        }
        | PrepareError::FillTargetsAbsentCell {
            sheet,
            coordinate,
            origin,
        }
        | PrepareError::OverlappingFills {
            sheet,
            coordinate,
            second_origin: origin,
            ..
        } => (*origin, Some(sheet.clone()), Some(*coordinate)),
        PrepareError::DuplicateTable { origin, .. }
        | PrepareError::DuplicateName { origin, .. }
        | PrepareError::TableNameConflict { origin, .. }
        | PrepareError::InvalidTableHeader { origin, .. }
        | PrepareError::DuplicateTableHeader { origin, .. }
        | PrepareError::UnresolvedTable { origin, .. }
        | PrepareError::UnresolvedTableHeader { origin, .. }
        | PrepareError::RangeLimitExceeded { origin, .. } => (*origin, None, None),
        PrepareError::Coordinate { .. } | PrepareError::SourceOrderOverflow => (None, None, None),
    }
}

fn evaluation_error_diagnostic(formula: &CompiledFormula, error: &EvaluationError) -> Diagnostic {
    simple_diagnostic(
        CALC_RESOURCE_LIMIT_DIAGNOSTIC,
        format!("{}: {error}", formula.cell),
        formula.origin,
        Some(formula.cell.sheet.clone()),
        Some(formula.cell.coordinate),
    )
}

fn limit_diagnostic(message: String) -> Diagnostic {
    simple_diagnostic(CALC_RESOURCE_LIMIT_DIAGNOSTIC, message, None, None, None)
}

fn simple_diagnostic(
    code: &str,
    message: String,
    origin: Option<Origin>,
    sheet: Option<SheetId>,
    cell: Option<Coordinate>,
) -> Diagnostic {
    Diagnostic {
        code: diagnostic_code(code),
        severity: Severity::Error,
        message,
        primary: LabeledSpan {
            span: origin.map_or_else(ByteSpan::default, |origin| origin.span),
            label: None,
        },
        related: Vec::new(),
        context: (sheet.is_some() || cell.is_some()).then_some(DiagnosticContext { sheet, cell }),
        suggestion: None,
    }
}

fn diagnostic_code(code: &str) -> DiagnosticCode {
    DiagnosticCode::new(code).expect("engine diagnostic constants are valid")
}

fn empty_result(
    calculation: &PreparedCalculation,
    diagnostics: Vec<Diagnostic>,
) -> CalculationResult {
    CalculationResult {
        cells: Vec::new(),
        diagnostics,
        revision: calculation.revision,
        stats: CalcStats::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::{
        Block, Cell, Fill, FillTarget, FormulaSource, Name, NameId, NameTarget, Sheet,
        SheetCoordinate, SheetItem, SheetRange, Table, TableId, Value, WorkbookSettings,
    };

    fn sheet_id(value: &str) -> SheetId {
        SheetId::parse(value).unwrap()
    }

    fn table_id(value: &str) -> TableId {
        TableId::parse(value).unwrap()
    }

    fn coordinate(value: &str) -> Coordinate {
        Coordinate::parse(value).unwrap()
    }

    fn range(value: &str) -> Range {
        Range::parse(value).unwrap()
    }

    fn key(sheet: &str, address: &str) -> CellKey {
        CellKey::new(sheet_id(sheet), coordinate(address))
    }

    fn formula(source: &str) -> Value {
        Value::Formula(FormulaSource::new(source).unwrap())
    }

    fn block(anchor: &str, rows: Vec<Vec<Value>>) -> SheetItem {
        SheetItem::Block(
            Block::new(
                coordinate(anchor),
                rows.into_iter()
                    .map(|row| row.into_iter().map(Cell::new).collect())
                    .collect(),
            )
            .unwrap(),
        )
    }

    fn sheet(id: &str, items: Vec<SheetItem>) -> Sheet {
        Sheet {
            id: sheet_id(id),
            label: id.to_owned(),
            items,
            origin: None,
        }
    }

    fn workbook(sheets: Vec<Sheet>) -> Workbook {
        Workbook {
            settings: WorkbookSettings::default(),
            sheets,
            ..Workbook::default()
        }
    }

    fn prepared(workbook: &Workbook) -> PreparedCalculation {
        let report = ReferenceCalcEngine.prepare(workbook, CalcLimits::default());
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        report.calculation.unwrap()
    }

    fn calculate(
        calculation: &mut PreparedCalculation,
        sheet: &str,
        selected: &str,
    ) -> CalculationResult {
        ReferenceCalcEngine.calculate(
            calculation,
            &CalculationRequest::new(sheet_id(sheet), range(selected)),
        )
    }

    fn values(result: &CalculationResult) -> Vec<CalcValue> {
        result.cells.iter().map(|cell| cell.value.clone()).collect()
    }

    #[test]
    fn chain_and_diamond_calculate_in_dependency_order_and_invalidate_exactly() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![
                    Value::Number(1.0),
                    formula("=A1+1"),
                    formula("=A1+2"),
                    formula("=B1+C1"),
                ]],
            )],
        )]);
        let mut calculation = prepared(&workbook);

        let initial = calculate(&mut calculation, "main", "A1:D1");
        assert_eq!(
            values(&initial),
            vec![
                CalcValue::Number(1.0),
                CalcValue::Number(2.0),
                CalcValue::Number(3.0),
                CalcValue::Number(5.0),
            ]
        );
        assert_eq!(
            initial.stats.evaluated_cells,
            [key("main", "B1"), key("main", "C1"), key("main", "D1")].into()
        );

        let dirty = ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(2.0)),
            )
            .unwrap();
        assert_eq!(
            dirty.cells,
            [
                key("main", "A1"),
                key("main", "B1"),
                key("main", "C1"),
                key("main", "D1"),
            ]
            .into()
        );

        let incremental = calculate(&mut calculation, "main", "A1:D1");
        assert_eq!(
            values(&incremental),
            vec![
                CalcValue::Number(2.0),
                CalcValue::Number(3.0),
                CalcValue::Number(4.0),
                CalcValue::Number(7.0),
            ]
        );
        assert_eq!(incremental.stats.dirty_cells, dirty.cells);
        assert_eq!(incremental.stats.evaluated_cell_count, 3);
    }

    #[test]
    fn cycles_are_reported_once_per_scc_and_propagate_downstream() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![
                    vec![formula("=A1")],
                    vec![formula("=A3")],
                    vec![formula("=A2")],
                    vec![formula("=A2+1")],
                ],
            )],
        )]);
        let report = ReferenceCalcEngine.prepare(&workbook, CalcLimits::default());
        assert_eq!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_str() == FORMULA_CYCLE_DIAGNOSTIC)
                .count(),
            2
        );
        let mut calculation = report.calculation.unwrap();
        let result = calculate(&mut calculation, "main", "A1:A4");
        assert_eq!(
            values(&result),
            vec![CalcValue::Error(CellError::Circular); 4]
        );
    }

    #[test]
    fn cross_sheet_cycle_is_one_component() {
        let workbook = workbook(vec![
            sheet("left", vec![block("A1", vec![vec![formula("=right!A1")]])]),
            sheet("right", vec![block("A1", vec![vec![formula("=left!A1")]])]),
        ]);
        let report = ReferenceCalcEngine.prepare(&workbook, CalcLimits::default());
        assert_eq!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_str() == FORMULA_CYCLE_DIAGNOSTIC)
                .count(),
            1
        );
        let mut calculation = report.calculation.unwrap();
        assert_eq!(
            calculate(&mut calculation, "left", "A1").cells[0].value,
            CalcValue::Error(CellError::Circular)
        );
        assert_eq!(
            calculate(&mut calculation, "right", "A1").cells[0].value,
            CalcValue::Error(CellError::Circular)
        );
    }

    #[test]
    fn independent_formula_islands_are_calculated_on_demand() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![
                    Value::Number(1.0),
                    formula("=A1+1"),
                    Value::Blank,
                    Value::Number(10.0),
                    formula("=D1+1"),
                ]],
            )],
        )]);
        let mut calculation = prepared(&workbook);

        let first = calculate(&mut calculation, "main", "A1:B1");
        assert_eq!(
            values(&first),
            vec![CalcValue::Number(1.0), CalcValue::Number(2.0)]
        );
        assert_eq!(
            first.stats.dirty_cells,
            [key("main", "A1"), key("main", "B1")].into()
        );
        assert_eq!(first.stats.evaluated_cells, [key("main", "B1")].into());
        assert_eq!(
            calculation.pending_dirty().cells,
            [key("main", "C1"), key("main", "D1"), key("main", "E1")].into()
        );

        let second = calculate(&mut calculation, "main", "D1:E1");
        assert_eq!(
            values(&second),
            vec![CalcValue::Number(10.0), CalcValue::Number(11.0)]
        );
        assert_eq!(
            second.stats.dirty_cells,
            [key("main", "D1"), key("main", "E1")].into()
        );
        assert_eq!(second.stats.evaluated_cells, [key("main", "E1")].into());
        assert_eq!(
            calculation.pending_dirty().cells,
            [key("main", "C1")].into()
        );
    }

    #[test]
    fn unrelated_cycle_stays_pending_and_out_of_result_diagnostics() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![
                    Value::Number(1.0),
                    formula("=A1+1"),
                    Value::Blank,
                    formula("=D1"),
                ]],
            )],
        )]);
        let report = ReferenceCalcEngine.prepare(&workbook, CalcLimits::default());
        assert_eq!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_str() == FORMULA_CYCLE_DIAGNOSTIC)
                .count(),
            1
        );
        let mut calculation = report.calculation.unwrap();

        let independent = calculate(&mut calculation, "main", "A1:B1");
        assert_eq!(
            values(&independent),
            vec![CalcValue::Number(1.0), CalcValue::Number(2.0)]
        );
        assert!(
            independent
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code.as_str() != FORMULA_CYCLE_DIAGNOSTIC)
        );
        assert_eq!(
            independent.stats.dirty_cells,
            [key("main", "A1"), key("main", "B1")].into()
        );
        assert_eq!(
            calculation.pending_dirty().cells,
            [key("main", "C1"), key("main", "D1")].into()
        );

        let circular = calculate(&mut calculation, "main", "D1");
        assert_eq!(
            circular.cells[0].value,
            CalcValue::Error(CellError::Circular)
        );
        assert_eq!(circular.stats.dirty_cells, [key("main", "D1")].into());
        assert_eq!(circular.stats.evaluated_cells, [key("main", "D1")].into());
        assert_eq!(
            circular
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_str() == FORMULA_CYCLE_DIAGNOSTIC)
                .count(),
            1
        );
        assert_eq!(
            calculation.pending_dirty().cells,
            [key("main", "C1")].into()
        );
    }

    #[test]
    fn unresolved_reference_diagnostic_is_scoped_to_its_formula_island() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![
                    Value::Number(1.0),
                    formula("=A1+1"),
                    Value::Blank,
                    formula("=missing_name"),
                ]],
            )],
        )]);
        let report = ReferenceCalcEngine.prepare(&workbook, CalcLimits::default());
        assert_eq!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code.as_str() == UNRESOLVED_REFERENCE_DIAGNOSTIC
                })
                .count(),
            1
        );
        let mut calculation = report
            .calculation
            .expect("an unresolved reference remains an executable typed error");

        let independent = calculate(&mut calculation, "main", "A1:B1");
        assert_eq!(
            values(&independent),
            vec![CalcValue::Number(1.0), CalcValue::Number(2.0)]
        );
        assert!(
            independent
                .diagnostics
                .iter()
                .all(|diagnostic| { diagnostic.code.as_str() != UNRESOLVED_REFERENCE_DIAGNOSTIC })
        );
        assert_eq!(
            calculation.pending_dirty().cells,
            [key("main", "C1"), key("main", "D1")].into()
        );

        let unresolved = calculate(&mut calculation, "main", "D1");
        assert_eq!(unresolved.cells[0].value, CalcValue::Error(CellError::Name));
        assert_eq!(
            unresolved
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code.as_str() == UNRESOLVED_REFERENCE_DIAGNOSTIC
                })
                .count(),
            1
        );
        assert_eq!(unresolved.stats.dirty_cells, [key("main", "D1")].into());
        assert_eq!(unresolved.stats.evaluated_cells, [key("main", "D1")].into());
        assert_eq!(
            calculation.pending_dirty().cells,
            [key("main", "C1")].into()
        );
    }

    #[test]
    fn virtual_fills_names_tables_and_current_row_references_calculate() {
        let inventory = table_id("inventory");
        let table = Table {
            id: inventory.clone(),
            block: Block::new(
                coordinate("A1"),
                vec![
                    vec!["Item", "Price", "Qty", "Total"],
                    vec!["Pens", "2", "10", ""],
                    vec!["Paper", "5", "3", ""],
                ]
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| Cell::new(Value::from_csv_field(value)))
                        .collect()
                })
                .collect(),
            )
            .unwrap(),
            origin: None,
        };
        let fill = Fill {
            target: FillTarget::TableColumn {
                table: inventory.clone(),
                header: "Total".to_owned(),
            },
            formula: FormulaSource::new("=[@Price]*[@Qty]").unwrap(),
            origin: None,
        };
        let mut workbook = workbook(vec![
            sheet("data", vec![SheetItem::Table(table), SheetItem::Fill(fill)]),
            sheet(
                "summary",
                vec![block(
                    "A1",
                    vec![
                        vec![formula("=SUM(inventory[Total])")],
                        vec![formula("=AVERAGE(prices)")],
                        vec![formula("=COUNTA(inventory[#Headers])")],
                        vec![formula("=INDEX(inventory[Item],2)")],
                    ],
                )],
            ),
        ]);
        workbook.names.push(Name {
            id: NameId::parse("prices").unwrap(),
            target: NameTarget::TableColumn {
                table: inventory,
                header: "Price".to_owned(),
            },
            origin: None,
        });
        let mut calculation = prepared(&workbook);
        assert_eq!(
            values(&calculate(&mut calculation, "data", "D2:D3")),
            vec![CalcValue::Number(20.0), CalcValue::Number(15.0)]
        );
        assert_eq!(
            values(&calculate(&mut calculation, "summary", "A1:A4")),
            vec![
                CalcValue::Number(35.0),
                CalcValue::Number(3.5),
                CalcValue::Number(4.0),
                CalcValue::Text("Paper".to_owned()),
            ]
        );
        assert_eq!(calculation.prepared.sheets[0].virtual_cells.len(), 2);
    }

    #[test]
    fn empty_table_data_retains_shape_and_blank_is_not_empty_text() {
        let empty = table_id("empty");
        let table = Table {
            id: empty,
            block: Block::new(
                coordinate("A1"),
                vec![vec![
                    Cell::new(Value::Text("Item".to_owned())),
                    Cell::new(Value::Text("Price".to_owned())),
                ]],
            )
            .unwrap(),
            origin: None,
        };
        let workbook = workbook(vec![
            sheet("data", vec![SheetItem::Table(table)]),
            sheet(
                "summary",
                vec![block(
                    "A1",
                    vec![vec![
                        formula("=SUM(empty[#Data])"),
                        Value::Blank,
                        Value::Text(String::new()),
                    ]],
                )],
            ),
        ]);
        let mut calculation = prepared(&workbook);
        assert_eq!(
            values(&calculate(&mut calculation, "summary", "A1:C1")),
            vec![
                CalcValue::Number(0.0),
                CalcValue::Blank,
                CalcValue::Text(String::new()),
            ]
        );
    }

    #[test]
    fn selection_is_row_major_including_absent_cells() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![
                    vec![Value::Number(1.0), Value::Number(2.0)],
                    vec![Value::Number(3.0), Value::Number(4.0)],
                ],
            )],
        )]);
        let mut calculation = prepared(&workbook);
        let result = calculate(&mut calculation, "main", "A1:C2");
        assert_eq!(
            result
                .cells
                .iter()
                .map(|cell| cell.cell.coordinate)
                .collect::<Vec<_>>(),
            ["A1", "B1", "C1", "A2", "B2", "C2"]
                .map(coordinate)
                .to_vec()
        );
        assert_eq!(result.cells[2].value, CalcValue::Blank);
        assert_eq!(result.cells[5].value, CalcValue::Blank);
    }

    #[test]
    fn preparation_and_output_limits_fail_without_partial_results() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![Value::Number(1.0), Value::Number(2.0)]],
            )],
        )]);
        let mut limits = CalcLimits::default();
        limits.work.max_graph_nodes = 1;
        let report = ReferenceCalcEngine.prepare(&workbook, limits);
        assert!(report.calculation.is_none());
        assert_eq!(
            report.diagnostics[0].code.as_str(),
            CALC_RESOURCE_LIMIT_DIAGNOSTIC
        );

        let mut limits = CalcLimits::default();
        limits.work.max_output_cells = 1;
        let report = ReferenceCalcEngine.prepare(&workbook, limits);
        let mut calculation = report.calculation.unwrap();
        let result = calculate(&mut calculation, "main", "A1:B1");
        assert!(result.cells.is_empty());
        assert!(!calculation.pending_dirty().is_empty());
    }

    #[test]
    fn evaluation_limit_is_atomic_and_retryable() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![
                    Value::Number(1.0),
                    formula("=A1+1"),
                    formula("=B1+1+1"),
                ]],
            )],
        )]);
        let mut calculation = prepared(&workbook);
        assert_eq!(
            values(&calculate(&mut calculation, "main", "A1:C1")),
            vec![
                CalcValue::Number(1.0),
                CalcValue::Number(2.0),
                CalcValue::Number(4.0),
            ]
        );
        ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(2.0)),
            )
            .unwrap();
        calculation.limits.evaluation.max_steps = 3;
        let before = calculation.pending_dirty();
        let result = calculate(&mut calculation, "main", "A1:C1");
        assert!(result.cells.is_empty());
        assert_eq!(result.stats.evaluated_cells, [key("main", "B1")].into());
        assert_eq!(
            result.stats.evaluation_steps, 7,
            "stats include the failing formula's four attempted steps"
        );
        assert_eq!(calculation.pending_dirty(), before);
        assert_eq!(
            calculation.values.get(&key("main", "B1")),
            Some(&CalcValue::Number(2.0)),
            "a successful candidate must not leak through a later failure"
        );
        assert_eq!(
            result.diagnostics.last().unwrap().code.as_str(),
            CALC_RESOURCE_LIMIT_DIAGNOSTIC
        );

        calculation.limits.evaluation.max_steps = 10;
        assert_eq!(
            values(&calculate(&mut calculation, "main", "A1:C1")),
            vec![
                CalcValue::Number(2.0),
                CalcValue::Number(3.0),
                CalcValue::Number(5.0),
            ]
        );
    }

    #[test]
    fn range_cell_limit_is_enforced_before_materializing_the_range() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![
                    vec![Value::Number(1.0)],
                    vec![Value::Number(2.0)],
                    vec![Value::Number(3.0)],
                    vec![formula("=SUM(A1:A3)")],
                ],
            )],
        )]);
        let mut calculation = prepared(&workbook);
        // Smaller than the 3-cell range A1:A3 that A4 references.
        calculation.limits.evaluation.max_range_cells = 2;
        let before = calculation.pending_dirty();

        let result = calculate(&mut calculation, "main", "A4");
        assert!(
            result.cells.is_empty(),
            "the oversized range must be denied outright, not partially evaluated"
        );
        assert_eq!(
            result.stats.range_cells, 3,
            "the whole range is accounted for atomically before any cell is consumed, \
             not stopped partway through consumption at the configured limit"
        );
        assert_eq!(
            result.diagnostics.last().unwrap().code.as_str(),
            CALC_RESOURCE_LIMIT_DIAGNOSTIC
        );
        assert_eq!(
            calculation.pending_dirty(),
            before,
            "a denied evaluation must not commit any partial state"
        );

        calculation.limits.evaluation.max_range_cells = 3;
        assert_eq!(
            values(&calculate(&mut calculation, "main", "A4")),
            vec![CalcValue::Number(6.0)]
        );
    }

    #[test]
    fn rejected_change_is_atomic() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![Value::Number(1.0), formula("=A1+1"), formula("=B1+1")]],
            )],
        )]);
        let mut calculation = prepared(&workbook);
        calculate(&mut calculation, "main", "A1:C1");
        calculation.limits.work.max_dirty_cells = 2;
        let revision = calculation.revision();
        let error = ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(9.0)),
            )
            .unwrap_err();
        assert!(matches!(error, ChangeError::DirtyCellLimitExceeded { .. }));
        assert_eq!(calculation.revision(), revision);
        assert_eq!(
            calculate(&mut calculation, "main", "A1:C1").cells[0].value,
            CalcValue::Number(1.0)
        );
    }

    #[test]
    fn formula_overrides_require_reload() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block("A1", vec![vec![formula("=1")]])],
        )]);
        let mut calculation = prepared(&workbook);
        let error = ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(2.0)),
            )
            .unwrap_err();
        assert_eq!(error, ChangeError::FormulaRequiresReload(key("main", "A1")));
    }

    #[test]
    fn unchanged_literal_override_is_a_no_op() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block("A1", vec![vec![Value::Number(1.0)]])],
        )]);
        let mut calculation = prepared(&workbook);
        calculate(&mut calculation, "main", "A1");
        let dirty = ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(1.0)),
            )
            .unwrap();
        assert!(dirty.is_empty());
        assert_eq!(calculation.revision(), 0);
        assert!(calculation.pending_dirty().is_empty());
    }

    #[test]
    fn evaluation_order_is_computed_once_per_graph_revision() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![Value::Number(1.0), formula("=A1+1"), formula("=B1+1")]],
            )],
        )]);
        let mut calculation = prepared(&workbook);
        assert_eq!(
            calculation.plan.computations, 1,
            "preparation orders the graph once"
        );

        calculate(&mut calculation, "main", "A1:C1");
        assert_eq!(
            calculation.plan.computations, 1,
            "one calculation orders the graph at most once"
        );
        calculate(&mut calculation, "main", "A1:C1");
        assert_eq!(
            calculation.plan.computations, 1,
            "an unchanged graph revision reuses the cached order"
        );

        ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(2.0)),
            )
            .unwrap();
        assert_eq!(
            values(&calculate(&mut calculation, "main", "A1:C1")),
            vec![
                CalcValue::Number(2.0),
                CalcValue::Number(3.0),
                CalcValue::Number(4.0),
            ]
        );
        assert_eq!(
            calculation.plan.computations, 1,
            "overriding a known literal leaves the graph, and its order, intact"
        );

        ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "Z9"), CalcValue::Number(5.0)),
            )
            .unwrap();
        calculate(&mut calculation, "main", "A1:C1");
        assert_eq!(
            calculation.plan.computations, 2,
            "registering a previously absent cell invalidates the cached order"
        );
    }

    /// Edits `A1` of a `rows`-tall workbook of independent `=A{row}+1` pairs
    /// and recalculates only `B1`, returning the pending entries visited.
    ///
    /// Nothing warms the calculation up first, so every row below the first
    /// stays pending: this is the shape a viewport-sized recalculation has.
    fn viewport_edit_dirty_visits(rows: u32) -> usize {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                (1..=rows)
                    .map(|row| {
                        vec![
                            Value::Number(f64::from(row)),
                            formula(&format!("=A{row}+1")),
                        ]
                    })
                    .collect(),
            )],
        )]);
        let mut calculation = prepared(&workbook);

        calculation.dirty.visits.set(0);
        ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(100.0)),
            )
            .unwrap();
        let result = calculate(&mut calculation, "main", "B1");
        assert_eq!(values(&result), vec![CalcValue::Number(101.0)]);
        calculation.dirty.visits.get()
    }

    #[test]
    fn pending_dirty_bookkeeping_visits_only_the_changed_scope() {
        let small = viewport_edit_dirty_visits(64);
        let large = viewport_edit_dirty_visits(1024);

        assert_eq!(
            small, large,
            "a 16x larger workbook must not make an unchanged two-cell edit cost more"
        );
        assert!(
            small <= 8,
            "the four bookkeeping steps each visit at most the two cells this edit \
             dirties or the two cells it requests, got {small}"
        );
    }

    #[test]
    fn viewport_sized_calculations_match_always_calculating_everything() {
        // A chain, a cycle, and an island, so partial viewports leave a mixed
        // backlog pending between edits.
        let source = workbook(vec![sheet(
            "main",
            vec![
                block(
                    "A1",
                    vec![
                        vec![Value::Number(1.0), formula("=A1+1"), formula("=B1*2")],
                        vec![Value::Number(2.0), formula("=A2+B1"), formula("=C1+B2")],
                        vec![Value::Number(3.0), formula("=A3+C2"), formula("=B3+1")],
                    ],
                ),
                block("E1", vec![vec![formula("=F1"), formula("=E1")]]),
                block("A5", vec![vec![Value::Number(9.0), formula("=A5*3")]]),
            ],
        )]);
        let prepare_cyclic = || {
            ReferenceCalcEngine
                .prepare(&source, CalcLimits::default())
                .calculation
                .unwrap()
        };
        // One calculation only ever sees the requested viewport; the other
        // always recalculates everything and is the ground truth.
        let mut viewport_only = prepare_cyclic();
        let mut always_full = prepare_cyclic();

        let viewports = ["B1:B1", "C2:C2", "A5:B5", "B3:C3", "E1:F1", "A1:C3"];
        let edits = [("A1", 10.0), ("A3", 7.0), ("A5", 4.0), ("A2", -1.0)];
        for (step, viewport) in viewports.iter().cycle().take(24).enumerate() {
            let (cell, value) = edits[step % edits.len()];
            let change = ChangeSet::new().with(
                key("main", cell),
                CalcValue::Number(value + f64::from(u32::try_from(step).unwrap())),
            );
            ReferenceCalcEngine
                .apply_changes(&mut viewport_only, change.clone())
                .unwrap();
            ReferenceCalcEngine
                .apply_changes(&mut always_full, change)
                .unwrap();

            let partial = calculate(&mut viewport_only, "main", viewport);
            calculate(&mut always_full, "main", "A1:F6");
            let complete = calculate(&mut always_full, "main", viewport);
            assert_eq!(
                values(&partial),
                values(&complete),
                "step {step} {viewport}"
            );
        }

        assert_eq!(
            values(&calculate(&mut viewport_only, "main", "A1:F6")),
            values(&calculate(&mut always_full, "main", "A1:F6"))
        );
        assert!(viewport_only.pending_dirty().is_empty());
    }

    #[test]
    fn overriding_zero_with_negative_zero_takes_effect_in_both_directions() {
        let workbook = workbook(vec![sheet(
            "main",
            vec![block(
                "A1",
                vec![vec![Value::Number(0.0), formula("=CONCAT(A1)")]],
            )],
        )]);
        let mut calculation = prepared(&workbook);
        assert_eq!(
            calculate(&mut calculation, "main", "B1").cells[0].value,
            CalcValue::Text("0".to_owned())
        );

        // A sign-only change is observable through CONCAT's canonical
        // rendering, so it must not be filtered out as a no-op.
        let dirty = ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(-0.0)),
            )
            .unwrap();
        assert!(!dirty.is_empty(), "0 -> -0 must not be treated as a no-op");
        assert_eq!(
            calculate(&mut calculation, "main", "B1").cells[0].value,
            CalcValue::Text("-0".to_owned())
        );

        let dirty = ReferenceCalcEngine
            .apply_changes(
                &mut calculation,
                ChangeSet::new().with(key("main", "A1"), CalcValue::Number(0.0)),
            )
            .unwrap();
        assert!(!dirty.is_empty(), "-0 -> 0 must not be treated as a no-op");
        assert_eq!(
            calculate(&mut calculation, "main", "B1").cells[0].value,
            CalcValue::Text("0".to_owned())
        );
    }

    #[test]
    fn explicit_range_names_work_across_sheets() {
        let mut workbook = workbook(vec![
            sheet(
                "data",
                vec![block(
                    "A1",
                    vec![vec![Value::Number(2.0)], vec![Value::Number(3.0)]],
                )],
            ),
            sheet(
                "summary",
                vec![block("A1", vec![vec![formula("=SUM(inputs)")]])],
            ),
        ]);
        workbook.names.push(Name {
            id: NameId::parse("inputs").unwrap(),
            target: NameTarget::Range(SheetRange {
                sheet: sheet_id("data"),
                range: range("A1:A2"),
            }),
            origin: None,
        });
        let mut calculation = prepared(&workbook);
        assert_eq!(
            calculate(&mut calculation, "summary", "A1").cells[0].value,
            CalcValue::Number(5.0)
        );
    }

    #[test]
    fn named_cell_is_scalar_but_one_cell_named_range_stays_a_range() {
        let mut workbook = workbook(vec![
            sheet("data", vec![block("A1", vec![vec![Value::Number(4.0)]])]),
            sheet(
                "summary",
                vec![block(
                    "A1",
                    vec![vec![
                        formula("=scalar+1"),
                        formula("=single_range+1"),
                        formula("=SUM(single_range)"),
                    ]],
                )],
            ),
        ]);
        workbook.names.extend([
            Name {
                id: NameId::parse("scalar").unwrap(),
                target: NameTarget::Cell(SheetCoordinate {
                    sheet: sheet_id("data"),
                    coordinate: coordinate("A1"),
                }),
                origin: None,
            },
            Name {
                id: NameId::parse("single_range").unwrap(),
                target: NameTarget::Range(SheetRange {
                    sheet: sheet_id("data"),
                    range: range("A1:A1"),
                }),
                origin: None,
            },
        ]);

        let mut calculation = prepared(&workbook);
        assert_eq!(
            values(&calculate(&mut calculation, "summary", "A1:C1")),
            vec![
                CalcValue::Number(5.0),
                CalcValue::Error(CellError::Value),
                CalcValue::Number(4.0),
            ]
        );
    }
}
