//! Diagnostic rendering helpers.
//!
//! Keeping presentation separate from command execution makes the JSON shape a
//! stable, machine-facing contract and lets `fmt` reuse the human renderer.

use std::{
    io::{self, Write},
    path::Path,
};

use marksheet_model::{ByteSpan, Diagnostic, LineIndex, Severity};
use serde::Serialize;

use crate::OutputFormat;

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
