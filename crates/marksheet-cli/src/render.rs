//! Diagnostic rendering helpers.
//!
//! Keeping presentation separate from command execution makes the JSON shape a
//! stable, machine-facing contract and lets `fmt` reuse the human renderer.

use std::{
    io::{self, Write},
    path::Path,
};

use marksheet_calc::{CalculationRequest, CalculationResult, eval::CalcValue};
use marksheet_model::{ByteSpan, Diagnostic, LineIndex, Severity};
use serde::Serialize;

use crate::{CalcOutputFormat, OutputFormat};

pub(crate) fn render(
    path: &Path,
    source: &[u8],
    diagnostics: &[Diagnostic],
    format: OutputFormat,
) -> io::Result<()> {
    match format {
        OutputFormat::Human => render_human(path, source, diagnostics),
        OutputFormat::Json => render_json(source, diagnostics),
    }
}

pub(crate) fn render_human(
    path: &Path,
    source: &[u8],
    diagnostics: &[Diagnostic],
) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    let line_index = std::str::from_utf8(source).ok().map(LineIndex::new);

    for diagnostic in diagnostics {
        write_location(
            &mut stderr,
            path,
            line_index.as_ref(),
            diagnostic.primary.span,
        )?;
        writeln!(
            stderr,
            ": {}[{}]: {}",
            severity_name(diagnostic.severity),
            diagnostic.code,
            diagnostic.message
        )?;
        for related in &diagnostic.related {
            write!(&mut stderr, "  note: {}", related.message)?;
            write_location(&mut stderr, path, line_index.as_ref(), related.span.span)?;
            writeln!(stderr)?;
        }
    }

    Ok(())
}

pub(crate) fn print_stderr(message: &str) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{message}")
}

/// Serializes a complete calculation result before stdout is touched. This is
/// what prevents malformed or operationally incomplete results from leaving a
/// partial CSV or JSON document in a pipeline.
pub(crate) fn format_calculation(
    request: &CalculationRequest,
    result: &CalculationResult,
    format: CalcOutputFormat,
) -> Result<String, serde_json::Error> {
    match format {
        CalcOutputFormat::Json => {
            serde_json::to_string_pretty(&JsonCalculation::new(request, result))
                .map(|output| format!("{output}\n"))
        }
        CalcOutputFormat::Csv => Ok(format_csv(request, result)),
        CalcOutputFormat::Text => Ok(format_text(request, result)),
    }
}

pub(crate) fn print_stdout(output: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())
}

fn format_csv(request: &CalculationRequest, result: &CalculationResult) -> String {
    let columns = usize::try_from(request.range.width().expect("validated selection width"))
        .expect("output limit bounds CSV width");
    let mut output = String::new();
    for row in result.cells.chunks(columns) {
        for (index, cell) in row.iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            append_csv_field(&mut output, &csv_value(&cell.value));
        }
        output.push('\n');
    }
    output
}

fn append_csv_field(output: &mut String, value: &str) {
    let quote = value.contains([',', '"', '\n', '\r']);
    if quote {
        output.push('"');
        for character in value.chars() {
            if character == '"' {
                output.push('"');
            }
            output.push(character);
        }
        output.push('"');
    } else {
        output.push_str(value);
    }
}

fn format_text(request: &CalculationRequest, result: &CalculationResult) -> String {
    let columns = usize::try_from(request.range.width().expect("validated selection width"))
        .expect("output limit bounds text width");
    let mut output = format!("{}!{}\n", request.sheet, request.range);
    for row in result.cells.chunks(columns) {
        for (index, cell) in row.iter().enumerate() {
            if index != 0 {
                output.push_str("\t|\t");
            }
            output.push_str(&text_value(&cell.value));
        }
        output.push('\n');
    }
    output
}

fn csv_value(value: &CalcValue) -> String {
    match value {
        CalcValue::Blank => String::new(),
        // CSV consumers commonly treat a text field as a formula after
        // stripping leading whitespace/control characters. Prefix an
        // apostrophe so exported text remains text when opened in a sheet.
        CalcValue::Text(value) => neutralize_csv_formula(value),
        CalcValue::Number(value) => value.to_string(),
        CalcValue::Boolean(value) => value.to_string(),
        CalcValue::Date(value) => value.to_string(),
        CalcValue::DateTime(value) => value.to_string(),
        CalcValue::Error(value) => value.to_string(),
    }
}

fn neutralize_csv_formula(value: &str) -> String {
    let first_non_prefix = value
        .chars()
        .find(|character| !character.is_whitespace() && !character.is_control());
    if matches!(first_non_prefix, Some('=' | '+' | '-' | '@')) {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

fn text_value(value: &CalcValue) -> String {
    match value {
        CalcValue::Blank => "<blank>".to_owned(),
        CalcValue::Text(value) if value.is_empty() => "\"\"".to_owned(),
        // Keep each selected row on one physical terminal row. Debug output
        // makes the controls visible without changing ordinary text output.
        CalcValue::Text(value) if value.contains(['\t', '\n', '\r']) => format!("{value:?}"),
        CalcValue::Text(value) => value.clone(),
        _ => csv_value(value),
    }
}

fn write_location(
    writer: &mut impl Write,
    path: &Path,
    line_index: Option<&LineIndex>,
    span: ByteSpan,
) -> io::Result<()> {
    write!(writer, "{}", path.display())?;
    if let Some(line_index) = line_index
        && let Ok(position) = line_index.line_column(span.start)
    {
        return write!(writer, ":{}:{}", position.line, position.column);
    }
    write!(writer, ":byte {}..{}", span.start, span.end)
}

fn render_json(source: &[u8], diagnostics: &[Diagnostic]) -> io::Result<()> {
    let line_index = std::str::from_utf8(source).ok().map(LineIndex::new);
    let diagnostics: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| JsonDiagnostic::from_diagnostic(diagnostic, line_index.as_ref()))
        .collect();
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &diagnostics).map_err(io::Error::other)?;
    writeln!(stdout)
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

#[derive(Serialize)]
struct JsonDiagnostic {
    code: String,
    severity: &'static str,
    message: String,
    primary: JsonSpan,
    related: Vec<JsonRelatedDiagnostic>,
    context: Option<marksheet_model::DiagnosticContext>,
    suggestion: Option<marksheet_model::Suggestion>,
}

impl JsonDiagnostic {
    fn from_diagnostic(diagnostic: &Diagnostic, line_index: Option<&LineIndex>) -> Self {
        Self {
            code: diagnostic.code.as_str().to_owned(),
            severity: severity_name(diagnostic.severity),
            message: diagnostic.message.clone(),
            primary: JsonSpan::new(diagnostic.primary.span, line_index),
            related: diagnostic
                .related
                .iter()
                .map(|related| JsonRelatedDiagnostic {
                    message: related.message.clone(),
                    span: JsonSpan::new(related.span.span, line_index),
                })
                .collect(),
            context: diagnostic.context.clone(),
            suggestion: diagnostic.suggestion.clone(),
        }
    }
}

#[derive(Serialize)]
struct JsonRelatedDiagnostic {
    message: String,
    span: JsonSpan,
}

#[derive(Serialize)]
struct JsonSpan {
    start: u64,
    end: u64,
    line: Option<u64>,
    column: Option<u64>,
}

impl JsonSpan {
    fn new(span: ByteSpan, line_index: Option<&LineIndex>) -> Self {
        let position = line_index.and_then(|index| index.line_column(span.start).ok());
        Self {
            start: span.start,
            end: span.end,
            line: position.map(|position| position.line),
            column: position.map(|position| position.column),
        }
    }
}

#[derive(Serialize)]
struct JsonCalculation<'a> {
    version: &'static str,
    profile: &'static str,
    selection: JsonSelection<'a>,
    cells: Vec<JsonCalculatedCell<'a>>,
    diagnostics: &'a [Diagnostic],
    revision: u64,
    stats: JsonCalcStats,
}

impl<'a> JsonCalculation<'a> {
    fn new(request: &'a CalculationRequest, result: &'a CalculationResult) -> Self {
        Self {
            version: "marksheet-calc@1",
            profile: "portable-a1@1",
            selection: JsonSelection {
                sheet: request.sheet.as_str(),
                range: request.range.to_string(),
            },
            cells: result
                .cells
                .iter()
                .map(|cell| JsonCalculatedCell {
                    coordinate: cell.cell.coordinate.to_string(),
                    value: &cell.value,
                })
                .collect(),
            diagnostics: &result.diagnostics,
            revision: result.revision,
            stats: JsonCalcStats::from(&result.stats),
        }
    }
}

/// The CLI contract publishes bounded work summaries, not unbounded internal
/// cell sets. The complete sets remain available through the Rust API.
#[derive(Serialize)]
struct JsonCalcStats {
    dirty_cell_count: usize,
    evaluated_cell_count: usize,
    evaluation_steps: usize,
    range_cells: usize,
    text_bytes: usize,
}

impl From<&marksheet_calc::CalcStats> for JsonCalcStats {
    fn from(stats: &marksheet_calc::CalcStats) -> Self {
        Self {
            dirty_cell_count: stats.dirty_cell_count,
            evaluated_cell_count: stats.evaluated_cell_count,
            evaluation_steps: stats.evaluation_steps,
            range_cells: stats.range_cells,
            text_bytes: stats.text_bytes,
        }
    }
}

#[derive(Serialize)]
struct JsonSelection<'a> {
    sheet: &'a str,
    range: String,
}

#[derive(Serialize)]
struct JsonCalculatedCell<'a> {
    coordinate: String,
    value: &'a CalcValue,
}
