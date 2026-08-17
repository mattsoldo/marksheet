//! Executable conformance runner for the versioned browser-session fixtures.
//!
//! The fixture documents, rather than case ids in this file, drive every
//! assertion. Assertions that need a DOM, worker scheduling, or a filesystem
//! host are named by `unsupported_assertions` and must not be silently skipped.

use std::{collections::BTreeMap, fs, path::PathBuf};

use marksheet_calc::{CalculationResult, eval::CalcValue};
use marksheet_edit::transaction::{EditErrorKind, EditOperation, EditTransaction};
use marksheet_model::{
    Color, Coordinate, HorizontalAlignment, NumberFormat, Range, SheetId, Value,
};
use marksheet_view::{CellSource, PresentedCell};
use marksheet_wasm::{
    PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, SessionLimits, WorkbenchSession,
    WorkerErrorCode, WorkerRequest, WorkerResponse, WorkerRuntime,
};
use serde::Deserialize;

const FIXTURE_PROTOCOL: &str = "marksheet-view-conformance@1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u64,
    protocol: String,
    cases: Vec<ManifestCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCase {
    id: String,
    kind: FixtureKind,
    fixture: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FixtureKind {
    WorkbookView,
    LayerProjection,
    SparseViewport,
    WorkerProtocol,
    DiagnosticSource,
    ExternalChange,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    protocol: String,
    #[serde(default)]
    source: Option<String>,
    operations: Vec<Operation>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Open {
        expect: OpenExpectation,
    },
    OpenLocal {
        expect: LocalOpenExpectation,
    },
    VisibleRegion {
        sheet: String,
        range: String,
        expect_layers: Vec<Layer>,
        #[serde(default)]
        expect_cells: BTreeMap<String, ExpectedCell>,
        #[serde(default)]
        expect_authored_coordinates: Vec<String>,
        #[serde(default)]
        expect_absent_coordinates: Vec<String>,
        #[serde(default)]
        budget: Option<SparseBudget>,
        #[serde(default)]
        unsupported_assertions: Vec<HostOnlyAssertion>,
    },
    EditAndSave {
        edit: SetCellExpectation,
        expect: SaveExpectation,
    },
    SimulateExternalReplace {
        current_source: String,
    },
    Request {
        request_id: String,
        worker_protocol: String,
        revision: u64,
        kind: RequestKind,
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        targets: Vec<String>,
    },
    Reply {
        request_id: String,
        revision: u64,
        outcome: ReplyOutcome,
        #[serde(default)]
        must_not_mutate_active_revision: bool,
    },
    AssertActiveRevision {
        revision: u64,
        must_not_include_result_from: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenExpectation {
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    sheet_tabs: Vec<SheetTab>,
    #[serde(default)]
    unsupported_required_extensions: Vec<String>,
    #[serde(default)]
    valid_editable_workbook: Option<bool>,
    #[serde(default)]
    diagnostics: Vec<ExpectedDiagnostic>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalOpenExpectation {
    revision: u64,
    base_snapshot: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SheetTab {
    id: String,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedDiagnostic {
    code: String,
    primary_source_excerpt: String,
    related_cells: Vec<RelatedCell>,
    source_navigation: SourceNavigation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelatedCell {
    sheet: String,
    coordinate: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SourceNavigation {
    Available,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Layer {
    Authored,
    Virtual,
    Calculated,
    Presentation,
    Geometry,
    SourceLinks,
}

const STANDARD_LAYERS: [Layer; 6] = [
    Layer::Authored,
    Layer::Virtual,
    Layer::Calculated,
    Layer::Presentation,
    Layer::Geometry,
    Layer::SourceLinks,
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // Field names intentionally mirror fixture JSON.
struct SparseBudget {
    max_returned_cells: usize,
    max_rendered_grid_cells: usize,
    max_coordinate_probes: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum HostOnlyAssertion {
    MaxRenderedGridCells,
    MaxCoordinateProbes,
    Writes,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedCell {
    #[serde(default)]
    authored: Option<ExpectedValue>,
    #[serde(default, rename = "virtual")]
    virtual_cell: Option<ExpectedVirtual>,
    #[serde(default)]
    calculated: Option<ExpectedScalar>,
    #[serde(default)]
    style: Option<ExpectedStyle>,
    #[serde(default)]
    geometry: Option<ExpectedGeometry>,
    #[serde(default)]
    source_link: Option<SourceLink>,
    #[serde(default)]
    set_cell_outcome: Option<SetCellOutcome>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ExpectedValue {
    Number { value: f64 },
    Formula { source: String },
    Text { value: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ExpectedScalar {
    Number { value: f64 },
    Text { value: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedVirtual {
    formula_source: String,
    origin: FillOrigin,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FillOrigin {
    Fill,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedStyle {
    #[serde(default)]
    bold: Option<bool>,
    #[serde(default)]
    text_color: Option<String>,
    #[serde(default)]
    fill: Option<String>,
    #[serde(default)]
    number: Option<ExpectedNumberFormat>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    decimals: Option<u8>,
    #[serde(default)]
    align: Option<ExpectedAlignment>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ExpectedNumberFormat {
    Currency,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ExpectedAlignment {
    Right,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedGeometry {
    column_width: f64,
    row_height: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SourceLink {
    CsvField,
    FillDirective,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SetCellOutcome {
    VirtualCellRefused,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SetCellExpectation {
    SetCell {
        sheet: String,
        coordinate: String,
        value: ExpectedValue,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveExpectation {
    outcome: SaveOutcome,
    #[serde(default)]
    new_revision: Option<u64>,
    #[serde(default)]
    changed_authored_coordinates: Vec<RelatedCell>,
    #[serde(default)]
    after_source: Option<String>,
    #[serde(default)]
    focused_source_replacement: Option<FocusedReplacement>,
    #[serde(default)]
    recalculated: Option<CalculatedExpectation>,
    #[serde(default)]
    writes: Option<u64>,
    #[serde(default)]
    active_source: Option<String>,
    #[serde(default)]
    diagnostic_kind: Option<DiagnosticKind>,
    #[serde(default)]
    unsupported_assertions: Vec<HostOnlyAssertion>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SaveOutcome {
    Saved,
    Conflict,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusedReplacement {
    old: String,
    new: String,
    count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalculatedExpectation {
    sheet: String,
    coordinate: String,
    kind: ScalarKind,
    value: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ScalarKind {
    Number,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DiagnosticKind {
    Conflict,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RequestKind {
    Open,
    Calculate,
    ReplaceSource,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ReplyOutcome {
    Opened,
    CancelledOrStale,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/view")
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    fs::read(fixture_root().join(name))
        .unwrap_or_else(|error| panic!("could not read view fixture {name:?}: {error}"))
}

fn fixture_source(name: &str) -> Vec<u8> {
    let path = fixture_root().join(name);
    fs::read(&path)
        .unwrap_or_else(|error| panic!("could not read source {}: {error}", path.display()))
}

fn coordinate(value: &str) -> Coordinate {
    Coordinate::parse(value)
        .unwrap_or_else(|error| panic!("invalid fixture coordinate {value:?}: {error}"))
}

fn range(value: &str) -> Range {
    Range::parse(value).unwrap_or_else(|error| panic!("invalid fixture range {value:?}: {error}"))
}

fn sheet(value: &str) -> SheetId {
    SheetId::parse(value).unwrap_or_else(|error| panic!("invalid fixture sheet {value:?}: {error}"))
}

fn read_manifest() -> Manifest {
    serde_json::from_slice(&fixture_bytes("manifest.json"))
        .expect("view manifest must have its documented strict shape")
}

fn read_fixture(case: &ManifestCase) -> Fixture {
    let fixture: Fixture =
        serde_json::from_slice(&fixture_bytes(&case.fixture)).unwrap_or_else(|error| {
            panic!("{} must have a strict fixture shape: {error}", case.fixture)
        });
    assert_eq!(
        fixture.protocol, FIXTURE_PROTOCOL,
        "{} protocol",
        case.fixture
    );
    assert!(
        !fixture.operations.is_empty(),
        "{} operations",
        case.fixture
    );
    fixture
}

fn calculated_by_coordinate<'a>(
    calculation: &'a CalculationResult,
    expected: &str,
) -> &'a CalcValue {
    let expected = coordinate(expected);
    &calculation
        .cells
        .iter()
        .find(|cell| cell.cell.coordinate == expected)
        .unwrap_or_else(|| panic!("missing calculated cell {expected}"))
        .value
}

fn assert_scalar(actual: &CalcValue, expected: &ExpectedScalar, coordinate: &str) {
    match (actual, expected) {
        (CalcValue::Number(actual), ExpectedScalar::Number { value }) => assert_eq!(
            actual.to_bits(),
            value.to_bits(),
            "unexpected value at {coordinate}"
        ),
        (CalcValue::Text(actual), ExpectedScalar::Text { value }) => {
            assert_eq!(actual, value, "unexpected value at {coordinate}");
        }
        _ => panic!("unexpected calculated value kind at {coordinate}"),
    }
}

fn assert_value(actual: &Value, expected: &ExpectedValue, coordinate: &str) {
    match (actual, expected) {
        (Value::Number(actual), ExpectedValue::Number { value }) => assert_eq!(
            actual.to_bits(),
            value.to_bits(),
            "unexpected authored value at {coordinate}"
        ),
        (Value::Formula(actual), ExpectedValue::Formula { source }) => {
            assert_eq!(
                actual.as_str(),
                source,
                "unexpected formula at {coordinate}"
            );
        }
        (Value::Text(actual), ExpectedValue::Text { value }) => {
            assert_eq!(actual, value, "unexpected authored text at {coordinate}");
        }
        _ => panic!("unexpected authored value kind at {coordinate}"),
    }
}

fn assert_expected_cell(
    cell: &PresentedCell,
    expected: &ExpectedCell,
    coordinate: &str,
    layers: &[Layer],
) {
    if let Some(authored) = &expected.authored {
        require_layer(layers, Layer::Authored, coordinate);
        let CellSource::Authored { value, source_span } = &cell.source else {
            panic!("{coordinate} should be authored");
        };
        assert!(source_span.is_some(), "{coordinate} needs a source link");
        assert_value(value, authored, coordinate);
    }
    if let Some(virtual_cell) = &expected.virtual_cell {
        require_layer(layers, Layer::Virtual, coordinate);
        let CellSource::VirtualFill {
            formula,
            fill_source_span,
            ..
        } = &cell.source
        else {
            panic!("{coordinate} should be virtual");
        };
        assert!(
            fill_source_span.is_some(),
            "{coordinate} needs a fill source link"
        );
        assert_eq!(formula.as_str(), virtual_cell.formula_source);
        assert_eq!(virtual_cell.origin, FillOrigin::Fill);
    }
    if let Some(style) = &expected.style {
        require_layer(layers, Layer::Presentation, coordinate);
        if let Some(bold) = style.bold {
            assert_eq!(cell.style.properties.bold, Some(bold));
        }
        if let Some(color) = &style.text_color {
            assert_eq!(
                cell.style.properties.text_color,
                Some(Color::parse(color).unwrap())
            );
        }
        if let Some(color) = &style.fill {
            assert_eq!(
                cell.style.properties.fill,
                Some(Color::parse(color).unwrap())
            );
        }
        if let Some(number) = style.number {
            assert_eq!(number, ExpectedNumberFormat::Currency);
            assert_eq!(cell.style.properties.number, Some(NumberFormat::Currency));
        }
        if let Some(currency) = &style.currency {
            assert_eq!(
                cell.style.properties.currency.as_deref(),
                Some(currency.as_str())
            );
        }
        if let Some(decimals) = style.decimals {
            assert_eq!(cell.style.properties.decimals, Some(decimals));
        }
        if let Some(align) = style.align {
            assert_eq!(align, ExpectedAlignment::Right);
            assert_eq!(
                cell.style.properties.align,
                Some(HorizontalAlignment::Right)
            );
        }
    }
    if let Some(geometry) = &expected.geometry {
        require_layer(layers, Layer::Geometry, coordinate);
        assert_eq!(
            cell.column.size.map(f64::to_bits),
            Some(geometry.column_width.to_bits())
        );
        assert_eq!(
            cell.row.size.map(f64::to_bits),
            Some(geometry.row_height.to_bits())
        );
    }
    if let Some(source_link) = expected.source_link {
        require_layer(layers, Layer::SourceLinks, coordinate);
        match (source_link, &cell.source) {
            (SourceLink::CsvField, CellSource::Authored { source_span, .. }) => {
                assert!(source_span.is_some());
            }
            (
                SourceLink::FillDirective,
                CellSource::VirtualFill {
                    fill_source_span, ..
                },
            ) => assert!(fill_source_span.is_some()),
            _ => panic!("{coordinate} source-link kind does not match source"),
        }
    }
}

fn edit_operation(edit: &SetCellExpectation) -> EditOperation {
    match edit {
        SetCellExpectation::SetCell {
            sheet: expected_sheet,
            coordinate: expected_coordinate,
            value,
        } => {
            let value = match value {
                ExpectedValue::Number { value } => Value::Number(*value),
                ExpectedValue::Text { value } => Value::Text(value.clone()),
                ExpectedValue::Formula { source } => Value::Formula(
                    marksheet_model::FormulaSource::new(source.clone()).expect("fixture formula"),
                ),
            };
            EditOperation::SetCell {
                sheet: sheet(expected_sheet),
                coordinate: coordinate(expected_coordinate),
                value,
            }
        }
    }
}

fn validate_host_only(values: &[HostOnlyAssertion], allowed: &[HostOnlyAssertion]) {
    assert!(
        values.iter().all(|item| allowed.contains(item)),
        "unsupported assertion not valid here"
    );
}

fn require_layer(layers: &[Layer], expected: Layer, coordinate: &str) {
    assert!(
        layers.contains(&expected),
        "fixture expectation for {coordinate} requires undeclared layer {expected:?}"
    );
}

#[allow(clippy::too_many_arguments)] // One parameter per visible-region fixture field.
fn execute_visible(
    session: &mut WorkbenchSession,
    requested_sheet: &str,
    requested_range: &str,
    expected_layers: &[Layer],
    expected_cells: &BTreeMap<String, ExpectedCell>,
    expected_authored: &[String],
    expected_absent: &[String],
    budget: Option<&SparseBudget>,
    unsupported: &[HostOnlyAssertion],
) {
    assert_eq!(expected_layers.len(), STANDARD_LAYERS.len());
    for layer in STANDARD_LAYERS {
        assert!(
            expected_layers.contains(&layer),
            "reference visible-region fixture omits standard layer {layer:?}"
        );
    }
    let requested_range = range(requested_range);
    for coordinate_text in expected_cells
        .keys()
        .chain(expected_authored.iter())
        .chain(expected_absent.iter())
    {
        assert!(
            requested_range.contains(coordinate(coordinate_text)),
            "fixture coordinate {coordinate_text} lies outside the requested range"
        );
    }
    validate_host_only(
        unsupported,
        &[
            HostOnlyAssertion::MaxRenderedGridCells,
            HostOnlyAssertion::MaxCoordinateProbes,
        ],
    );
    let region = session
        .visible_region(requested_sheet, requested_range)
        .unwrap();
    if let Some(budget) = budget {
        assert!(region.cells.len() <= budget.max_returned_cells);
        assert!(unsupported.contains(&HostOnlyAssertion::MaxRenderedGridCells));
        assert!(unsupported.contains(&HostOnlyAssertion::MaxCoordinateProbes));
        assert!(budget.max_rendered_grid_cells > 0 && budget.max_coordinate_probes > 0);
    }
    for (coordinate_text, expectation) in expected_cells {
        let cell = region
            .cells
            .iter()
            .find(|cell| cell.coordinate == coordinate(coordinate_text))
            .unwrap_or_else(|| panic!("missing projected fixture cell {coordinate_text}"));
        assert_expected_cell(cell, expectation, coordinate_text, expected_layers);
        if let Some(calculated) = &expectation.calculated {
            require_layer(expected_layers, Layer::Calculated, coordinate_text);
            let calculation = session.calculate(requested_sheet, requested_range).unwrap();
            assert_scalar(
                calculated_by_coordinate(&calculation, coordinate_text),
                calculated,
                coordinate_text,
            );
        }
        if expectation.set_cell_outcome == Some(SetCellOutcome::VirtualCellRefused) {
            let before = session.source_bytes().to_vec();
            let error = session
                .edit(&EditTransaction::single(EditOperation::SetCell {
                    sheet: sheet(requested_sheet),
                    coordinate: coordinate(coordinate_text),
                    value: Value::Number(99.0),
                }))
                .unwrap_err();
            assert_eq!(error.code, WorkerErrorCode::Edit);
            assert_eq!(session.source_bytes(), before.as_slice());
        }
    }
    for coordinate_text in expected_authored {
        assert!(matches!(
            region
                .cells
                .iter()
                .find(|cell| cell.coordinate == coordinate(coordinate_text))
                .map(|cell| &cell.source),
            Some(CellSource::Authored { .. })
        ));
    }
    for coordinate_text in expected_absent {
        assert!(
            !region
                .cells
                .iter()
                .any(|cell| cell.coordinate == coordinate(coordinate_text))
        );
    }
}

fn envelope(request_id: &str, revision: u64, request: WorkerRequest) -> RequestEnvelope {
    RequestEnvelope {
        protocol: PROTOCOL_VERSION.to_owned(),
        request_id: request_id.to_owned(),
        revision,
        request,
    }
}

#[test]
fn view_fixtures_are_strictly_deserialized_and_executed() {
    let manifest = read_manifest();
    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.protocol, FIXTURE_PROTOCOL);
    assert_eq!(manifest.cases.len(), 6);
    let mut seen = BTreeMap::new();
    for case in &manifest.cases {
        assert!(
            seen.insert(&case.id, case.kind).is_none(),
            "duplicate fixture id"
        );
        let fixture = read_fixture(case);
        match case.kind {
            FixtureKind::WorkbookView
            | FixtureKind::LayerProjection
            | FixtureKind::SparseViewport
            | FixtureKind::DiagnosticSource => run_session_fixture(case, fixture),
            FixtureKind::WorkerProtocol => run_worker_fixture(fixture),
            FixtureKind::ExternalChange => run_external_fixture(fixture),
        }
    }
}

#[allow(clippy::too_many_lines)] // The match intentionally documents every fixture operation.
fn run_session_fixture(case: &ManifestCase, fixture: Fixture) {
    let source_name = fixture.source.as_deref().expect("session fixture source");
    let source = fixture_source(source_name);
    let mut session = WorkbenchSession::open(source.clone(), SessionLimits::default()).unwrap();
    for operation in fixture.operations {
        match operation {
            Operation::Open { expect } => {
                if let Some(revision) = expect.revision {
                    assert_eq!(session.revision(), revision);
                }
                if !expect.sheet_tabs.is_empty() {
                    assert_eq!(
                        session.snapshot().sheets.len(),
                        expect.sheet_tabs.len(),
                        "{} sheet tab count",
                        case.id
                    );
                    for (actual, expected) in
                        session.snapshot().sheets.iter().zip(&expect.sheet_tabs)
                    {
                        assert_eq!((&actual.id, &actual.label), (&expected.id, &expected.label));
                    }
                }
                assert!(expect.unsupported_required_extensions.is_empty());
                if expect.valid_editable_workbook == Some(false) {
                    let diagnostic = expect
                        .diagnostics
                        .first()
                        .expect("invalid fixture diagnostic");
                    let related = diagnostic
                        .related_cells
                        .first()
                        .expect("diagnostic context");
                    let region = session
                        .visible_region(
                            &related.sheet,
                            Range::single(coordinate(&related.coordinate)),
                        )
                        .unwrap();
                    let actual = region
                        .diagnostics
                        .iter()
                        .find(|actual| actual.code.as_str() == diagnostic.code)
                        .expect("expected diagnostic");
                    let start = usize::try_from(actual.primary.span.start).unwrap();
                    let end = usize::try_from(actual.primary.span.end).unwrap();
                    assert_eq!(
                        &source[start..end],
                        diagnostic.primary_source_excerpt.as_bytes()
                    );
                    assert_eq!(
                        actual
                            .context
                            .as_ref()
                            .and_then(|context| context.sheet.as_ref())
                            .map(SheetId::as_str),
                        Some(related.sheet.as_str())
                    );
                    assert_eq!(
                        actual.context.as_ref().and_then(|context| context.cell),
                        Some(coordinate(&related.coordinate))
                    );
                    assert_eq!(diagnostic.source_navigation, SourceNavigation::Available);
                    assert_eq!(
                        session.edit(&EditTransaction::default()).unwrap_err().code,
                        WorkerErrorCode::Edit
                    );
                }
            }
            Operation::VisibleRegion {
                sheet: expected_sheet,
                range: expected_range,
                expect_layers,
                expect_cells,
                expect_authored_coordinates,
                expect_absent_coordinates,
                budget,
                unsupported_assertions,
            } => execute_visible(
                &mut session,
                &expected_sheet,
                &expected_range,
                &expect_layers,
                &expect_cells,
                &expect_authored_coordinates,
                &expect_absent_coordinates,
                budget.as_ref(),
                &unsupported_assertions,
            ),
            Operation::EditAndSave { edit, expect } => {
                let operation = edit_operation(&edit);
                match expect.outcome {
                    SaveOutcome::Saved => {
                        let (changed, patches, snapshot) =
                            session.edit(&EditTransaction::single(operation)).unwrap();
                        assert!(changed);
                        assert_eq!(Some(snapshot.revision), expect.new_revision);
                        for expected in expect.changed_authored_coordinates {
                            match &edit {
                                SetCellExpectation::SetCell {
                                    sheet, coordinate, ..
                                } => assert_eq!(
                                    (&expected.sheet, &expected.coordinate),
                                    (sheet, coordinate)
                                ),
                            }
                        }
                        let focused = expect
                            .focused_source_replacement
                            .expect("saved fixture focus");
                        assert_eq!(
                            patches
                                .iter()
                                .filter(|patch| patch.replacement == focused.new.as_bytes())
                                .count(),
                            focused.count
                        );
                        assert!(patches.iter().all(|patch| {
                            let start = usize::try_from(patch.span.start).unwrap();
                            let end = usize::try_from(patch.span.end).unwrap();
                            &source[start..end] == focused.old.as_bytes()
                        }));
                        let after = fixture_source(
                            expect
                                .after_source
                                .as_deref()
                                .expect("saved fixture source"),
                        );
                        assert_eq!(session.source_bytes(), after.as_slice());
                        let calculated = expect.recalculated.expect("saved recalculation");
                        assert_eq!(calculated.kind, ScalarKind::Number);
                        let result = session
                            .calculate(
                                &calculated.sheet,
                                Range::single(coordinate(&calculated.coordinate)),
                            )
                            .unwrap();
                        assert_scalar(
                            calculated_by_coordinate(&result, &calculated.coordinate),
                            &ExpectedScalar::Number {
                                value: calculated.value,
                            },
                            &calculated.coordinate,
                        );
                    }
                    SaveOutcome::Conflict => {
                        panic!("conflict fixture must use external-change runner")
                    }
                }
            }
            unexpected => panic!("{} has inapplicable operation {unexpected:?}", case.id),
        }
    }
}

fn run_worker_fixture(fixture: Fixture) {
    assert!(fixture.source.is_none());
    let mut runtime = WorkerRuntime::new(SessionLimits::default());
    let mut replies = BTreeMap::<String, ResponseEnvelope>::new();
    for operation in fixture.operations {
        match operation {
            Operation::Request {
                request_id,
                worker_protocol,
                revision,
                kind,
                source,
                targets,
            } => {
                assert_eq!(worker_protocol, PROTOCOL_VERSION);
                let request = match kind {
                    RequestKind::Open => WorkerRequest::Open {
                        source: fixture_source(source.as_deref().expect("open source")),
                    },
                    RequestKind::ReplaceSource => WorkerRequest::ReplaceSource {
                        source: fixture_source(source.as_deref().expect("replacement source")),
                    },
                    RequestKind::Calculate => {
                        let target = targets.first().expect("calculation target");
                        let (sheet, target_coordinate) =
                            target.split_once('!').expect("sheet-qualified target");
                        WorkerRequest::Calculate {
                            sheet: sheet.to_owned(),
                            range: Range::single(coordinate(target_coordinate)),
                        }
                    }
                };
                let response = runtime.dispatch(envelope(&request_id, revision, request));
                assert_eq!(response.protocol, PROTOCOL_VERSION);
                assert_eq!(response.request_id, request_id);
                replies.insert(request_id, response);
            }
            Operation::Reply {
                request_id,
                revision,
                outcome,
                must_not_mutate_active_revision,
            } => {
                let response = replies.get(&request_id).expect("reply must follow request");
                match outcome {
                    ReplyOutcome::Opened => assert!(matches!(
                        response.response,
                        WorkerResponse::Opened { .. } | WorkerResponse::Replaced { .. }
                    )),
                    ReplyOutcome::CancelledOrStale => assert!(
                        response.revision
                            <= runtime
                                .dispatch(envelope(
                                    "revision-check",
                                    2,
                                    WorkerRequest::WorkbookSnapshot
                                ))
                                .revision
                    ),
                }
                assert_eq!(response.revision, revision);
                if must_not_mutate_active_revision {
                    assert!(
                        response.revision
                            < runtime
                                .dispatch(envelope(
                                    "revision-check",
                                    2,
                                    WorkerRequest::WorkbookSnapshot
                                ))
                                .revision
                    );
                }
            }
            Operation::AssertActiveRevision {
                revision,
                must_not_include_result_from,
            } => {
                let active = runtime.dispatch(envelope(
                    "snapshot",
                    revision,
                    WorkerRequest::WorkbookSnapshot,
                ));
                assert_eq!(active.revision, revision);
                assert!(matches!(active.response, WorkerResponse::Snapshot { .. }));
                let old = replies
                    .get(&must_not_include_result_from)
                    .expect("old response");
                assert!(old.revision < active.revision);
            }
            unexpected => panic!("worker fixture has inapplicable operation {unexpected:?}"),
        }
    }
    let parsed: ResponseEnvelope =
        serde_json::from_str(&runtime.dispatch_json("not-json")).unwrap();
    assert!(
        matches!(parsed.response, WorkerResponse::Error { error } if error.code == WorkerErrorCode::Protocol)
    );
}

fn run_external_fixture(fixture: Fixture) {
    let base = fixture_source(fixture.source.as_deref().expect("external base source"));
    let mut current = None;
    let mut runtime = WorkerRuntime::new(SessionLimits::default());
    for operation in fixture.operations {
        match operation {
            Operation::OpenLocal { expect } => {
                let response = runtime.dispatch(envelope(
                    "open-local",
                    0,
                    WorkerRequest::Open {
                        source: base.clone(),
                    },
                ));
                assert_eq!(response.revision, expect.revision);
                assert_eq!(fixture_source(&expect.base_snapshot), base);
            }
            Operation::SimulateExternalReplace { current_source } => {
                current = Some(fixture_source(&current_source));
            }
            Operation::EditAndSave { edit, expect } => {
                assert_eq!(expect.outcome, SaveOutcome::Conflict);
                validate_host_only(&expect.unsupported_assertions, &[HostOnlyAssertion::Writes]);
                assert!(
                    expect
                        .unsupported_assertions
                        .contains(&HostOnlyAssertion::Writes)
                );
                assert_eq!(expect.writes, Some(0));
                assert_eq!(expect.diagnostic_kind, Some(DiagnosticKind::Conflict));
                let current = current.as_ref().expect("external replacement");
                let error = EditTransaction::single(edit_operation(&edit))
                    .expecting_source(&base)
                    .execute(current)
                    .unwrap_err();
                assert_eq!(error.kind, EditErrorKind::Conflict);
                assert_eq!(
                    fixture_source(expect.active_source.as_deref().expect("active source")),
                    *current
                );
                let replaced = runtime.dispatch(envelope(
                    "external-replace",
                    1,
                    WorkerRequest::ReplaceSource {
                        source: current.clone(),
                    },
                ));
                assert_eq!(replaced.revision, 2);
                let active =
                    runtime.dispatch(envelope("active-source", 2, WorkerRequest::SourceBytes));
                assert!(
                    matches!(active.response, WorkerResponse::SourceBytes { source } if source == *current)
                );
            }
            unexpected => panic!("external fixture has inapplicable operation {unexpected:?}"),
        }
    }
}
