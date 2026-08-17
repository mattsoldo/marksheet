//! Sparse, renderer-neutral presentation projections for Marksheet workbooks.
//!
//! This crate is deliberately not a UI toolkit. It turns an authored workbook
//! into bounded viewport data that a native, web, or terminal renderer can
//! consume without allocating a sheet's potentially enormous coordinate extent.
//! All collections are proportional to the request or authored sparse content,
//! never to the distance between two populated cells.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use marksheet_calc::eval::CalcValue;
use marksheet_calc::prepare::PreparedSheet;
use marksheet_calc::{
    CalcEngine, CalcLimits, CalculationRequest, PrepareError, PreparedCalculation,
    PreparedWorkbook, ReferenceCalcEngine,
};
use marksheet_model::{
    ApplyTarget, ByteSpan, Coordinate, Diagnostic, Origin, Range, Sheet, SheetId, SheetItem,
    StyleId, StyleProperties, TableRegion, Value, Workbook,
};
use marksheet_syntax::{ParsedDocument, SourceMap};
use serde::{Deserialize, Serialize};

/// Bounded resource settings for a [`WorkbookView`].
///
/// The viewport limits are deliberately independent of the calculator's
/// global preparation limits. A user can open a sheet with far-away cells,
/// while every individual draw remains small and predictable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewLimits {
    /// Maximum number of coordinates requested in one rectangular viewport.
    pub max_viewport_cells: u64,
    /// Maximum sparse cells emitted in one viewport result.
    pub max_presented_cells: usize,
    /// Maximum style intervals returned for one viewport, including intervals
    /// that cover blank cells.
    pub max_style_regions: usize,
    /// Maximum ordered style applications resolved for one presented cell.
    pub max_style_layers_per_cell: usize,
    /// Maximum pre-indexed `@apply` directives examined while projecting one
    /// viewport.
    ///
    /// Each sheet keeps a per-axis interval index over its `@apply` targets,
    /// so a request only examines the applications whose row or column band
    /// reaches the requested range. Directives elsewhere on the sheet are
    /// pruned without being examined and do not count against this limit: a
    /// sheet may hold far more applications than this, and only a viewport
    /// that would actually have to look at more than this many is refused.
    pub max_style_applications: usize,
    /// Bounds used when indexing fills and calculating viewport values.
    pub calculation: CalcLimits,
}

impl Default for ViewLimits {
    fn default() -> Self {
        Self {
            max_viewport_cells: 20_000,
            max_presented_cells: 20_000,
            max_style_regions: 20_000,
            max_style_layers_per_cell: 1_000,
            max_style_applications: 1_024,
            calculation: CalcLimits::default(),
        }
    }
}

/// A declarative, bounded rectangular region to project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VisibleRegionRequest {
    pub sheet: SheetId,
    pub range: Range,
    /// Requests deterministic formula values in addition to authored values.
    pub calculate: bool,
}

impl VisibleRegionRequest {
    #[must_use]
    pub const fn new(sheet: SheetId, range: Range) -> Self {
        Self {
            sheet,
            range,
            calculate: true,
        }
    }
}

/// A workbook overview retaining the declared sheet order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkbookSummary {
    pub sheets: Vec<SheetSummary>,
    /// Whether this projection can make complete core calculation and
    /// rendering claims for the parsed source.
    pub completeness: ViewCompleteness,
    pub diagnostics: Vec<Diagnostic>,
}

/// Explicit capability status for a recovered workbook projection.
///
/// A parser can recover a semantic workbook from invalid source. In that case
/// renderers may still present the authored core content, but must not mistake
/// that recovery for a complete result or use calculated values derived from
/// an invalid document.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ViewCompleteness {
    /// `false` means calculated values were not prepared and must not be
    /// inferred from the projection.
    pub calculation_complete: bool,
    /// `false` means the renderer only received a recoverable core projection.
    pub rendering_complete: bool,
}

impl ViewCompleteness {
    /// Complete status for a trusted semantic workbook or an error-free parse.
    pub const COMPLETE: Self = Self {
        calculation_complete: true,
        rendering_complete: true,
    };

    /// Recovered status for a parsed document containing an error diagnostic.
    pub const RECOVERED_INCOMPLETE: Self = Self {
        calculation_complete: false,
        rendering_complete: false,
    };
}

/// Renderer-friendly facts about one declared sheet.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SheetSummary {
    pub id: SheetId,
    pub label: String,
    pub source_span: Option<ByteSpan>,
    /// Counts only physical CSV fields, including authored blank fields.
    pub authored_cell_count: usize,
    /// Counts finite fill destinations that have not been materialized into source.
    pub virtual_cell_count: usize,
    pub footprint_count: usize,
}

/// A sparse viewport projection. `cells` contains no synthetic empty cells.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisibleRegion {
    pub sheet: SheetSummary,
    pub range: Range,
    /// Completeness of this projection's source document.
    pub completeness: ViewCompleteness,
    /// Sparse cells sorted by coordinate.
    pub cells: Vec<PresentedCell>,
    /// Sparse `@apply` intersections in source order. Renderers apply these
    /// layers even when their target contains no authored or virtual cell.
    pub style_regions: Vec<StyledRegion>,
    /// One bounded entry per requested column, in coordinate order.
    pub columns: Vec<ColumnPresentation>,
    /// One bounded entry per requested row, in coordinate order.
    pub rows: Vec<RowPresentation>,
    pub diagnostics: Vec<Diagnostic>,
}

/// One `@apply` target clipped to a visible viewport.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StyledRegion {
    pub range: Range,
    pub style: ResolvedStyle,
    /// Stable sheet-item order; later applications override earlier ones.
    pub source_order: u64,
}

/// One authored or fill-derived cell visible in a viewport.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresentedCell {
    pub coordinate: Coordinate,
    pub source: CellSource,
    /// `None` when calculation was not requested or could not be prepared.
    pub calculated: Option<CalcValue>,
    pub style: ResolvedStyle,
    pub column: AxisGeometry,
    pub row: AxisGeometry,
}

/// Whether a cell came from physical source or a virtual fill destination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CellSource {
    Authored {
        value: Value,
        source_span: Option<ByteSpan>,
    },
    VirtualFill {
        formula: marksheet_model::FormulaSource,
        /// The source span of the responsible `@fill` directive.
        fill_source_span: Option<ByteSpan>,
        /// The first generated coordinate for this fill directive.
        fill_anchor: Coordinate,
    },
}

/// A style after property-wise, source-order precedence has been resolved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolvedStyle {
    pub properties: StyleProperties,
    /// Every contributing style in application order. The final layer is not
    /// necessarily the source of every resolved property.
    pub layers: Vec<StyleLayer>,
}

/// Provenance for a style layer that applied to a visible cell.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StyleLayer {
    pub id: StyleId,
    pub style_source_span: Option<ByteSpan>,
    pub application_source_span: Option<ByteSpan>,
}

/// The resolved size and source provenance of one row or column.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AxisGeometry {
    pub size: Option<f64>,
    pub source_span: Option<ByteSpan>,
}

/// Geometry needed to draw one requested column.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnPresentation {
    pub column: u64,
    pub geometry: AxisGeometry,
}

/// Geometry needed to draw one requested row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RowPresentation {
    pub row: u64,
    pub geometry: AxisGeometry,
}

/// A failure that prevents construction or bounded projection of a view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewError {
    InvalidDocument,
    Preparation(PrepareError),
    UnknownSheet(SheetId),
    ViewportTooLarge { cells: u64, limit: u64 },
    PresentedCellLimitExceeded { cells: usize, limit: usize },
    StyleRegionLimitExceeded { regions: usize, limit: usize },
    StyleLayerLimitExceeded { layers: usize, limit: usize },
    StyleApplicationLimitExceeded { applications: usize, limit: usize },
    CoordinateOverflow,
}

impl fmt::Display for ViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDocument => formatter.write_str("document has no semantic workbook"),
            Self::Preparation(error) => write!(formatter, "cannot prepare workbook view: {error}"),
            Self::UnknownSheet(sheet) => write!(formatter, "unknown sheet {sheet}"),
            Self::ViewportTooLarge { cells, limit } => {
                write!(formatter, "viewport has {cells} cells; limit is {limit}")
            }
            Self::PresentedCellLimitExceeded { cells, limit } => {
                write!(
                    formatter,
                    "viewport has {cells} sparse cells; limit is {limit}"
                )
            }
            Self::StyleRegionLimitExceeded { regions, limit } => {
                write!(
                    formatter,
                    "viewport has more than {regions} style regions; limit is {limit}"
                )
            }
            Self::StyleLayerLimitExceeded { layers, limit } => {
                write!(
                    formatter,
                    "cell has more than {layers} style layers; limit is {limit}"
                )
            }
            Self::StyleApplicationLimitExceeded {
                applications,
                limit,
            } => write!(
                formatter,
                "viewport must examine at least {applications} style applications; limit is {limit}"
            ),
            Self::CoordinateOverflow => formatter.write_str("viewport coordinate overflow"),
        }
    }
}
impl std::error::Error for ViewError {}

#[derive(Clone, Debug)]
struct IndexedStyleApplication {
    range: Range,
    style: ResolvedStyle,
    source_order: u64,
}

/// One `@apply` target's inclusive extent on a single sheet axis.
#[derive(Clone, Copy, Debug)]
struct AxisInterval {
    start: u64,
    end: u64,
    /// Position of the owning application within its sheet's source order.
    application: usize,
}

/// A bounded band query against an [`AxisIntervalIndex`].
#[derive(Clone, Copy, Debug)]
struct AxisIntervalQuery {
    start: u64,
    end: u64,
    /// Maximum intervals the caller is willing to have examined.
    limit: usize,
}

/// An interval index over one axis of a sheet's `@apply` targets.
///
/// Intervals are held sorted by start with a max-end tournament tree above
/// them. A subtree is skipped whole when its smallest start is past the
/// queried band or its largest end is before it, so a query descends only
/// where an overlapping interval can exist. Enumerating the applications near
/// a viewport therefore costs work proportional to the overlapping intervals
/// plus the tree depth, never to the number of `@apply` directives authored
/// on the sheet. The index itself stays a small constant factor of the
/// applications the view already retains, so no build-time bound is traded
/// away for the per-viewport one.
#[derive(Clone, Debug, Default)]
struct AxisIntervalIndex {
    /// Sorted by `start`, then by source order for a stable layout.
    intervals: Vec<AxisInterval>,
    /// `max_end[node]` is the largest interval end within that node's subtree.
    max_end: Vec<u64>,
}

impl AxisIntervalIndex {
    fn new(mut intervals: Vec<AxisInterval>) -> Self {
        intervals.sort_unstable_by_key(|interval| (interval.start, interval.application));
        let nodes = intervals.len().saturating_mul(4);
        let mut index = Self {
            intervals,
            max_end: vec![0; nodes],
        };
        if let Some(last) = index.intervals.len().checked_sub(1) {
            index.fill(0, 0, last);
        }
        index
    }

    /// Populates `max_end` for the subtree covering `intervals[low..=high]`.
    fn fill(&mut self, node: usize, low: usize, high: usize) -> u64 {
        let value = if low == high {
            self.intervals[low].end
        } else {
            let middle = low + (high - low) / 2;
            let left = self.fill(node * 2 + 1, low, middle);
            let right = self.fill(node * 2 + 2, middle + 1, high);
            left.max(right)
        };
        self.max_end[node] = value;
        value
    }

    /// Collects the source-order positions of every interval overlapping the
    /// queried band, in index order.
    ///
    /// # Errors
    ///
    /// Returns the number of intervals examined, `limit + 1`, as soon as the
    /// band is found to overlap more intervals than the query allows.
    fn overlapping(&self, query: AxisIntervalQuery) -> Result<Vec<usize>, usize> {
        let mut found = Vec::new();
        let Some(last) = self.intervals.len().checked_sub(1) else {
            return Ok(found);
        };
        if self.collect(0, 0, last, query, &mut found) {
            Ok(found)
        } else {
            Err(found.len())
        }
    }

    /// Walks the subtree covering `intervals[low..=high]`, returning `false`
    /// once more than `query.limit` intervals have been examined.
    fn collect(
        &self,
        node: usize,
        low: usize,
        high: usize,
        query: AxisIntervalQuery,
        found: &mut Vec<usize>,
    ) -> bool {
        // Sorted starts make `intervals[low].start` the smallest start in this
        // subtree, and the tournament tree makes `max_end[node]` its largest
        // end. Either bound failing rules out every interval below this node.
        if self.intervals[low].start > query.end || self.max_end[node] < query.start {
            return true;
        }
        if low == high {
            found.push(self.intervals[low].application);
            return found.len() <= query.limit;
        }
        let middle = low + (high - low) / 2;
        self.collect(node * 2 + 1, low, middle, query, found)
            && self.collect(node * 2 + 2, middle + 1, high, query, found)
    }
}

/// One sheet's `@apply` applications in source order, with an interval index
/// over each axis of their targets.
#[derive(Clone, Debug, Default)]
struct SheetStyleIndex {
    applications: Vec<IndexedStyleApplication>,
    rows: AxisIntervalIndex,
    columns: AxisIntervalIndex,
}

impl SheetStyleIndex {
    fn new(applications: Vec<IndexedStyleApplication>) -> Self {
        let rows = AxisIntervalIndex::new(
            applications
                .iter()
                .enumerate()
                .map(|(application, indexed)| AxisInterval {
                    start: indexed.range.start.row,
                    end: indexed.range.end.row,
                    application,
                })
                .collect(),
        );
        let columns = AxisIntervalIndex::new(
            applications
                .iter()
                .enumerate()
                .map(|(application, indexed)| AxisInterval {
                    start: indexed.range.start.column,
                    end: indexed.range.end.column,
                    application,
                })
                .collect(),
        );
        Self {
            applications,
            rows,
            columns,
        }
    }

    /// Returns the applications overlapping `range` in source order, having
    /// examined at most `limit` of them.
    ///
    /// # Errors
    ///
    /// Returns the number examined when neither axis can narrow the range to
    /// `limit` applications or fewer.
    fn overlapping(
        &self,
        range: Range,
        limit: usize,
    ) -> Result<Vec<&IndexedStyleApplication>, usize> {
        // Either axis alone yields a superset of the overlapping applications.
        // Rows are tried first because a viewport typically scrolls along
        // them; a viewport that shares its rows with too many applications can
        // still be cheap on the column axis, so that is the fallback before
        // the request is refused.
        let mut positions = match self.rows.overlapping(AxisIntervalQuery {
            start: range.start.row,
            end: range.end.row,
            limit,
        }) {
            Ok(positions) => positions,
            Err(_) => self.columns.overlapping(AxisIntervalQuery {
                start: range.start.column,
                end: range.end.column,
                limit,
            })?,
        };
        // Style precedence is source order, which the per-axis index layout
        // does not preserve.
        positions.sort_unstable();
        Ok(positions
            .into_iter()
            .filter_map(|position| self.applications.get(position))
            .filter(|application| application.range.overlaps(range))
            .collect())
    }
}

/// A compact, source-order-resolved interval map for one sheet axis.
///
/// Each key starts a piecewise-constant run. Applying a later geometry
/// directive overwrites only its interval and retains the value immediately
/// after the interval, so lookup is logarithmic and never rescans sheet items.
#[derive(Clone, Debug, Default)]
struct AxisGeometryIndex {
    starts: BTreeMap<u64, AxisGeometry>,
}

impl AxisGeometryIndex {
    fn apply(&mut self, start: u64, end: u64, geometry: AxisGeometry) {
        let after = end.checked_add(1).map(|coordinate| self.get(coordinate));
        let overwritten = self
            .starts
            .range(start..=end)
            .map(|(&coordinate, _)| coordinate)
            .collect::<Vec<_>>();
        for coordinate in overwritten {
            self.starts.remove(&coordinate);
        }
        self.starts.insert(start, geometry);
        if let (Some(after_coordinate), Some(after_geometry)) = (end.checked_add(1), after) {
            self.starts.insert(after_coordinate, after_geometry);
        }
    }

    fn get(&self, coordinate: u64) -> AxisGeometry {
        self.starts
            .range(..=coordinate)
            .next_back()
            .map_or_else(AxisGeometry::default, |(_, geometry)| geometry.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct SheetGeometryIndex {
    columns: AxisGeometryIndex,
    rows: AxisGeometryIndex,
}

impl SheetGeometryIndex {
    fn from_sheet(sheet: &Sheet) -> Self {
        let mut index = Self::default();
        for item in &sheet.items {
            match item {
                SheetItem::ColumnGeometry(geometry) => index.columns.apply(
                    geometry.columns.start,
                    geometry.columns.end,
                    AxisGeometry {
                        size: Some(geometry.width),
                        source_span: geometry.origin.map(|origin| origin.span),
                    },
                ),
                SheetItem::RowGeometry(geometry) => index.rows.apply(
                    geometry.rows.start,
                    geometry.rows.end,
                    AxisGeometry {
                        size: Some(geometry.height),
                        source_span: geometry.origin.map(|origin| origin.span),
                    },
                ),
                _ => {}
            }
        }
        index
    }
}

/// A two-axis index over one prepared sheet's sparse cell coordinates.
///
/// [`Coordinate`] sorts columns before rows. Keeping each axis separately
/// prevents a viewport such as `A1:B2` from walking unrelated `A1000000`
/// cells while looking for data in column B.
#[derive(Debug, Default)]
struct SheetSparseIndex {
    authored_rows_by_column: BTreeMap<u64, BTreeSet<u64>>,
    virtual_rows_by_column: BTreeMap<u64, BTreeSet<u64>>,
}

impl SheetSparseIndex {
    fn from_prepared(sheet: &PreparedSheet) -> Self {
        let mut index = Self::default();
        for coordinate in sheet.authored_cells.keys() {
            index
                .authored_rows_by_column
                .entry(coordinate.column)
                .or_default()
                .insert(coordinate.row);
        }
        for coordinate in sheet.virtual_cells.keys() {
            index
                .virtual_rows_by_column
                .entry(coordinate.column)
                .or_default()
                .insert(coordinate.row);
        }
        index
    }

    fn coordinates_in(&self, range: Range, limit: usize) -> Result<Vec<Coordinate>, ViewError> {
        let mut coordinates = BTreeSet::new();
        // Virtual cells take precedence in the final projection, but both
        // maps participate in the unique-output budget.
        Self::extend_coordinates(&mut coordinates, &self.virtual_rows_by_column, range, limit)?;
        Self::extend_coordinates(
            &mut coordinates,
            &self.authored_rows_by_column,
            range,
            limit,
        )?;
        Ok(coordinates.into_iter().collect())
    }

    fn extend_coordinates(
        output: &mut BTreeSet<Coordinate>,
        rows_by_column: &BTreeMap<u64, BTreeSet<u64>>,
        range: Range,
        limit: usize,
    ) -> Result<(), ViewError> {
        for (&column, rows) in rows_by_column.range(range.start.column..=range.end.column) {
            for &row in rows.range(range.start.row..=range.end.row) {
                output.insert(Coordinate { column, row });
                if output.len() > limit {
                    return Err(ViewError::PresentedCellLimitExceeded {
                        cells: output.len(),
                        limit,
                    });
                }
            }
        }
        Ok(())
    }
}

/// A stateful, sparse projection of one workbook.
///
/// The calculation cache is private so renderers cannot accidentally retain
/// or mutate calculator internals. Calling [`Self::visible_region`] may update
/// that cache, but never mutates the authored workbook.
#[derive(Debug)]
pub struct WorkbookView {
    workbook: Workbook,
    source_map: Option<SourceMap>,
    prepared: PreparedWorkbook,
    sparse_indexes: BTreeMap<SheetId, SheetSparseIndex>,
    style_indexes: BTreeMap<SheetId, SheetStyleIndex>,
    geometry_indexes: BTreeMap<SheetId, SheetGeometryIndex>,
    completeness: ViewCompleteness,
    diagnostics: Vec<Diagnostic>,
    limits: ViewLimits,
    calculation: Option<PreparedCalculation>,
}

impl WorkbookView {
    /// Creates a view from a semantic workbook and optional syntax-owned source map.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Preparation`] when sparse indexing fails. This is
    /// intentionally atomic: no partial presentation model is exposed.
    pub fn new(
        workbook: Workbook,
        source_map: Option<SourceMap>,
        diagnostics: Vec<Diagnostic>,
        limits: ViewLimits,
    ) -> Result<Self, ViewError> {
        Self::build(
            workbook,
            source_map,
            diagnostics,
            limits,
            ViewCompleteness::COMPLETE,
            true,
        )
    }

    fn build(
        workbook: Workbook,
        source_map: Option<SourceMap>,
        diagnostics: Vec<Diagnostic>,
        mut limits: ViewLimits,
        completeness: ViewCompleteness,
        prepare_calculation: bool,
    ) -> Result<Self, ViewError> {
        // A viewport calculation must not quietly bypass this crate's bounds.
        limits.calculation.work.max_output_cells = limits.max_viewport_cells;
        let prepared = PreparedWorkbook::build(&workbook, limits.calculation.prepare)
            .map_err(ViewError::Preparation)?;
        let sparse_indexes = prepared
            .sheets
            .iter()
            .map(|sheet| (sheet.id.clone(), SheetSparseIndex::from_prepared(sheet)))
            .collect();
        let definitions: BTreeMap<_, _> = workbook
            .styles
            .iter()
            .map(|style| (style.id.clone(), style))
            .collect();
        let mut style_indexes = BTreeMap::new();
        for sheet in &workbook.sheets {
            let Some(prepared_sheet) = prepared.sheet(&sheet.id) else {
                continue;
            };
            let applications = index_style_applications(
                sheet,
                prepared_sheet,
                &definitions,
                limits.max_style_layers_per_cell,
            )?;
            style_indexes.insert(sheet.id.clone(), SheetStyleIndex::new(applications));
        }
        let geometry_indexes = workbook
            .sheets
            .iter()
            .map(|sheet| (sheet.id.clone(), SheetGeometryIndex::from_sheet(sheet)))
            .collect();
        let mut all_diagnostics = diagnostics;
        let calculation = if prepare_calculation {
            let engine = ReferenceCalcEngine;
            let calculation_report = engine.prepare(&workbook, limits.calculation.clone());
            all_diagnostics.extend(calculation_report.diagnostics);
            calculation_report.calculation
        } else {
            None
        };
        Ok(Self {
            workbook,
            source_map,
            prepared,
            sparse_indexes,
            style_indexes,
            geometry_indexes,
            completeness,
            diagnostics: all_diagnostics,
            limits,
            calculation,
        })
    }

    /// Creates a view from a parsed document, preserving parser diagnostics and source spans.
    ///
    /// Error-bearing documents remain available as sparse recoverable views,
    /// but calculation is not prepared and their completeness flags are both
    /// `false`. Warnings do not make an otherwise valid document incomplete.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::InvalidDocument`] when semantic lowering did not
    /// yield a workbook, or a preparation error for an invalid workbook.
    pub fn from_document(document: &ParsedDocument, limits: ViewLimits) -> Result<Self, ViewError> {
        let workbook = document
            .workbook
            .clone()
            .ok_or(ViewError::InvalidDocument)?;
        let has_errors = document.has_errors();
        Self::build(
            workbook,
            Some(document.source_map.clone()),
            document.diagnostics.clone(),
            limits,
            if has_errors {
                ViewCompleteness::RECOVERED_INCOMPLETE
            } else {
                ViewCompleteness::COMPLETE
            },
            !has_errors,
        )
    }

    /// Returns declared sheets in their source order, with no coordinate expansion.
    #[must_use]
    pub fn summary(&self) -> WorkbookSummary {
        WorkbookSummary {
            sheets: self
                .workbook
                .sheets
                .iter()
                .filter_map(|sheet| {
                    self.prepared
                        .sheet(&sheet.id)
                        .map(|prepared| sheet_summary(sheet, prepared))
                })
                .collect(),
            completeness: self.completeness,
            diagnostics: self.diagnostics.clone(),
        }
    }

    /// Projects one bounded viewport without allocating empty coordinates outside it.
    ///
    /// # Errors
    ///
    /// Returns a limit error before allocating output when the requested
    /// rectangle or sparse result would exceed [`ViewLimits`].
    pub fn visible_region(
        &mut self,
        request: &VisibleRegionRequest,
    ) -> Result<VisibleRegion, ViewError> {
        let cells = range_cells(request.range)?;
        if cells > self.limits.max_viewport_cells {
            return Err(ViewError::ViewportTooLarge {
                cells,
                limit: self.limits.max_viewport_cells,
            });
        }
        if !self
            .workbook
            .sheets
            .iter()
            .any(|sheet| sheet.id == request.sheet)
        {
            return Err(ViewError::UnknownSheet(request.sheet.clone()));
        }
        // Coordinate is ordered by column then row, so a single rectangular
        // BTreeMap range would include every row from intermediate columns.
        // The two-axis index visits only requested columns and rows and stops
        // before retaining more than the public sparse-cell budget.
        let sparse_coordinates = self
            .sparse_indexes
            .get(&request.sheet)
            .ok_or_else(|| ViewError::UnknownSheet(request.sheet.clone()))?
            .coordinates_in(request.range, self.limits.max_presented_cells)?;
        let (intersecting_styles, style_regions) = self.resolve_style_regions(request)?;

        let mut region_diagnostics = self.diagnostics.clone();
        // Calculation is deliberately after the sparse output limit check so
        // an oversized projection does not trigger avoidable evaluation work.
        let calculated = if request.calculate {
            self.calculate_region(request, &mut region_diagnostics)
        } else {
            BTreeMap::new()
        };
        let sheet = self
            .workbook
            .sheets
            .iter()
            .find(|sheet| sheet.id == request.sheet)
            .ok_or_else(|| ViewError::UnknownSheet(request.sheet.clone()))?;
        let prepared_sheet = self
            .prepared
            .sheet(&request.sheet)
            .ok_or_else(|| ViewError::UnknownSheet(request.sheet.clone()))?;

        let geometry = self
            .geometry_indexes
            .get(&request.sheet)
            .ok_or_else(|| ViewError::UnknownSheet(request.sheet.clone()))?;
        let columns = columns_for(&geometry.columns, request.range)?;
        let rows = rows_for(&geometry.rows, request.range)?;
        let mut presented_cells = Vec::with_capacity(sparse_coordinates.len());
        for coordinate in sparse_coordinates {
            let Some(source) = self.cell_source(prepared_sheet, &request.sheet, coordinate) else {
                continue;
            };
            presented_cells.push(PresentedCell {
                coordinate,
                calculated: calculated.get(&coordinate).cloned(),
                style: resolve_indexed_style(
                    &intersecting_styles,
                    coordinate,
                    self.limits.max_style_layers_per_cell,
                )?,
                column: geometry.columns.get(coordinate.column),
                row: geometry.rows.get(coordinate.row),
                source,
            });
        }

        Ok(VisibleRegion {
            sheet: sheet_summary(sheet, prepared_sheet),
            range: request.range,
            completeness: self.completeness,
            cells: presented_cells,
            style_regions,
            columns,
            rows,
            diagnostics: region_diagnostics,
        })
    }

    /// Resolves the `@apply` intervals overlapping `request`, bounding both
    /// the work examined and the results returned.
    ///
    /// The sheet's interval index reports only the applications whose row or
    /// column band reaches the requested range, so
    /// [`ViewLimits::max_style_applications`] bounds the applications this one
    /// request examines. A viewport away from the styled area of an
    /// application-heavy sheet examines nothing and is projected normally.
    /// Both limits are checked before any output is allocated.
    fn resolve_style_regions(
        &self,
        request: &VisibleRegionRequest,
    ) -> Result<(Vec<IndexedStyleApplication>, Vec<StyledRegion>), ViewError> {
        let style_index = self
            .style_indexes
            .get(&request.sheet)
            .ok_or_else(|| ViewError::UnknownSheet(request.sheet.clone()))?;
        let intersecting_styles = style_index
            .overlapping(request.range, self.limits.max_style_applications)
            .map_err(|applications| ViewError::StyleApplicationLimitExceeded {
                applications,
                limit: self.limits.max_style_applications,
            })?;
        if intersecting_styles.len() > self.limits.max_style_regions {
            return Err(ViewError::StyleRegionLimitExceeded {
                regions: intersecting_styles.len(),
                limit: self.limits.max_style_regions,
            });
        }
        let style_regions = intersecting_styles
            .iter()
            .filter_map(|application| {
                application
                    .range
                    .intersection(request.range)
                    .map(|range| StyledRegion {
                        range,
                        style: application.style.clone(),
                        source_order: application.source_order,
                    })
            })
            .collect::<Vec<_>>();
        let intersecting_styles = intersecting_styles.into_iter().cloned().collect::<Vec<_>>();
        Ok((intersecting_styles, style_regions))
    }

    fn calculate_region(
        &mut self,
        request: &VisibleRegionRequest,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> BTreeMap<Coordinate, CalcValue> {
        let Some(calculation) = self.calculation.as_mut() else {
            return BTreeMap::new();
        };
        let engine = ReferenceCalcEngine;
        let result = engine.calculate(
            calculation,
            &CalculationRequest::new(request.sheet.clone(), request.range),
        );
        extend_unique_diagnostics(diagnostics, result.diagnostics);
        result
            .cells
            .into_iter()
            .map(|cell| (cell.cell.coordinate, cell.value))
            .collect()
    }

    fn cell_span(
        &self,
        sheet: &SheetId,
        coordinate: Coordinate,
        origin: Option<Origin>,
    ) -> Option<ByteSpan> {
        self.source_map
            .as_ref()
            .and_then(|map| map.cell(sheet, coordinate).map(|location| location.field))
            .or_else(|| origin.map(|origin| origin.span))
    }

    fn cell_source(
        &self,
        prepared: &PreparedSheet,
        sheet: &SheetId,
        coordinate: Coordinate,
    ) -> Option<CellSource> {
        // A fill intentionally overlays an authored *blank* field. Keep its
        // generated formula visible rather than presenting the CSV placeholder
        // that made the fill legal.
        prepared.virtual_cell(coordinate).map_or_else(
            || {
                prepared
                    .authored_cell(coordinate)
                    .map(|authored| CellSource::Authored {
                        value: authored.cell.value.clone(),
                        source_span: self.cell_span(sheet, coordinate, authored.cell.origin),
                    })
            },
            |virtual_cell| {
                Some(CellSource::VirtualFill {
                    formula: virtual_cell.formula.clone(),
                    fill_source_span: virtual_cell.fill_origin.map(|origin| origin.span),
                    fill_anchor: virtual_cell.fill_anchor,
                })
            },
        )
    }
}

fn sheet_summary(sheet: &Sheet, prepared: &PreparedSheet) -> SheetSummary {
    SheetSummary {
        id: sheet.id.clone(),
        label: sheet.label.clone(),
        source_span: sheet.origin.map(|origin| origin.span),
        authored_cell_count: prepared.authored_cells.len(),
        virtual_cell_count: prepared.virtual_cells.len(),
        footprint_count: prepared.footprints.len(),
    }
}

/// Appends diagnostics in stable first-seen order without repeating persistent
/// preparation diagnostics when calculation returns the same diagnostic again.
fn extend_unique_diagnostics(destination: &mut Vec<Diagnostic>, additions: Vec<Diagnostic>) {
    for diagnostic in additions {
        if !destination.contains(&diagnostic) {
            destination.push(diagnostic);
        }
    }
}

fn range_cells(range: Range) -> Result<u64, ViewError> {
    range
        .width()
        .ok()
        .and_then(|width| {
            range
                .height()
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(ViewError::CoordinateOverflow)
}

fn columns_for(
    geometry: &AxisGeometryIndex,
    range: Range,
) -> Result<Vec<ColumnPresentation>, ViewError> {
    let count = range.width().map_err(|_| ViewError::CoordinateOverflow)?;
    let capacity = usize::try_from(count).map_err(|_| ViewError::CoordinateOverflow)?;
    let mut columns = Vec::with_capacity(capacity);
    for column in range.start.column..=range.end.column {
        columns.push(ColumnPresentation {
            column,
            geometry: geometry.get(column),
        });
    }
    Ok(columns)
}

fn rows_for(geometry: &AxisGeometryIndex, range: Range) -> Result<Vec<RowPresentation>, ViewError> {
    let count = range.height().map_err(|_| ViewError::CoordinateOverflow)?;
    let capacity = usize::try_from(count).map_err(|_| ViewError::CoordinateOverflow)?;
    let mut rows = Vec::with_capacity(capacity);
    for row in range.start.row..=range.end.row {
        rows.push(RowPresentation {
            row,
            geometry: geometry.get(row),
        });
    }
    Ok(rows)
}

/// Indexes every `@apply` directive authored on `sheet`, in source order.
///
/// This runs once at build time and is not itself bounded by
/// [`ViewLimits::max_style_applications`]: that limit is a per-viewport bound
/// on the work examined while projecting a requested region, so a sheet with
/// many `@apply` directives must still open successfully. The limit is
/// enforced later, in [`WorkbookView::visible_region`], against the
/// applications the [`SheetStyleIndex`] reports as near that request.
fn index_style_applications(
    sheet: &Sheet,
    prepared: &PreparedSheet,
    definitions: &BTreeMap<StyleId, &marksheet_model::Style>,
    max_layers: usize,
) -> Result<Vec<IndexedStyleApplication>, ViewError> {
    let mut applications = Vec::new();
    for (position, item) in sheet.items.iter().enumerate() {
        let SheetItem::Apply(apply) = item else {
            continue;
        };
        let Some(range) = (match &apply.target {
            ApplyTarget::Range(range) => Some(*range),
            ApplyTarget::Table { table, region } => {
                prepared.tables.get(table).and_then(|table| match region {
                    TableRegion::Headers => Some(Range {
                        start: table.footprint.start,
                        end: Coordinate {
                            column: table.footprint.end.column,
                            row: table.footprint.start.row,
                        },
                    }),
                    TableRegion::Data => table.data_range,
                    TableRegion::Column { header } => table.data_column(header),
                })
            }
        }) else {
            continue;
        };
        let mut style = ResolvedStyle::default();
        for id in &apply.styles {
            let Some(definition) = definitions.get(id) else {
                continue;
            };
            if style.layers.len() >= max_layers {
                return Err(ViewError::StyleLayerLimitExceeded {
                    layers: style.layers.len().saturating_add(1),
                    limit: max_layers,
                });
            }
            merge_properties(&mut style.properties, &definition.properties);
            style.layers.push(StyleLayer {
                id: id.clone(),
                style_source_span: definition.origin.map(|origin| origin.span),
                application_source_span: apply.origin.map(|origin| origin.span),
            });
        }
        applications.push(IndexedStyleApplication {
            range,
            style,
            source_order: u64::try_from(position).map_err(|_| ViewError::CoordinateOverflow)?,
        });
    }
    Ok(applications)
}

fn resolve_indexed_style(
    applications: &[IndexedStyleApplication],
    coordinate: Coordinate,
    max_layers: usize,
) -> Result<ResolvedStyle, ViewError> {
    let mut resolved = ResolvedStyle::default();
    for application in applications {
        if !application.range.contains(coordinate) {
            continue;
        }
        let layer_count = resolved
            .layers
            .len()
            .checked_add(application.style.layers.len())
            .ok_or(ViewError::StyleLayerLimitExceeded {
                layers: usize::MAX,
                limit: max_layers,
            })?;
        if layer_count > max_layers {
            return Err(ViewError::StyleLayerLimitExceeded {
                layers: layer_count,
                limit: max_layers,
            });
        }
        merge_properties(&mut resolved.properties, &application.style.properties);
        resolved.layers.extend(application.style.layers.clone());
    }
    Ok(resolved)
}

fn merge_properties(destination: &mut StyleProperties, source: &StyleProperties) {
    macro_rules! merge {
        ($field:ident) => {
            if source.$field.is_some() {
                destination.$field = source.$field.clone();
            }
        };
    }
    merge!(bold);
    merge!(italic);
    merge!(wrap);
    merge!(text_color);
    merge!(fill);
    merge!(font_size);
    merge!(align);
    merge!(valign);
    merge!(number);
    merge!(decimals);
    merge!(currency);
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::{Coordinate, Range};
    use marksheet_syntax::parse;

    fn coordinate(value: &str) -> Coordinate {
        value.parse().unwrap()
    }

    #[test]
    fn budget_is_sparse_calculated_and_keeps_declared_sheet_order() {
        let document = parse(include_bytes!("../../../examples/budget.ms"));
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let summary = view.summary();
        assert_eq!(
            summary
                .sheets
                .iter()
                .map(|sheet| sheet.id.as_str())
                .collect::<Vec<_>>(),
            ["inputs", "summary"]
        );
        let region = view
            .visible_region(&VisibleRegionRequest::new(
                "summary".parse().unwrap(),
                Range::parse("A1:B4").unwrap(),
            ))
            .unwrap();
        assert_eq!(region.cells.len(), 8);
        assert!(
            matches!(region.cells.iter().find(|cell| cell.coordinate == coordinate("B4")).and_then(|cell| cell.calculated.as_ref()), Some(CalcValue::Number(value)) if (*value - 1648.0).abs() < f64::EPSILON)
        );
        assert_eq!(region.columns[0].geometry.size, Some(20.0));
    }

    #[test]
    fn style_precedence_is_property_wise_and_column_excludes_table_header() {
        let document = parse(include_bytes!("../../../examples/budget.ms"));
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let inputs: SheetId = "inputs".parse().unwrap();
        let region = view
            .visible_region(&VisibleRegionRequest {
                sheet: inputs,
                range: Range::parse("A1:D2").unwrap(),
                calculate: false,
            })
            .unwrap();
        let header = region
            .cells
            .iter()
            .find(|cell| cell.coordinate == coordinate("A1"))
            .unwrap();
        assert_eq!(header.style.properties.bold, Some(true));
        assert_eq!(
            header.style.properties.number, None,
            "cost column style must not include table header"
        );
        let cost = region
            .cells
            .iter()
            .find(|cell| cell.coordinate == coordinate("B2"))
            .unwrap();
        assert_eq!(
            cost.style.properties.number,
            Some(marksheet_model::NumberFormat::Currency)
        );
        assert_eq!(
            cost.style.properties.align,
            Some(marksheet_model::HorizontalAlignment::Right)
        );
    }

    #[test]
    fn virtual_fill_retains_fill_origin_without_becoming_an_authored_cell() {
        let document = parse(include_bytes!("../../../examples/budget.ms"));
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let region = view
            .visible_region(&VisibleRegionRequest {
                sheet: "inputs".parse().unwrap(),
                range: Range::parse("D2:D4").unwrap(),
                calculate: false,
            })
            .unwrap();
        assert!(region.cells.iter().all(|cell| matches!(cell.source, CellSource::VirtualFill { fill_anchor, .. } if fill_anchor == coordinate("D2"))));
    }

    #[test]
    fn distant_sparse_blocks_do_not_expand_the_sheet() {
        let source = b"#!marksheet 0.1\n@sheet s \"Sparse\"\n@block A1 csv\nnear\n@end\n@block XFD1000000 csv\nfar\n@end\n";
        let document = parse(source);
        assert!(!document.has_errors(), "{:?}", document.diagnostics);
        let limits = ViewLimits {
            max_viewport_cells: 4,
            ..ViewLimits::default()
        };
        let mut view = WorkbookView::from_document(&document, limits).unwrap();
        assert_eq!(view.summary().sheets[0].authored_cell_count, 2);
        let far = view
            .visible_region(&VisibleRegionRequest {
                sheet: "s".parse().unwrap(),
                range: Range::parse("XFD1000000").unwrap(),
                calculate: false,
            })
            .unwrap();
        assert_eq!(far.cells.len(), 1);
        assert_eq!(far.cells[0].coordinate, coordinate("XFD1000000"));
        assert!(matches!(
            view.visible_region(&VisibleRegionRequest {
                sheet: "s".parse().unwrap(),
                range: Range::parse("A1:C2").unwrap(),
                calculate: false
            }),
            Err(ViewError::ViewportTooLarge { .. })
        ));
    }

    #[test]
    fn two_axis_sparse_index_does_not_walk_out_of_range_rows() {
        let mut index = SheetSparseIndex::default();
        index
            .authored_rows_by_column
            .entry(1)
            .or_default()
            .extend([1, 10_000_000]);
        index
            .authored_rows_by_column
            .entry(2)
            .or_default()
            .extend([2, 20_000_000]);

        let coordinates = index
            .coordinates_in(Range::parse("A1:B2").unwrap(), 2)
            .unwrap();
        assert_eq!(coordinates, vec![coordinate("A1"), coordinate("B2")]);
    }

    #[test]
    fn sparse_cell_limit_rejects_before_calculation_or_presentation() {
        let source = b"#!marksheet 0.1\n@sheet s \"Budget\"\n@block A1 csv\na,b,c\n@end\n";
        let document = parse(source);
        let limits = ViewLimits {
            max_presented_cells: 2,
            ..ViewLimits::default()
        };
        let mut view = WorkbookView::from_document(&document, limits).unwrap();
        assert_eq!(
            view.visible_region(&VisibleRegionRequest {
                sheet: "s".parse().unwrap(),
                range: Range::parse("A1:C1").unwrap(),
                calculate: true,
            }),
            Err(ViewError::PresentedCellLimitExceeded { cells: 3, limit: 2 })
        );
    }

    #[test]
    fn calculation_diagnostics_are_stably_deduplicated() {
        let source = b"#!marksheet 0.1\n@sheet s \"Cycle\"\n@block A1 csv\n=A1\n@end\n";
        let document = parse(source);
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let region = view
            .visible_region(&VisibleRegionRequest::new(
                "s".parse().unwrap(),
                Range::parse("A1").unwrap(),
            ))
            .unwrap();
        assert_eq!(
            region
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_str() == "MS2303")
                .count(),
            1
        );
    }

    #[test]
    fn required_unavailable_extension_is_viewable_but_incomplete_and_uncalculated() {
        let source = b"#!marksheet 0.1\n@require actuarial_functions@1\n@sheet s \"Recovered\"\n@block A1 csv\nValue,Double\n5,=A2*2\n@end\n";
        let document = parse(source);
        assert!(document.has_errors());
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS3101")
        );

        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        assert_eq!(
            view.summary().completeness,
            ViewCompleteness::RECOVERED_INCOMPLETE
        );
        let region = view
            .visible_region(&VisibleRegionRequest::new(
                "s".parse().unwrap(),
                Range::parse("A1:B2").unwrap(),
            ))
            .unwrap();

        assert_eq!(region.completeness, ViewCompleteness::RECOVERED_INCOMPLETE);
        assert_eq!(
            region.cells.len(),
            4,
            "recovered core cells remain viewable"
        );
        assert!(
            region.cells.iter().all(|cell| cell.calculated.is_none()),
            "an invalid parsed document must not expose calculated values"
        );
    }

    #[test]
    fn optional_unavailable_extension_warning_remains_complete_and_calculable() {
        let source = b"#!marksheet 0.1\n@use actuarial_functions@1\n@sheet s \"Optional\"\n@block A1 csv\nValue,Double\n5,=A2*2\n@end\n";
        let document = parse(source);
        assert!(!document.has_errors());
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS3102")
        );

        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        assert_eq!(view.summary().completeness, ViewCompleteness::COMPLETE);
        let region = view
            .visible_region(&VisibleRegionRequest::new(
                "s".parse().unwrap(),
                Range::parse("A1:B2").unwrap(),
            ))
            .unwrap();

        assert_eq!(region.completeness, ViewCompleteness::COMPLETE);
        assert!(matches!(
            region
                .cells
                .iter()
                .find(|cell| cell.coordinate == coordinate("B2"))
                .and_then(|cell| cell.calculated.as_ref()),
            Some(CalcValue::Number(value)) if (*value - 10.0).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn style_regions_preserve_applies_to_blank_cells() {
        let source =
            b"#!marksheet 0.1\n@style note italic=true\n@sheet s \"Styled\"\n@apply C3:D4 note\n";
        let document = parse(source);
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let region = view
            .visible_region(&VisibleRegionRequest {
                sheet: "s".parse().unwrap(),
                range: Range::parse("B2:C3").unwrap(),
                calculate: false,
            })
            .unwrap();
        assert!(region.cells.is_empty());
        assert_eq!(region.style_regions.len(), 1);
        assert_eq!(region.style_regions[0].range, Range::parse("C3").unwrap());
        assert_eq!(region.style_regions[0].style.properties.italic, Some(true));
    }

    #[test]
    fn geometry_index_preserves_later_interval_precedence() {
        let mut index = AxisGeometryIndex::default();
        index.apply(
            1,
            10,
            AxisGeometry {
                size: Some(12.0),
                source_span: None,
            },
        );
        index.apply(
            3,
            4,
            AxisGeometry {
                size: Some(24.0),
                source_span: None,
            },
        );

        assert_eq!(index.get(1).size, Some(12.0));
        assert_eq!(index.get(3).size, Some(24.0));
        assert_eq!(index.get(5).size, Some(12.0));
        assert_eq!(index.get(11).size, None);
    }

    #[test]
    fn style_application_limit_does_not_block_opening_a_document() {
        // max_style_applications is documented as a per-viewport bound on work
        // examined while projecting a region, not a whole-sheet bound checked
        // at build time. A sheet with more @apply directives than the limit
        // must still open successfully.
        let source = b"#!marksheet 0.1\n@style note italic=true\n@sheet s \"Styled\"\n@apply A1 note\n@apply B1 note\n";
        let document = parse(source);
        let limits = ViewLimits {
            max_style_applications: 1,
            ..ViewLimits::default()
        };
        assert!(WorkbookView::from_document(&document, limits).is_ok());
    }

    #[test]
    fn style_application_limit_bounds_request_resolution_work() {
        let source = b"#!marksheet 0.1\n@style note italic=true\n@sheet s \"Styled\"\n@apply A1 note\n@apply B1 note\n";
        let document = parse(source);
        let limits = ViewLimits {
            max_style_applications: 1,
            ..ViewLimits::default()
        };
        let mut view = WorkbookView::from_document(&document, limits).unwrap();
        let before = format!("{view:?}");
        let result = view.visible_region(&VisibleRegionRequest {
            sheet: "s".parse().unwrap(),
            range: Range::parse("A1:B1").unwrap(),
            calculate: false,
        });
        assert!(matches!(
            result,
            Err(ViewError::StyleApplicationLimitExceeded {
                applications: 2,
                limit: 1
            })
        ));
        // The rejected request must not have mutated any cached view state.
        assert_eq!(format!("{view:?}"), before);
    }

    /// Builds a sheet whose rows 1..=1025 each carry an `@apply A:C` band,
    /// exceeding the default `max_style_applications`.
    fn oversized_style_document() -> ParsedDocument {
        use std::fmt::Write as _;

        let mut source =
            String::from("#!marksheet 0.1\n@style note italic=true\n@sheet s \"Styled\"\n");
        for row in 1..=1025u32 {
            let _ = writeln!(source, "@apply A{row}:C{row} note");
        }
        let document = parse(source.as_bytes());
        assert!(!document.has_errors(), "{:?}", document.diagnostics);
        document
    }

    #[test]
    fn thousand_and_twenty_five_style_applications_open_successfully() {
        // Regression test: a valid workbook with more @apply directives on one
        // sheet than the default max_style_applications must still open, since
        // the limit bounds per-viewport projection work, not build-time
        // indexing. Reproduces the 1025-application case from the finding.
        let document = oversized_style_document();
        let result = WorkbookView::from_document(&document, ViewLimits::default());
        assert!(result.is_ok());

        // Requesting a viewport that really does reach all 1025 pre-indexed
        // applications exceeds the default limit and fails, atomically, with
        // the documented error.
        let mut view = result.unwrap();
        let before = format!("{view:?}");
        let region = view.visible_region(&VisibleRegionRequest {
            sheet: "s".parse().unwrap(),
            range: Range::parse("A1:A1025").unwrap(),
            calculate: false,
        });
        assert!(matches!(
            region,
            Err(ViewError::StyleApplicationLimitExceeded {
                applications: 1025,
                limit: 1_024
            })
        ));
        // The rejected request must not have mutated any cached view state.
        assert_eq!(format!("{view:?}"), before);
    }

    #[test]
    fn viewports_away_from_the_styled_area_project_on_an_oversized_sheet() {
        // The limit is per viewport, so exceeding it on one request must not
        // make the sheet unviewable: requests that do not reach the excess
        // applications still project normally.
        let document = oversized_style_document();
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let mut project = |range: &str| {
            view.visible_region(&VisibleRegionRequest {
                sheet: "s".parse().unwrap(),
                range: Range::parse(range).unwrap(),
                calculate: false,
            })
        };

        // Far on both axes: the row index prunes every application.
        let far = project("ZZ9000:ZZ9000").unwrap();
        assert!(far.style_regions.is_empty());
        // Distant rows, styled column: still pruned by the row index.
        let below = project("A9000:C9100").unwrap();
        assert!(below.style_regions.is_empty());
        // Every application shares this row band, so the row index cannot
        // narrow the request; the column index prunes them instead.
        let beside = project("ZZ1:ZZ1025").unwrap();
        assert!(beside.style_regions.is_empty());
        // A viewport that genuinely overlaps a few applications resolves them.
        let overlapping = project("B2:C3").unwrap();
        assert_eq!(
            overlapping
                .style_regions
                .iter()
                .map(|region| region.range)
                .collect::<Vec<_>>(),
            vec![
                Range::parse("B2:C2").unwrap(),
                Range::parse("B3:C3").unwrap()
            ]
        );
    }

    #[test]
    fn style_index_still_finds_a_sheet_spanning_application_from_far_away() {
        // Pruning must never lose a wide application: a directive covering the
        // whole sheet reaches every viewport, however distant.
        use std::fmt::Write as _;

        let mut source =
            String::from("#!marksheet 0.1\n@style note italic=true\n@sheet s \"Styled\"\n");
        for row in 1..=1024u32 {
            let _ = writeln!(source, "@apply A{row} note");
        }
        source.push_str("@apply A1:XFD1048576 note\n");
        let document = parse(source.as_bytes());
        assert!(!document.has_errors(), "{:?}", document.diagnostics);
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let region = view
            .visible_region(&VisibleRegionRequest {
                sheet: "s".parse().unwrap(),
                range: Range::parse("ZZ9000:ZZ9000").unwrap(),
                calculate: false,
            })
            .unwrap();
        assert_eq!(
            region
                .style_regions
                .iter()
                .map(|region| region.range)
                .collect::<Vec<_>>(),
            vec![Range::parse("ZZ9000").unwrap()]
        );
    }

    #[test]
    fn style_index_reports_applications_in_source_order() {
        // The per-axis index is ordered by interval start, but style
        // precedence is source order: the later @apply must win even when it
        // starts on an earlier row.
        let source = b"#!marksheet 0.1\n@style low bold=false\n@style high bold=true\n@sheet s \"Styled\"\n@block B2 csv\nx\n@end\n@apply B2 low\n@apply A1:B2 high\n";
        let document = parse(source);
        assert!(!document.has_errors(), "{:?}", document.diagnostics);
        let mut view = WorkbookView::from_document(&document, ViewLimits::default()).unwrap();
        let region = view
            .visible_region(&VisibleRegionRequest {
                sheet: "s".parse().unwrap(),
                range: Range::parse("A1:B2").unwrap(),
                calculate: false,
            })
            .unwrap();
        assert_eq!(
            region
                .style_regions
                .iter()
                .map(|region| region.source_order)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let cell = region
            .cells
            .iter()
            .find(|cell| cell.coordinate == coordinate("B2"))
            .unwrap();
        assert_eq!(cell.style.properties.bold, Some(true));
    }

    #[test]
    fn axis_interval_index_examines_only_intervals_near_the_band() {
        // One interval spans the whole axis while a thousand short ones sit at
        // its start. A band far from the short intervals must be answerable
        // within a limit far below their count.
        let mut intervals = (0..1_000)
            .map(|application| AxisInterval {
                start: 1,
                end: 2,
                application,
            })
            .collect::<Vec<_>>();
        intervals.push(AxisInterval {
            start: 1,
            end: 1_000_000,
            application: 1_000,
        });
        let index = AxisIntervalIndex::new(intervals);

        assert_eq!(
            index.overlapping(AxisIntervalQuery {
                start: 900_000,
                end: 900_001,
                limit: 1,
            }),
            Ok(vec![1_000])
        );
        assert_eq!(
            index.overlapping(AxisIntervalQuery {
                start: 2_000_000,
                end: 2_000_001,
                limit: 1,
            }),
            Ok(Vec::new())
        );
        assert_eq!(
            index.overlapping(AxisIntervalQuery {
                start: 1,
                end: 1,
                limit: 8,
            }),
            Err(9)
        );
    }
}
