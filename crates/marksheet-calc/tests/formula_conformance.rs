//! Executable checks for the repository-owned formula conformance corpus.
//!
//! The JSON files under `tests/formula` are deliberately external to the calc
//! crate. Keeping this runner here makes the public formula profile executable
//! without making the corpus an implementation detail of parser or evaluator
//! unit tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use marksheet_calc::eval::{
    CalcValue, EvaluationContext, RectangularRange, ResolvedValue, evaluate_with_defaults,
};
use marksheet_calc::formula::{
    A1Reference, Expr, ExprKind, Formula, Literal, ParseLimits, Reference, StructuredReference,
    TableRegion, format_formula, parse,
};
use marksheet_calc::{CalcEngine, CalcLimits, CalculationRequest, ReferenceCalcEngine};
use marksheet_model::{ByteSpan, CellError, Coordinate};
use serde::Deserialize;
use time::{Date, OffsetDateTime, format_description::well_known::Rfc3339};

const FORMULA_SCHEMA: &str = "marksheet.formula-conformance@1";
const SCENARIO_SCHEMA: &str = "marksheet.calculation-scenario@1";
const PORTABLE_A1_PROFILE: &str = "portable-a1@1";
const DEFAULT_SHEET: &str = "main";

#[test]
fn formula_conformance_corpus() {
    let parser_documents = formula_documents("parser");
    let evaluation_documents = formula_documents("eval");
    let format_documents = formula_documents("format");
    assert!(
        !parser_documents.is_empty(),
        "formula conformance corpus contains no parser documents"
    );
    assert!(
        !evaluation_documents.is_empty(),
        "formula conformance corpus contains no evaluation documents"
    );
    assert!(
        !format_documents.is_empty(),
        "formula conformance corpus contains no canonical-format documents"
    );

    let mut identifiers = BTreeSet::new();
    for (path, document) in parser_documents {
        run_formula_document(&path, &document, DocumentKind::Parser, &mut identifiers);
    }
    for (path, document) in evaluation_documents {
        run_formula_document(&path, &document, DocumentKind::Evaluation, &mut identifiers);
    }
    for (path, document) in format_documents {
        run_formula_document(&path, &document, DocumentKind::Format, &mut identifiers);
    }
}

/// Runs every source-level calculation scenario through the reference engine.
#[test]
fn calculation_scenario_corpus() {
    let documents = scenario_documents();
    assert!(
        !documents.is_empty(),
        "calculation scenario corpus contains no .calc.json documents"
    );

    let declared_sources = documents
        .iter()
        .map(|(path, document)| {
            path.parent()
                .expect("scenario document has a parent directory")
                .join(&document.source)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        declared_sources.len(),
        documents.len(),
        "multiple scenario documents reference the same .ms source"
    );
    let source_files = source_files(&corpus_root().join("scenarios"));
    assert_eq!(
        declared_sources, source_files,
        "every scenario .ms source must have exactly one matching .calc.json document"
    );

    for (path, document) in documents {
        run_scenario_document(&path, &document);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentKind {
    Parser,
    Evaluation,
    Format,
}

fn run_formula_document(
    path: &Path,
    document: &FormulaDocument,
    kind: DocumentKind,
    identifiers: &mut BTreeSet<String>,
) {
    assert_eq!(
        document.schema,
        FORMULA_SCHEMA,
        "{}: unsupported formula schema",
        path.display()
    );
    assert_eq!(
        document.profile,
        PORTABLE_A1_PROFILE,
        "{}: unsupported formula profile",
        path.display()
    );
    assert!(
        !document.cases.is_empty(),
        "{}: formula document has no cases",
        path.display()
    );

    for case in &document.cases {
        run_formula_case(path, case, kind, identifiers);
    }
}

fn run_formula_case(
    path: &Path,
    case: &FormulaCase,
    kind: DocumentKind,
    identifiers: &mut BTreeSet<String>,
) {
    validate_formula_case(path, case, identifiers);
    let expectation = case
        .expectation()
        .unwrap_or_else(|error| panic!("{}: {}: {error}", path.display(), case.id));
    assert_formula_expectation(path, case, kind, expectation);
}

fn validate_formula_case(path: &Path, case: &FormulaCase, identifiers: &mut BTreeSet<String>) {
    assert!(
        is_case_identifier(&case.id),
        "{}: invalid case identifier {:?}",
        path.display(),
        case.id
    );
    assert!(
        identifiers.insert(case.id.clone()),
        "duplicate formula conformance case id {:?}",
        case.id
    );
    assert!(
        case.formula.starts_with('='),
        "{}: {} does not begin with '='",
        path.display(),
        case.id
    );
}

fn assert_formula_expectation(
    path: &Path,
    case: &FormulaCase,
    kind: DocumentKind,
    expectation: CaseExpectation<'_>,
) {
    match (kind, expectation) {
        (DocumentKind::Parser, CaseExpectation::Ast(expected)) => {
            let formula = parse_case_formula(path, case);
            assert_eq!(
                normalized_expression(&formula.expression),
                expected,
                "{}: {} parsed to a different normalized AST",
                path.display(),
                case.id
            );
        }
        (DocumentKind::Parser, CaseExpectation::Diagnostic(code)) => {
            assert_eq!(
                code,
                marksheet_calc::formula::FORMULA_SYNTAX_DIAGNOSTIC,
                "{}: {} uses an unsupported parser diagnostic",
                path.display(),
                case.id
            );
            assert!(
                parse(&case.formula, &ParseLimits::default()).is_err(),
                "{}: {} expected {code} but parsed successfully",
                path.display(),
                case.id
            );
        }
        (DocumentKind::Evaluation, CaseExpectation::Value(expected)) => {
            let formula = parse_case_formula(path, case);
            let context = CorpusContext::from_case(case).unwrap_or_else(|error| {
                panic!(
                    "{}: {}: invalid evaluation context: {error}",
                    path.display(),
                    case.id
                )
            });
            let actual = evaluate_with_defaults(&formula, &context)
                .unwrap_or_else(|error| {
                    panic!(
                        "{}: {}: evaluation failed operationally: {error}",
                        path.display(),
                        case.id
                    )
                })
                .value;
            let expected = expected.to_calc_value().unwrap_or_else(|error| {
                panic!(
                    "{}: {}: invalid expected value: {error}",
                    path.display(),
                    case.id
                )
            });
            assert_calc_values_equal(path, &case.id, &actual, &expected);
        }
        (DocumentKind::Format, CaseExpectation::Canonical(expected)) => {
            let formula = parse_case_formula(path, case);
            let actual = format_formula(&formula).unwrap_or_else(|error| {
                panic!(
                    "{}: {} could not be formatted canonically: {error}",
                    path.display(),
                    case.id
                )
            });
            assert_eq!(
                actual,
                expected,
                "{}: {} formatted to a different canonical formula",
                path.display(),
                case.id
            );
        }
        (DocumentKind::Parser, other) => panic!(
            "{}: {} is a parser case but has {other:?} expectation",
            path.display(),
            case.id
        ),
        (DocumentKind::Evaluation, other) => panic!(
            "{}: {} is an evaluation case but has {other:?} expectation",
            path.display(),
            case.id
        ),
        (DocumentKind::Format, other) => panic!(
            "{}: {} is a canonical-format case but has {other:?} expectation",
            path.display(),
            case.id
        ),
    }
}

fn parse_case_formula(path: &Path, case: &FormulaCase) -> Formula {
    parse(&case.formula, &ParseLimits::default()).unwrap_or_else(|error| {
        panic!(
            "{}: {} unexpectedly failed to parse as MS2202: {error}",
            path.display(),
            case.id
        )
    })
}

fn assert_calc_values_equal(path: &Path, case_id: &str, actual: &CalcValue, expected: &CalcValue) {
    match (actual, expected) {
        (CalcValue::Number(actual), CalcValue::Number(expected)) => assert!(
            actual.to_bits() == expected.to_bits(),
            "{}: {} expected number {expected:?}, got {actual:?}",
            path.display(),
            case_id
        ),
        _ => assert_eq!(
            actual,
            expected,
            "{}: {} produced a different typed value",
            path.display(),
            case_id
        ),
    }
}

fn formula_documents(directory: &str) -> Vec<(PathBuf, FormulaDocument)> {
    let root = corpus_root().join(directory);
    let paths = json_files(&root, |path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths
        .into_iter()
        .map(|path| {
            let document = deserialize_document::<FormulaDocument>(&path);
            (path, document)
        })
        .collect()
}

fn scenario_documents() -> Vec<(PathBuf, ScenarioDocument)> {
    let root = corpus_root().join("scenarios");
    let paths = json_files(&root, |path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".calc.json"))
    });
    paths
        .into_iter()
        .map(|path| {
            let document = deserialize_document::<ScenarioDocument>(&path);
            (path, document)
        })
        .collect()
}

fn source_files(root: &Path) -> BTreeSet<PathBuf> {
    let mut paths = Vec::new();
    collect_matching_files(
        root,
        &|path| path.extension().is_some_and(|extension| extension == "ms"),
        &mut paths,
    );
    paths.into_iter().collect()
}

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/formula")
}

fn json_files(predicate_root: &Path, include: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_matching_files(predicate_root, &include, &mut paths);
    paths.sort();
    paths
}

fn collect_matching_files(root: &Path, include: &dyn Fn(&Path) -> bool, paths: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", root.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("cannot read corpus directory entry: {error}"))
            .path();
        if path.is_dir() {
            collect_matching_files(&path, include, paths);
        } else if path.is_file() && include(&path) {
            paths.push(path);
        }
    }
}

fn deserialize_document<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&source).unwrap_or_else(|error| {
        panic!(
            "{} is not a valid conformance document: {error}",
            path.display()
        )
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormulaDocument {
    schema: String,
    profile: String,
    cases: Vec<FormulaCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormulaCase {
    id: String,
    #[allow(dead_code)]
    description: Option<String>,
    formula: String,
    sheet: Option<String>,
    cell: Option<String>,
    #[serde(default)]
    cells: BTreeMap<String, TypedValue>,
    expect: FormulaExpectation,
}

impl FormulaCase {
    fn expectation(&self) -> Result<CaseExpectation<'_>, String> {
        let mut alternatives = 0;
        if self.expect.ast.is_some() {
            alternatives += 1;
        }
        if self.expect.diagnostic.is_some() {
            alternatives += 1;
        }
        if self.expect.value.is_some() {
            alternatives += 1;
        }
        if self.expect.canonical.is_some() {
            alternatives += 1;
        }
        if alternatives != 1 {
            return Err(
                "expect must contain exactly one of ast, diagnostic, value, or canonical"
                    .to_owned(),
            );
        }
        if let Some(ast) = &self.expect.ast {
            return Ok(CaseExpectation::Ast(ast));
        }
        if let Some(diagnostic) = &self.expect.diagnostic {
            return Ok(CaseExpectation::Diagnostic(diagnostic));
        }
        if let Some(canonical) = &self.expect.canonical {
            return Ok(CaseExpectation::Canonical(canonical));
        }
        Ok(CaseExpectation::Value(
            self.expect
                .value
                .as_ref()
                .expect("one expectation was counted"),
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormulaExpectation {
    ast: Option<String>,
    diagnostic: Option<String>,
    value: Option<TypedValue>,
    canonical: Option<String>,
}

#[derive(Debug)]
enum CaseExpectation<'a> {
    Ast(&'a str),
    Diagnostic(&'a str),
    Value(&'a TypedValue),
    Canonical(&'a str),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum TypedValue {
    Blank,
    Text(String),
    Number(f64),
    Boolean(bool),
    Date(String),
    #[serde(rename = "datetime")]
    DateTime(String),
    Error(String),
}

impl TypedValue {
    fn to_calc_value(&self) -> Result<CalcValue, String> {
        match self {
            Self::Blank => Ok(CalcValue::Blank),
            Self::Text(value) => Ok(CalcValue::Text(value.clone())),
            Self::Number(value) if value.is_finite() => Ok(CalcValue::Number(*value)),
            Self::Number(_) => Err("numbers must be finite".to_owned()),
            Self::Boolean(value) => Ok(CalcValue::Boolean(*value)),
            Self::Date(value) => parse_date(value).map(CalcValue::Date),
            Self::DateTime(value) => OffsetDateTime::parse(value, &Rfc3339)
                .map(CalcValue::DateTime)
                .map_err(|error| format!("invalid RFC 3339 datetime {value:?}: {error}")),
            Self::Error(value) => CellError::parse(value)
                .map(CalcValue::Error)
                .ok_or_else(|| format!("unknown spreadsheet error {value:?}")),
        }
    }
}

fn parse_date(value: &str) -> Result<Date, String> {
    let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
        .map_err(|error| format!("cannot construct date parser: {error}"))?;
    Date::parse(value, &format).map_err(|error| format!("invalid ISO date {value:?}: {error}"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioDocument {
    schema: String,
    profile: String,
    source: String,
    expect: ScenarioExpectation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioExpectation {
    cells: BTreeMap<String, TypedValue>,
    diagnostics: Vec<ScenarioDiagnostic>,
    source_invariants: Option<SourceInvariants>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioDiagnostic {
    code: String,
    cells: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceInvariants {
    #[allow(dead_code)]
    virtual_formulas_only: bool,
}

/// A deterministic context for one independent evaluation conformance case.
///
/// Every sheet mentioned by an input is known. Cells omitted from a known sheet
/// resolve to Blank, while an unknown qualified sheet resolves to `#REF!`.
/// This matches authored A1 range behavior without treating empty text as a
/// missing cell.
#[derive(Debug)]
struct CorpusContext {
    current_sheet: String,
    sheets: BTreeSet<String>,
    cells: BTreeMap<(String, Coordinate), CalcValue>,
}

impl CorpusContext {
    fn from_case(case: &FormulaCase) -> Result<Self, String> {
        let cell_sheet = case
            .cell
            .as_deref()
            .and_then(|cell| cell.split_once('!').map(|(sheet, _)| sheet.to_owned()));
        let current_sheet = case
            .sheet
            .clone()
            .or(cell_sheet)
            .unwrap_or_else(|| DEFAULT_SHEET.to_owned());
        validate_sheet(&current_sheet)?;
        if let Some(cell) = &case.cell {
            let (cell_sheet, _) = parse_input_cell(cell, &current_sheet)
                .map_err(|error| format!("invalid formula cell {cell:?}: {error}"))?;
            if cell_sheet != current_sheet {
                return Err(format!(
                    "formula cell {cell:?} conflicts with current sheet {current_sheet:?}"
                ));
            }
        }

        let mut context = Self {
            current_sheet: current_sheet.clone(),
            sheets: BTreeSet::from([current_sheet]),
            cells: BTreeMap::new(),
        };
        for (address, value) in &case.cells {
            let (sheet, coordinate) = parse_input_cell(address, &context.current_sheet)?;
            context.sheets.insert(sheet.clone());
            let value = value.to_calc_value()?;
            if context.cells.insert((sheet, coordinate), value).is_some() {
                return Err(format!("duplicate cell input {address:?}"));
            }
        }
        Ok(context)
    }

    fn value_at(&self, sheet: &str, coordinate: Coordinate) -> Result<CalcValue, CellError> {
        if !self.sheets.contains(sheet) {
            return Err(CellError::Reference);
        }
        Ok(self
            .cells
            .get(&(sheet.to_owned(), coordinate))
            .cloned()
            .unwrap_or(CalcValue::Blank))
    }

    fn resolve_range(
        &self,
        sheet: &str,
        start: Coordinate,
        end: Coordinate,
    ) -> Result<RectangularRange, CellError> {
        if !self.sheets.contains(sheet) {
            return Err(CellError::Reference);
        }
        let first_row = start.row.min(end.row);
        let last_row = start.row.max(end.row);
        let first_column = start.column.min(end.column);
        let last_column = start.column.max(end.column);
        let row_count = last_row
            .checked_sub(first_row)
            .and_then(|length| length.checked_add(1))
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(CellError::Reference)?;
        let column_count = last_column
            .checked_sub(first_column)
            .and_then(|length| length.checked_add(1))
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(CellError::Reference)?;
        let capacity = row_count
            .checked_mul(column_count)
            .ok_or(CellError::Reference)?;
        let mut values = Vec::with_capacity(capacity);
        for row in first_row..=last_row {
            for column in first_column..=last_column {
                values.push(self.value_at(
                    sheet,
                    Coordinate::new(column, row).expect("normalized coordinates are nonzero"),
                )?);
            }
        }
        RectangularRange::new(row_count, column_count, values).map_err(|_| CellError::Reference)
    }
}

impl EvaluationContext for CorpusContext {
    fn resolve(&self, reference: &Reference, _span: ByteSpan) -> Result<ResolvedValue, CellError> {
        match reference {
            Reference::Cell { sheet, address } => self
                .value_at(
                    sheet
                        .as_ref()
                        .map_or(&self.current_sheet, |sheet| sheet.as_str()),
                    address.coordinate,
                )
                .map(ResolvedValue::Scalar),
            Reference::Range(range) => self
                .resolve_range(
                    range
                        .sheet
                        .as_ref()
                        .map_or(&self.current_sheet, |sheet| sheet.as_str()),
                    range.start.coordinate,
                    range.end.coordinate,
                )
                .map(ResolvedValue::Range),
            Reference::Name { .. } => Err(CellError::Name),
            Reference::Structured(_) => Err(CellError::Reference),
        }
    }
}

fn parse_input_cell(value: &str, current_sheet: &str) -> Result<(String, Coordinate), String> {
    if let Some((sheet, cell)) = value.split_once('!') {
        if sheet.is_empty() || cell.contains('!') {
            return Err(format!("invalid qualified cell {value:?}"));
        }
        validate_sheet(sheet)?;
        return Coordinate::parse(cell)
            .map(|coordinate| (sheet.to_owned(), coordinate))
            .map_err(|error| format!("invalid cell {value:?}: {error}"));
    }
    Coordinate::parse(value)
        .map(|coordinate| (current_sheet.to_owned(), coordinate))
        .map_err(|error| format!("invalid cell {value:?}: {error}"))
}

fn parse_qualified_cell(value: &str) -> Result<(String, Coordinate), String> {
    let (sheet, coordinate) = value
        .split_once('!')
        .ok_or_else(|| "expected sheet!A1".to_owned())?;
    if sheet.is_empty() || coordinate.contains('!') {
        return Err("invalid qualified cell".to_owned());
    }
    validate_sheet(sheet)?;
    Coordinate::parse(coordinate)
        .map(|coordinate| (sheet.to_owned(), coordinate))
        .map_err(|error| error.to_string())
}

fn validate_sheet(sheet: &str) -> Result<(), String> {
    marksheet_model::SheetId::parse(sheet)
        .map(|_| ())
        .map_err(|error| format!("invalid sheet identifier {sheet:?}: {error}"))
}

fn is_case_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    matches!(bytes.first(), Some(byte) if byte.is_ascii_lowercase())
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(*byte, b'.' | b'_' | b'-')
        })
}

fn normalized_expression(expression: &Expr) -> String {
    match &expression.kind {
        ExprKind::Literal { value } => normalized_literal(value),
        ExprKind::Reference { reference } => normalized_reference(reference),
        ExprKind::Unary { operator, operand } => {
            format!(
                "(unary {} {})",
                operator.symbol(),
                normalized_expression(operand)
            )
        }
        ExprKind::Binary {
            operator,
            left,
            right,
        } => format!(
            "(binary {} {} {})",
            operator.symbol(),
            normalized_expression(left),
            normalized_expression(right)
        ),
        ExprKind::Call { call } => {
            let arguments = call
                .arguments
                .iter()
                .map(normalized_expression)
                .collect::<Vec<_>>();
            if arguments.is_empty() {
                format!("(call {})", call.name)
            } else {
                format!("(call {} {})", call.name, arguments.join(" "))
            }
        }
    }
}

fn normalized_literal(literal: &Literal) -> String {
    match literal {
        Literal::Number(value) => format!("(number {value})"),
        Literal::Text(value) => format!("(text {value:?})"),
        Literal::Boolean(value) => format!("(boolean {value})"),
        Literal::Error(value) => format!("(error {:?})", value.token()),
    }
}

fn normalized_reference(reference: &Reference) -> String {
    match reference {
        Reference::Cell { sheet, address } => format!(
            "(cell {} {})",
            normalized_sheet(sheet.as_ref().map(marksheet_model::SheetId::as_str)),
            normalized_a1(address)
        ),
        Reference::Range(range) => format!(
            "(range {} {} {})",
            normalized_sheet(range.sheet.as_ref().map(marksheet_model::SheetId::as_str)),
            normalized_a1(&range.start),
            normalized_a1(&range.end)
        ),
        Reference::Name { name } => format!("(name {})", name.as_str()),
        Reference::Structured(structured) => normalized_structured_reference(structured),
    }
}

fn normalized_sheet(sheet: Option<&str>) -> &str {
    sheet.unwrap_or("-")
}

fn normalized_a1(address: &A1Reference) -> String {
    format!(
        "{}{}{}{}",
        if address.column_absolute { "$" } else { "" },
        address.coordinate.column_name(),
        if address.row_absolute { "$" } else { "" },
        address.coordinate.row
    )
}

fn normalized_structured_reference(reference: &StructuredReference) -> String {
    match reference {
        StructuredReference::Column { table, header } => format!(
            "(table-column {} {})",
            table.as_str(),
            normalized_header(header)
        ),
        StructuredReference::Region { table, region } => format!(
            "(table-region {} {})",
            table.as_str(),
            match region {
                TableRegion::Headers => "headers",
                TableRegion::Data => "data",
            }
        ),
        StructuredReference::CurrentRow { table, header } => format!(
            "(current-row {} {})",
            table.as_ref().map_or("-", marksheet_model::TableId::as_str),
            normalized_header(header)
        ),
    }
}

fn normalized_header(header: &str) -> String {
    if header.bytes().any(|byte| byte.is_ascii_whitespace()) {
        format!("{header:?}")
    } else {
        header.to_owned()
    }
}

// Keeping the source -> syntax -> engine -> asserted-result flow together
// makes it clear that each scenario document is fully exercised rather than
// only schema-validated. Its individual operations are small and named.
#[allow(clippy::too_many_lines)]
fn run_scenario_document(path: &Path, document: &ScenarioDocument) {
    assert_eq!(
        document.schema,
        SCENARIO_SCHEMA,
        "{}: unsupported scenario schema",
        path.display()
    );
    assert_eq!(
        document.profile,
        PORTABLE_A1_PROFILE,
        "{}: unsupported scenario profile",
        path.display()
    );
    assert!(
        Path::new(&document.source)
            .extension()
            .is_some_and(|extension| extension == "ms"),
        "{}: scenario source must end in .ms",
        path.display()
    );
    let source = path
        .parent()
        .expect("scenario document has a parent directory")
        .join(&document.source);
    assert!(
        source.is_file(),
        "{}: referenced scenario source {} is missing",
        path.display(),
        source.display()
    );
    assert!(
        !document.expect.cells.is_empty(),
        "{}: scenarios must assert at least one calculated cell",
        path.display()
    );
    for (address, value) in &document.expect.cells {
        parse_qualified_cell(address).unwrap_or_else(|error| {
            panic!(
                "{}: invalid expected cell {address:?}: {error}",
                path.display()
            )
        });
        value.to_calc_value().unwrap_or_else(|error| {
            panic!(
                "{}: invalid expected value for {address:?}: {error}",
                path.display()
            )
        });
    }
    for diagnostic in &document.expect.diagnostics {
        assert!(
            diagnostic.code.starts_with("MS")
                && diagnostic.code.len() == 6
                && diagnostic.code[2..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit()),
            "{}: invalid diagnostic code {:?}",
            path.display(),
            diagnostic.code
        );
        assert!(
            !diagnostic.cells.is_empty(),
            "{}: scenario diagnostic {:?} has no cells",
            path.display(),
            diagnostic.code
        );
        for cell in &diagnostic.cells {
            parse_qualified_cell(cell).unwrap_or_else(|error| {
                panic!(
                    "{}: invalid diagnostic cell {cell:?}: {error}",
                    path.display()
                )
            });
        }
    }

    let source_bytes = fs::read(&source)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", source.display()));
    let parsed = marksheet_syntax::parse(&source_bytes);
    assert!(
        !parsed.has_errors(),
        "{}: source failed to lower: {:?}",
        source.display(),
        parsed
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code.as_str(), &diagnostic.message))
            .collect::<Vec<_>>()
    );
    let workbook = parsed.workbook.as_ref().unwrap_or_else(|| {
        panic!(
            "{}: valid source did not produce a workbook",
            source.display()
        )
    });
    let source_workbook = workbook.clone();
    let engine = ReferenceCalcEngine::new();
    let report = engine.prepare(workbook, CalcLimits::default());
    let mut calculation = report.calculation.unwrap_or_else(|| {
        panic!(
            "{}: calculation preparation failed: {:?}",
            source.display(),
            report
                .diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.code.as_str(), &diagnostic.message))
                .collect::<Vec<_>>()
        )
    });

    let expected_diagnostics = document
        .expect
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code.clone(), diagnostic.cells.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        normalized_diagnostics(&report.diagnostics),
        expected_diagnostics,
        "{}: calculation diagnostics differ",
        path.display()
    );
    let expected_cells = document
        .expect
        .cells
        .iter()
        .map(|(cell, value)| {
            let value = value.to_calc_value().unwrap_or_else(|error| {
                panic!(
                    "{}: invalid expected value for {cell:?}: {error}",
                    path.display()
                )
            });
            (cell.clone(), value)
        })
        .collect::<BTreeMap<_, _>>();
    let mut requested_by_sheet = BTreeMap::<String, Vec<Coordinate>>::new();
    for cell in expected_cells.keys() {
        let (sheet, coordinate) = parse_qualified_cell(cell).unwrap_or_else(|error| {
            panic!(
                "{}: invalid expected cell {cell:?}: {error}",
                path.display()
            )
        });
        requested_by_sheet
            .entry(sheet)
            .or_default()
            .push(coordinate);
    }

    let mut actual_cells = BTreeMap::new();
    for (sheet, coordinates) in requested_by_sheet {
        let request = CalculationRequest::new(
            marksheet_model::SheetId::parse(&sheet).unwrap_or_else(|error| {
                panic!("{}: invalid sheet {sheet:?}: {error}", path.display())
            }),
            bounding_range(&coordinates).expect("each sheet has at least one expected cell"),
        );
        let result = engine.calculate(&mut calculation, &request);
        assert_eq!(
            normalized_diagnostics(&result.diagnostics),
            expected_diagnostics,
            "{}: calculation diagnostics changed while selecting {sheet}",
            path.display()
        );
        for calculated in result.cells {
            actual_cells.insert(calculated.cell.to_string(), calculated.value);
        }
    }
    let actual_expected_cells = expected_cells
        .keys()
        .map(|cell| {
            let actual = actual_cells.get(cell).unwrap_or_else(|| {
                panic!(
                    "{}: calculation did not return expected cell {cell}",
                    path.display()
                )
            });
            (cell.clone(), actual.clone())
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual_expected_cells,
        expected_cells,
        "{}: calculated cells differ",
        path.display()
    );
    if let Some(invariants) = &document.expect.source_invariants {
        assert!(
            invariants.virtual_formulas_only,
            "{}: unsupported source invariant value",
            path.display()
        );
        assert_eq!(
            workbook,
            &source_workbook,
            "{}: calculation mutated source-authored workbook data",
            path.display()
        );
    }
}

fn bounding_range(coordinates: &[Coordinate]) -> Option<marksheet_model::Range> {
    let first = *coordinates.first()?;
    Some(coordinates.iter().skip(1).fold(
        marksheet_model::Range::single(first),
        |range, coordinate| marksheet_model::Range::new(range.start, *coordinate),
    ))
}

fn normalized_diagnostics(
    diagnostics: &[marksheet_model::Diagnostic],
) -> Vec<(String, Vec<String>)> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let mut cells = Vec::new();
            if let Some(context) = &diagnostic.context {
                if let (Some(sheet), Some(cell)) = (&context.sheet, context.cell) {
                    cells.push(format!("{sheet}!{cell}"));
                }
            }
            cells.extend(
                diagnostic
                    .related
                    .iter()
                    .filter_map(|related| related.span.label.clone()),
            );
            (diagnostic.code.as_str().to_owned(), cells)
        })
        .collect()
}
