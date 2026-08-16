//! Command implementations.
//!
//! This module owns filesystem effects and exit-status policy. Formatting is
//! deliberately parse-first so malformed input is never overwritten.

use std::{
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
};

use marksheet_model::{
    ByteSpan, Diagnostic, DiagnosticCode, LabeledSpan, Range, Severity, SheetId,
};

use crate::{CalcOutputFormat, DiffOutputFormat, OutputFormat};

/// Runs `marksheet check`.
pub(crate) fn check(path: &Path, format: OutputFormat) -> Result<ExitCode, CliError> {
    let source = read_source(path)?;
    let document = marksheet_syntax::parse(&source);
    let mut diagnostics = document.diagnostics.clone();
    if !document.has_errors()
        && let Some(workbook) = document.workbook.as_ref()
    {
        diagnostics.extend(validate_formulas(workbook));
    }

    crate::render::render(path, document.source_bytes(), &diagnostics, format)
        .map_err(CliError::Render)?;

    Ok(exit_for_diagnostics(&diagnostics))
}

/// Validates the calculation-specific portion of an otherwise valid workbook.
/// The source parser intentionally retains formulas as opaque fields, so this
/// second pass is what makes `marksheet check` reject invalid formula syntax
/// and unresolved formula references without calculating a selected range.
fn validate_formulas(workbook: &marksheet_model::Workbook) -> Vec<Diagnostic> {
    use marksheet_calc::CalcEngine;

    marksheet_calc::ReferenceCalcEngine::new()
        .prepare(workbook, marksheet_calc::CalcLimits::default())
        .diagnostics
}

/// Runs `marksheet calc` for one explicit sheet selection.
pub(crate) fn calculate(
    path: &Path,
    sheet: SheetId,
    range: Range,
    format: CalcOutputFormat,
) -> Result<ExitCode, CliError> {
    use marksheet_calc::engine::CalcEngine;

    let source = read_source(path)?;
    let document = marksheet_syntax::parse(&source);
    if document.has_errors() {
        crate::render::render_human(path, document.source_bytes(), &document.diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }
    let workbook = document
        .workbook
        .as_ref()
        .ok_or(CliError::MissingWorkbook)?;

    let engine = marksheet_calc::engine::ReferenceCalcEngine::new();
    let report = engine.prepare(workbook, marksheet_calc::engine::CalcLimits::default());
    let Some(mut calculation) = report.calculation else {
        crate::render::render_human(path, document.source_bytes(), &report.diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    };

    let request = marksheet_calc::engine::CalculationRequest::new(sheet, range);
    let result = engine.calculate(&mut calculation, &request);
    let expected_cells = range_cell_count(range)?;
    if result.cells.len() as u64 != expected_cells {
        crate::render::render_human(path, document.source_bytes(), &result.diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }

    let output = crate::render::format_calculation(&request, &result, format)
        .map_err(CliError::Serialize)?;
    crate::render::print_stdout(&output).map_err(CliError::Render)?;
    if !result.diagnostics.is_empty() {
        crate::render::render_human(path, document.source_bytes(), &result.diagnostics)
            .map_err(CliError::Render)?;
    }

    Ok(exit_for_diagnostics(&result.diagnostics))
}

/// Runs `marksheet diff`.
///
/// Comparison is intentionally gated on both complete, formula-valid workbook
/// projections. This avoids a tempting but incorrect fallback to source text
/// when one side is malformed or asks for unsupported calculation semantics.
pub(crate) fn diff(
    old_path: &Path,
    new_path: &Path,
    format: DiffOutputFormat,
) -> Result<ExitCode, CliError> {
    let old_source = read_source(old_path)?;
    let new_source = read_source(new_path)?;
    let old = marksheet_syntax::parse(&old_source);
    let new = marksheet_syntax::parse(&new_source);

    let mut diagnostics = Vec::new();
    collect_document_diagnostics(old_path, &old, &mut diagnostics);
    collect_document_diagnostics(new_path, &new, &mut diagnostics);
    if !diagnostics.is_empty() {
        crate::render::render_diff_diagnostics(&diagnostics, format).map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }

    // `collect_document_diagnostics` established that both documents have a
    // complete workbook projection before semantic comparison begins.
    let old_workbook = old
        .workbook
        .as_ref()
        .expect("complete diff input has a workbook projection");
    let new_workbook = new
        .workbook
        .as_ref()
        .expect("complete diff input has a workbook projection");
    let semantic_diff = marksheet_edit::diff::SemanticDiff::between(old_workbook, new_workbook);
    let unsupported: Vec<_> = semantic_diff
        .changes
        .iter()
        .filter_map(|change| match change {
            marksheet_edit::diff::SemanticChange::UnsupportedComparison(issue) => Some(issue),
            _ => None,
        })
        .collect();
    if !unsupported.is_empty() {
        let diagnostics = unsupported
            .into_iter()
            .map(|issue| crate::render::DiffDiagnostic {
                path: old_path,
                source: old.source_bytes(),
                diagnostic: unsupported_comparison_diagnostic(issue),
            })
            .collect::<Vec<_>>();
        crate::render::render_diff_diagnostics(&diagnostics, format).map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }

    let output =
        crate::render::format_semantic_diff(&semantic_diff, format).map_err(CliError::Serialize)?;
    crate::render::print_stdout(&output).map_err(CliError::Render)?;
    if semantic_diff.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        // Deliberately match conventional `diff`: a real difference is a
        // useful nonzero condition, while I/O and serialization failures are
        // reserved for the top-level exit status 2.
        Ok(ExitCode::from(1))
    }
}

fn collect_document_diagnostics<'a>(
    path: &'a Path,
    document: &'a marksheet_syntax::ParsedDocument,
    output: &mut Vec<crate::render::DiffDiagnostic<'a>>,
) {
    let has_syntax_errors = document.has_errors();
    output.extend(
        document
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .map(|diagnostic| crate::render::DiffDiagnostic {
                path,
                source: document.source_bytes(),
                diagnostic: diagnostic.clone(),
            }),
    );
    if !has_syntax_errors && let Some(workbook) = document.workbook.as_ref() {
        output.extend(
            validate_formulas(workbook)
                .into_iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .map(|diagnostic| crate::render::DiffDiagnostic {
                    path,
                    source: document.source_bytes(),
                    diagnostic,
                }),
        );
    } else if !has_syntax_errors {
        output.push(crate::render::DiffDiagnostic {
            path,
            source: document.source_bytes(),
            diagnostic: incomplete_projection_diagnostic(),
        });
    }
}

fn incomplete_projection_diagnostic() -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new("MS3001").expect("registered diff diagnostic code"),
        severity: Severity::Error,
        message: "input did not produce a complete semantic workbook projection".to_owned(),
        primary: LabeledSpan {
            span: ByteSpan::default(),
            label: None,
        },
        related: Vec::new(),
        context: None,
        suggestion: None,
    }
}

fn unsupported_comparison_diagnostic(
    issue: &marksheet_edit::diff::UnsupportedComparison,
) -> Diagnostic {
    Diagnostic {
        // This diagnostic is emitted only after both parser and formula
        // validation passes succeed. It therefore describes an invariant the
        // semantic diff deliberately refuses to guess about, rather than a
        // source spelling error.
        code: DiagnosticCode::new("MS3001").expect("registered diff diagnostic code"),
        severity: Severity::Error,
        message: format!(
            "semantic comparison unsupported at {:?}: {}",
            issue.scope, issue.explanation
        ),
        primary: LabeledSpan {
            span: ByteSpan::default(),
            label: None,
        },
        related: Vec::new(),
        context: None,
        suggestion: None,
    }
}

fn range_cell_count(range: Range) -> Result<u64, CliError> {
    range
        .width()
        .ok()
        .and_then(|width| range.height().ok()?.checked_mul(width))
        .ok_or(CliError::InvalidRange(range))
}

/// Runs `marksheet fmt`.
pub(crate) fn format(path: &Path, check: bool) -> Result<ExitCode, CliError> {
    reject_symlink(path)?;
    let source = read_source(path)?;
    let document = marksheet_syntax::parse(&source);
    if document.has_errors() {
        crate::render::render_human(path, document.source_bytes(), &document.diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }
    if let Some(workbook) = document.workbook.as_ref() {
        let formula_diagnostics = validate_formulas(workbook);
        if formula_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            crate::render::render_human(path, document.source_bytes(), &formula_diagnostics)
                .map_err(CliError::Render)?;
            return Ok(ExitCode::from(1));
        }
    }

    let formatted = match marksheet_syntax::canonicalize(&document) {
        Ok(formatted) => formatted,
        Err(diagnostics) => {
            // This should normally be unreachable after `has_errors`, but the
            // formatter is allowed to reject a document if a future canonical
            // invariant cannot be represented safely.
            crate::render::render_human(path, document.source_bytes(), &diagnostics)
                .map_err(CliError::Render)?;
            return Ok(ExitCode::from(1));
        }
    };

    if check {
        if source == formatted {
            return Ok(ExitCode::SUCCESS);
        }
        crate::render::print_stderr(&format!("{} is not canonically formatted", path.display()))
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }

    if source != formatted {
        replace_atomically(path, &formatted)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn read_source(path: &Path) -> Result<Vec<u8>, CliError> {
    fs::read(path).map_err(|source| CliError::Read {
        path: path.to_owned(),
        source,
    })
}

/// Formatting replaces a directory entry, so following a symlink here would
/// replace the link itself rather than update its target. Refuse early to make
/// that surprising and potentially destructive behavior impossible.
fn reject_symlink(path: &Path) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| CliError::Read {
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CliError::SymbolicLink(path.to_owned()));
    }
    Ok(())
}

fn exit_for_diagnostics(diagnostics: &[marksheet_model::Diagnostic]) -> ExitCode {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Replaces `path` using a sibling temporary file. On filesystems where rename
/// is atomic within a directory, observers see either the old complete source
/// or the new complete source, never a partially written workbook.
fn replace_atomically(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| CliError::InvalidOutputPath(path.to_owned()))?;
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let (temporary, mut file) = create_temporary_file(parent, file_name)?;

    let write_result = (|| -> io::Result<()> {
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);

    if let Err(source) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(CliError::Write {
            path: temporary,
            source,
        });
    }

    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(CliError::Write {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

fn create_temporary_file(
    parent: &Path,
    file_name: &std::ffi::OsStr,
) -> Result<(PathBuf, fs::File), CliError> {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    let file_name = file_name.to_string_lossy();
    for _ in 0..128 {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.marksheet-{}-{sequence}.tmp",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(CliError::Write {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Err(CliError::TemporaryPath(parent.to_owned()))
}

#[derive(Debug)]
pub(crate) enum CliError {
    Read {
        path: std::path::PathBuf,
        source: io::Error,
    },
    Render(io::Error),
    Write {
        path: PathBuf,
        source: io::Error,
    },
    InvalidOutputPath(PathBuf),
    TemporaryPath(PathBuf),
    SymbolicLink(PathBuf),
    MissingWorkbook,
    InvalidRange(Range),
    Serialize(serde_json::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
            Self::Render(source) => write!(formatter, "could not write diagnostics: {source}"),
            Self::Write { path, source } => {
                write!(formatter, "could not write {}: {source}", path.display())
            }
            Self::InvalidOutputPath(path) => {
                write!(
                    formatter,
                    "{} does not name a workbook file",
                    path.display()
                )
            }
            Self::TemporaryPath(path) => write!(
                formatter,
                "could not allocate a temporary formatting file in {}",
                path.display()
            ),
            Self::SymbolicLink(path) => write!(
                formatter,
                "refusing to format symbolic link {}; format the target directly",
                path.display()
            ),
            Self::MissingWorkbook => {
                formatter.write_str("parsed document did not contain a workbook")
            }
            Self::InvalidRange(range) => write!(formatter, "invalid calculation range {range}"),
            Self::Serialize(source) => write!(
                formatter,
                "could not serialize calculation output: {source}"
            ),
        }
    }
}

impl std::error::Error for CliError {}
