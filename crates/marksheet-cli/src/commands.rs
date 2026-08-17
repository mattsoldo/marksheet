//! Command implementations.
//!
//! This module owns filesystem effects and exit-status policy. Formatting is
//! deliberately parse-first so malformed input is never overwritten.

use std::{
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
};

use marksheet_model::{
    ByteSpan, Coordinate, Diagnostic, DiagnosticCode, LabeledSpan, Range, Severity, SheetId,
    TableId,
};

use crate::{CalcOutputFormat, ConversionTarget, DiffOutputFormat, OutputFormat};

pub(crate) struct ConvertOptions<'a> {
    pub(crate) target: ConversionTarget,
    pub(crate) output: Option<&'a Path>,
    pub(crate) sheet: Option<SheetId>,
    pub(crate) label: Option<String>,
    pub(crate) range: Option<Range>,
    pub(crate) table: Option<TableId>,
    pub(crate) anchor: Option<Coordinate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceFormat {
    Marksheet,
    Xlsx,
    Csv,
}

struct CliParsedDocument {
    parsed: marksheet_syntax::ParsedDocument,
    diagnostics: Vec<Diagnostic>,
    capabilities_complete: bool,
}

/// Runs `marksheet convert` and emits exactly one machine-readable fidelity
/// report. Conversion failures are ordinary exit-one outcomes; filesystem and
/// output-stream failures retain exit two through [`CliError`].
pub(crate) fn convert(path: &Path, options: &ConvertOptions<'_>) -> Result<ExitCode, CliError> {
    let source_format = source_format(path)?;
    if !supported_conversion_pair(source_format, options.target) {
        let error = conversion_error(
            marksheet_convert::ConvertErrorCode::UnsupportedPackage,
            "the requested source and destination format pair is not supported",
        );
        let report = error.unsupported_report(
            source_format.descriptor(),
            options.target.descriptor(),
            "conversion",
        );
        print_conversion_report(&report)?;
        return Ok(ExitCode::from(1));
    }
    let limits = marksheet_convert::ConversionLimits::default();
    let Some(source) = read_conversion_source(path, limits.max_input_bytes)? else {
        let error = conversion_error(
            marksheet_convert::ConvertErrorCode::ResourceLimit,
            format!(
                "input exceeds the configured {} byte conversion limit",
                limits.max_input_bytes
            ),
        );
        let report = error.unsupported_report(
            source_format.descriptor(),
            options.target.descriptor(),
            "resource_limit.input_bytes",
        );
        print_conversion_report(&report)?;
        return Ok(ExitCode::from(1));
    };
    let output = options.output.map_or_else(
        || path.with_extension(options.target.extension()),
        Path::to_owned,
    );
    if output == path {
        return Err(CliError::OutputEqualsInput(output));
    }
    reject_destination_symlink(&output)?;

    let conversion = perform_conversion(source_format, &source, options);
    let conversion = match conversion {
        Ok(conversion) => conversion,
        Err(failure) => {
            print_conversion_report(&failure.report)?;
            return Ok(ExitCode::from(1));
        }
    };

    if conversion.report.fidelity() == marksheet_convert::Fidelity::Unsupported {
        print_conversion_report(&conversion.report)?;
        return Ok(ExitCode::from(1));
    }
    replace_atomically(&output, &conversion.value)?;
    print_conversion_report(&conversion.report)?;
    Ok(ExitCode::SUCCESS)
}

const fn supported_conversion_pair(source: SourceFormat, target: ConversionTarget) -> bool {
    matches!(
        (source, target),
        (
            SourceFormat::Marksheet,
            ConversionTarget::Xlsx | ConversionTarget::Csv
        ) | (
            SourceFormat::Xlsx | SourceFormat::Csv,
            ConversionTarget::Marksheet
        )
    )
}

fn perform_conversion(
    source_format: SourceFormat,
    source: &[u8],
    options: &ConvertOptions<'_>,
) -> marksheet_convert::ConversionResult<Vec<u8>> {
    let limits = marksheet_convert::ConversionLimits::default();
    match (source_format, options.target) {
        (SourceFormat::Marksheet, ConversionTarget::Xlsx) => {
            reject_csv_options(options).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            let workbook = validated_workbook(source).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            marksheet_convert::export_xlsx(&workbook, limits)
        }
        (SourceFormat::Marksheet, ConversionTarget::Csv) => {
            let workbook = validated_workbook(source).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            let selection = csv_export_selection(options).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            marksheet_convert::export_csv(&workbook, &selection, limits)
        }
        (SourceFormat::Xlsx, ConversionTarget::Marksheet) => {
            reject_csv_options(options).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            let imported = marksheet_convert::import_xlsx(source, limits)?;
            let value = serialize_imported_workbook(&imported.value).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            Ok(marksheet_convert::Conversion {
                value,
                report: imported.report,
            })
        }
        (SourceFormat::Csv, ConversionTarget::Marksheet) => {
            let selection = csv_import_selection(options).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            let imported = marksheet_convert::import_csv(source, &selection, limits)?;
            let value = serialize_imported_workbook(&imported.value).map_err(|error| {
                cli_conversion_failure(source_format, options.target, error, options)
            })?;
            Ok(marksheet_convert::Conversion {
                value,
                report: imported.report,
            })
        }
        _ => Err(cli_conversion_failure(
            source_format,
            options.target,
            conversion_error(
                marksheet_convert::ConvertErrorCode::UnsupportedPackage,
                "the requested source and destination format pair is not supported",
            ),
            options,
        )),
    }
}

fn cli_conversion_failure(
    source: SourceFormat,
    target: ConversionTarget,
    error: marksheet_convert::ConvertError,
    options: &ConvertOptions<'_>,
) -> marksheet_convert::ConversionFailure {
    let feature = conversion_error_feature(source, target, error.code, options);
    marksheet_convert::ConversionFailure::new(
        error,
        source.descriptor(),
        target.descriptor(),
        feature,
    )
}

fn serialize_imported_workbook(
    workbook: &marksheet_model::Workbook,
) -> Result<Vec<u8>, marksheet_convert::ConvertError> {
    marksheet_syntax::serialize_workbook(workbook).map_err(|diagnostics| {
        let detail = diagnostics.first().map_or_else(
            || "semantic workbook cannot be serialized".to_owned(),
            |diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message),
        );
        conversion_error(
            marksheet_convert::ConvertErrorCode::InvalidWorkbook,
            format!("imported workbook cannot be serialized as Marksheet: {detail}"),
        )
    })
}

fn validated_workbook(
    source: &[u8],
) -> Result<marksheet_model::Workbook, marksheet_convert::ConvertError> {
    let mut document = parse_with_extensions(source);
    if document.parsed.has_errors() || !document.capabilities_complete {
        let codes = document
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .take(8)
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(conversion_error(
            marksheet_convert::ConvertErrorCode::InvalidWorkbook,
            format!("Marksheet input is incomplete or invalid ({codes})"),
        ));
    }
    let workbook = document.parsed.workbook.take().ok_or_else(|| {
        conversion_error(
            marksheet_convert::ConvertErrorCode::InvalidWorkbook,
            "Marksheet input did not produce a semantic workbook",
        )
    })?;
    let formula_diagnostics = validate_formulas(&workbook);
    if let Some(diagnostic) = formula_diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(conversion_error(
            marksheet_convert::ConvertErrorCode::InvalidWorkbook,
            format!(
                "Marksheet formula validation failed ({}: {})",
                diagnostic.code, diagnostic.message
            ),
        ));
    }
    Ok(workbook)
}

fn csv_export_selection(
    options: &ConvertOptions<'_>,
) -> Result<marksheet_convert::CsvExportSelection, marksheet_convert::ConvertError> {
    if options.label.is_some() || options.anchor.is_some() {
        return Err(invalid_csv_export_selection());
    }
    match (&options.table, &options.sheet, options.range) {
        (Some(table), None, None) => Ok(marksheet_convert::CsvExportSelection::Table {
            table: table.clone(),
        }),
        (None, Some(sheet), Some(range)) => Ok(marksheet_convert::CsvExportSelection::Range {
            sheet: sheet.clone(),
            range,
        }),
        _ => Err(invalid_csv_export_selection()),
    }
}

fn csv_import_selection(
    options: &ConvertOptions<'_>,
) -> Result<marksheet_convert::CsvImportSelection, marksheet_convert::ConvertError> {
    let (Some(sheet), Some(label)) = (&options.sheet, &options.label) else {
        return Err(missing_csv_import_target());
    };
    if label.is_empty() {
        return Err(missing_csv_import_target());
    }
    match (&options.table, options.anchor, options.range) {
        (Some(table), Some(anchor), None) => Ok(marksheet_convert::CsvImportSelection::Table {
            sheet: sheet.clone(),
            label: label.clone(),
            table: table.clone(),
            anchor,
        }),
        (None, None, Some(range)) => Ok(marksheet_convert::CsvImportSelection::Range {
            sheet: sheet.clone(),
            label: label.clone(),
            range,
        }),
        _ => Err(missing_csv_import_target()),
    }
}

fn reject_csv_options(options: &ConvertOptions<'_>) -> Result<(), marksheet_convert::ConvertError> {
    if options.sheet.is_some()
        || options.label.is_some()
        || options.range.is_some()
        || options.table.is_some()
        || options.anchor.is_some()
    {
        return Err(conversion_error(
            marksheet_convert::ConvertErrorCode::InvalidSelection,
            "CSV selection options are only valid when CSV is the source or destination",
        ));
    }
    Ok(())
}

fn invalid_csv_export_selection() -> marksheet_convert::ConvertError {
    conversion_error(
        marksheet_convert::ConvertErrorCode::InvalidSelection,
        "CSV export requires exactly one `--table`, or `--sheet` together with `--range`",
    )
}

fn missing_csv_import_target() -> marksheet_convert::ConvertError {
    marksheet_convert::ConvertError::invalid_selection(
        "CSV import requires `--sheet` and non-empty `--label`, plus either `--range` or `--table` with `--anchor`",
    )
}

fn conversion_error(
    code: marksheet_convert::ConvertErrorCode,
    message: impl Into<String>,
) -> marksheet_convert::ConvertError {
    marksheet_convert::ConvertError::new(code, message)
}

fn conversion_error_feature(
    source: SourceFormat,
    target: ConversionTarget,
    code: marksheet_convert::ConvertErrorCode,
    options: &ConvertOptions<'_>,
) -> &'static str {
    if source == SourceFormat::Csv
        && target == ConversionTarget::Marksheet
        && !has_complete_csv_import_target(options)
    {
        return "csv_import_target";
    }
    match (source, target, code) {
        (
            SourceFormat::Marksheet,
            ConversionTarget::Csv,
            marksheet_convert::ConvertErrorCode::InvalidSelection,
        ) => "csv_selection",
        (
            _,
            _,
            marksheet_convert::ConvertErrorCode::ResourceLimit
            | marksheet_convert::ConvertErrorCode::OutputLimit,
        ) => "resource_limit",
        (_, _, marksheet_convert::ConvertErrorCode::InvalidWorkbook) => "source_workbook",
        _ => "conversion",
    }
}

fn has_complete_csv_import_target(options: &ConvertOptions<'_>) -> bool {
    options.sheet.is_some()
        && options
            .label
            .as_ref()
            .is_some_and(|label| !label.is_empty())
        && matches!(
            (&options.table, options.anchor, options.range),
            (Some(_), Some(_), None) | (None, None, Some(_))
        )
}

fn print_conversion_report(report: &marksheet_convert::ConversionReport) -> Result<(), CliError> {
    let output = serde_json::to_string_pretty(report).map_err(CliError::Serialize)?;
    crate::render::print_stdout(&format!("{output}\n")).map_err(CliError::Render)
}

impl SourceFormat {
    fn descriptor(self) -> marksheet_convert::FormatDescriptor {
        match self {
            Self::Marksheet => marksheet_convert::FormatDescriptor::marksheet_ir(),
            Self::Xlsx => marksheet_convert::FormatDescriptor::xlsx(),
            Self::Csv => marksheet_convert::FormatDescriptor::csv(),
        }
    }
}

impl ConversionTarget {
    const fn extension(self) -> &'static str {
        match self {
            Self::Marksheet => "ms",
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
        }
    }

    fn descriptor(self) -> marksheet_convert::FormatDescriptor {
        match self {
            Self::Marksheet => marksheet_convert::FormatDescriptor::marksheet_ir(),
            Self::Xlsx => marksheet_convert::FormatDescriptor::xlsx(),
            Self::Csv => marksheet_convert::FormatDescriptor::csv(),
        }
    }
}

fn source_format(path: &Path) -> Result<SourceFormat, CliError> {
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("ms") => Ok(SourceFormat::Marksheet),
        Some("xlsx") => Ok(SourceFormat::Xlsx),
        Some("csv") => Ok(SourceFormat::Csv),
        _ => Err(CliError::UnknownInputFormat(path.to_owned())),
    }
}

fn read_conversion_source(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, CliError> {
    let file = fs::File::open(path).map_err(|source| CliError::Read {
        path: path.to_owned(),
        source,
    })?;
    let read_limit = max_bytes.saturating_add(1);
    let initial_capacity = usize::try_from(max_bytes.min(64 * 1024)).unwrap_or(64 * 1024);
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| CliError::Read {
            path: path.to_owned(),
            source,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

/// Runs `marksheet check`.
pub(crate) fn check(path: &Path, format: OutputFormat) -> Result<ExitCode, CliError> {
    let source = read_source(path)?;
    let document = parse_with_extensions(&source);
    let mut diagnostics = document.diagnostics.clone();
    if !document.parsed.has_errors() && document.capabilities_complete {
        if let Some(workbook) = document.parsed.workbook.as_ref() {
            diagnostics.extend(validate_formulas(workbook));
            sort_and_deduplicate_diagnostics(&mut diagnostics);
        }
    }

    crate::render::render(path, document.parsed.source_bytes(), &diagnostics, format)
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

/// Parses with the exact capabilities installed in this executable, then lets
/// the same registry validate every opaque instance. Keeping this in one
/// function prevents commands from accidentally accepting a workbook under a
/// different extension environment.
fn parse_with_extensions(source: &[u8]) -> CliParsedDocument {
    let registry = marksheet_extensions::ExtensionRegistry::with_assertions();
    let supported_extensions = registry
        .capabilities()
        .into_iter()
        .map(|capability| format!("{}@{}", capability.id, capability.major))
        .collect();
    let options = marksheet_syntax::ParseOptions {
        supported_extensions,
    };
    let parsed = marksheet_syntax::parse_with_options(source, &options);
    let mut diagnostics = parsed.diagnostics.clone();
    let mut capabilities_complete = true;
    if let Some(workbook) = parsed.workbook.as_ref() {
        let report = registry.validate(workbook, &marksheet_extensions::ExtensionLimits::default());
        capabilities_complete = report.capabilities_complete;
        diagnostics.extend(
            report
                .diagnostics
                .into_iter()
                // Availability is already source-located by lowering using
                // this registry's exact capability set. Prefer that narrower
                // declaration-token span over the registry's directive span.
                .filter(|diagnostic| {
                    !matches!(
                        diagnostic.diagnostic.code.as_str(),
                        marksheet_extensions::AVAILABILITY_REQUIRED_DIAGNOSTIC
                            | marksheet_extensions::AVAILABILITY_WARNING_DIAGNOSTIC
                    )
                })
                .map(|diagnostic| diagnostic.diagnostic),
        );
    }
    sort_and_deduplicate_diagnostics(&mut diagnostics);
    CliParsedDocument {
        parsed,
        diagnostics,
        capabilities_complete,
    }
}

fn sort_and_deduplicate_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.sort_by(|left, right| {
        diagnostic_sort_key(left)
            .cmp(&diagnostic_sort_key(right))
            .then_with(|| left.primary.label.cmp(&right.primary.label))
            .then_with(|| left.message.cmp(&right.message))
    });
    diagnostics.dedup_by(|right, left| diagnostic_identity(left) == diagnostic_identity(right));
}

fn diagnostic_sort_key(diagnostic: &Diagnostic) -> (u64, u64, &str, u8) {
    (
        diagnostic.primary.span.start,
        diagnostic.primary.span.end,
        diagnostic.code.as_str(),
        severity_rank(diagnostic.severity),
    )
}

fn diagnostic_identity(diagnostic: &Diagnostic) -> (u64, u64, &str, Severity, &str) {
    (
        diagnostic.primary.span.start,
        diagnostic.primary.span.end,
        diagnostic.code.as_str(),
        diagnostic.severity,
        diagnostic.message.as_str(),
    )
}

const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    }
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
    let document = parse_with_extensions(&source);
    if document.parsed.has_errors() || !document.capabilities_complete {
        crate::render::render_human(path, document.parsed.source_bytes(), &document.diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }
    let workbook = document
        .parsed
        .workbook
        .as_ref()
        .ok_or(CliError::MissingWorkbook)?;

    let engine = marksheet_calc::engine::ReferenceCalcEngine::new();
    let report = engine.prepare(workbook, marksheet_calc::engine::CalcLimits::default());
    let Some(mut calculation) = report.calculation else {
        let mut diagnostics = document.diagnostics;
        diagnostics.extend(report.diagnostics);
        sort_and_deduplicate_diagnostics(&mut diagnostics);
        crate::render::render_human(path, document.parsed.source_bytes(), &diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    };

    let request = marksheet_calc::engine::CalculationRequest::new(sheet, range);
    let result = engine.calculate(&mut calculation, &request);
    let mut diagnostics = document.diagnostics;
    diagnostics.extend(result.diagnostics.iter().cloned());
    sort_and_deduplicate_diagnostics(&mut diagnostics);
    let expected_cells = range_cell_count(range)?;
    if result.cells.len() as u64 != expected_cells {
        crate::render::render_human(path, document.parsed.source_bytes(), &diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }

    let output = crate::render::format_calculation(&request, &result, format)
        .map_err(CliError::Serialize)?;
    crate::render::print_stdout(&output).map_err(CliError::Render)?;
    if !diagnostics.is_empty() {
        crate::render::render_human(path, document.parsed.source_bytes(), &diagnostics)
            .map_err(CliError::Render)?;
    }

    Ok(exit_for_diagnostics(&diagnostics))
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
    let old = parse_with_extensions(&old_source);
    let new = parse_with_extensions(&new_source);

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
        .parsed
        .workbook
        .as_ref()
        .expect("complete diff input has a workbook projection");
    let new_workbook = new
        .parsed
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
                source: old.parsed.source_bytes(),
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
    document: &'a CliParsedDocument,
    output: &mut Vec<crate::render::DiffDiagnostic<'a>>,
) {
    let incomplete = document.parsed.has_errors() || !document.capabilities_complete;
    if incomplete {
        output.extend(
            document
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .map(|diagnostic| crate::render::DiffDiagnostic {
                    path,
                    source: document.parsed.source_bytes(),
                    diagnostic: diagnostic.clone(),
                }),
        );
    }
    if !incomplete {
        if let Some(workbook) = document.parsed.workbook.as_ref() {
            output.extend(
                validate_formulas(workbook)
                    .into_iter()
                    .filter(|diagnostic| diagnostic.severity == Severity::Error)
                    .map(|diagnostic| crate::render::DiffDiagnostic {
                        path,
                        source: document.parsed.source_bytes(),
                        diagnostic,
                    }),
            );
        } else {
            output.push(crate::render::DiffDiagnostic {
                path,
                source: document.parsed.source_bytes(),
                diagnostic: incomplete_projection_diagnostic(),
            });
        }
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
    let document = parse_with_extensions(&source);
    if document.parsed.has_errors() || !document.capabilities_complete {
        crate::render::render_human(path, document.parsed.source_bytes(), &document.diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }
    if let Some(workbook) = document.parsed.workbook.as_ref() {
        let formula_diagnostics = validate_formulas(workbook);
        if formula_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            crate::render::render_human(path, document.parsed.source_bytes(), &formula_diagnostics)
                .map_err(CliError::Render)?;
            return Ok(ExitCode::from(1));
        }
    }

    let formatted = match marksheet_syntax::canonicalize(&document.parsed) {
        Ok(formatted) => formatted,
        Err(diagnostics) => {
            // This should normally be unreachable after `has_errors`, but the
            // formatter is allowed to reject a document if a future canonical
            // invariant cannot be represented safely.
            crate::render::render_human(path, document.parsed.source_bytes(), &diagnostics)
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

fn reject_destination_symlink(path: &Path) -> Result<(), CliError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(CliError::OutputSymbolicLink(path.to_owned()))
        }
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CliError::Read {
            path: path.to_owned(),
            source,
        }),
    }
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
    OutputSymbolicLink(PathBuf),
    OutputEqualsInput(PathBuf),
    UnknownInputFormat(PathBuf),
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
            Self::OutputSymbolicLink(path) => write!(
                formatter,
                "refusing to replace symbolic-link destination {}",
                path.display()
            ),
            Self::OutputEqualsInput(path) => write!(
                formatter,
                "refusing to overwrite conversion input {}",
                path.display()
            ),
            Self::UnknownInputFormat(path) => write!(
                formatter,
                "cannot infer input format from {}; expected .ms, .xlsx, or .csv",
                path.display()
            ),
            Self::MissingWorkbook => {
                formatter.write_str("parsed document did not contain a workbook")
            }
            Self::InvalidRange(range) => write!(formatter, "invalid calculation range {range}"),
            Self::Serialize(source) => write!(formatter, "could not serialize output: {source}"),
        }
    }
}

impl std::error::Error for CliError {}
