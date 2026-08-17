//! Stable automation-facing workbook inspection and focused edit commands.

use std::{collections::BTreeMap, fs, path::Path, process::ExitCode};

use marksheet_calc::{
    CalcEngine, CalcLimits, CalculationRequest, ReferenceCalcEngine,
    eval::CalcValue,
    prepare::{PrepareLimits, PreparedWorkbook, TableIndex},
};
use marksheet_edit::transaction::{
    EditErrorKind, EditOperation, EditTransaction, SourceFingerprint,
};
use marksheet_model::{
    Coordinate, Diagnostic, ExtensionId, LineIndex, NameId, NameTarget, Range, Severity, SheetId,
    TableId, Value, Workbook,
};
use serde::Serialize;

use crate::{commands, render};

const MAX_GET_CELLS: u64 = 1_000_000;
const MAX_AUTOMATION_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_FORMAT_REPLACEMENT_JSON_BYTES: usize = 31 * 1024 * 1024;

/// Runs `marksheet inspect` and always emits one versioned JSON document.
pub(crate) fn inspect(path: &Path) -> Result<ExitCode, commands::CliError> {
    let Some(source) = commands::read_source_bounded(path, MAX_AUTOMATION_SOURCE_BYTES)? else {
        return print_inspect_resource_failure(path);
    };
    let document = commands::parse_with_extensions(&source);
    let mut diagnostics = document.diagnostics.clone();
    let mut prepared = None;

    if !document.parsed.has_errors() && document.capabilities_complete {
        if let Some(workbook) = document.parsed.workbook.as_ref() {
            let report = ReferenceCalcEngine::new().prepare(workbook, CalcLimits::default());
            diagnostics.extend(report.diagnostics);
            prepared = PreparedWorkbook::build(workbook, PrepareLimits::default()).ok();
            commands::sort_and_deduplicate_diagnostics(&mut diagnostics);
        }
    }

    let status = status(&diagnostics, document.capabilities_complete);
    let output = InspectOutput {
        version: "marksheet-inspect@1",
        profile: "portable-a1@1",
        status,
        source: SourceVersion::new(&source),
        workbook: document
            .parsed
            .workbook
            .as_ref()
            .map(|workbook| inspect_workbook(workbook, prepared.as_ref())),
        diagnostics: json_diagnostics(&source, &diagnostics),
        error: None,
    };
    print_json(&output)?;
    Ok(exit_for_status(status))
}

/// Runs `marksheet get` for an explicit cell, range, name, or table.
pub(crate) fn get(
    path: &Path,
    target: &str,
    calculated: bool,
) -> Result<ExitCode, commands::CliError> {
    let Some(source) = commands::read_source_bounded(path, MAX_AUTOMATION_SOURCE_BYTES)? else {
        return print_get_resource_failure(path, target, calculated);
    };
    let document = commands::parse_with_extensions(&source);
    let mut diagnostics = document.diagnostics.clone();
    if document.parsed.has_errors() || !document.capabilities_complete {
        return print_get_failure(
            &source,
            target,
            calculated,
            &diagnostics,
            "invalid_base",
            "workbook is incomplete",
        );
    }
    let workbook = document
        .parsed
        .workbook
        .as_ref()
        .ok_or(commands::CliError::MissingWorkbook)?;
    let prepared = match PreparedWorkbook::build(workbook, PrepareLimits::default()) {
        Ok(prepared) => prepared,
        Err(error) => {
            return print_get_failure(
                &source,
                target,
                calculated,
                &diagnostics,
                "invalid_base",
                &error.to_string(),
            );
        }
    };
    let engine = ReferenceCalcEngine::new();
    let report = engine.prepare(workbook, CalcLimits::default());
    diagnostics.extend(report.diagnostics);
    commands::sort_and_deduplicate_diagnostics(&mut diagnostics);
    let Some(mut calculation) = report.calculation else {
        return print_get_failure(
            &source,
            target,
            calculated,
            &diagnostics,
            "invalid_base",
            "workbook cannot be prepared for explicit queries",
        );
    };
    let resolved = match resolve_target(&prepared, target) {
        Ok(resolved) => resolved,
        Err(message) => {
            return print_get_failure(
                &source,
                target,
                calculated,
                &diagnostics,
                "invalid_target",
                &message,
            );
        }
    };
    if cell_count(resolved.range)? > MAX_GET_CELLS {
        return print_get_failure(
            &source,
            target,
            calculated,
            &diagnostics,
            "resource_limit",
            "target exceeds the 1000000-cell query limit",
        );
    }

    let calculated_cells = if calculated {
        let request = CalculationRequest::new(resolved.sheet.clone(), resolved.range);
        let result = engine.calculate(&mut calculation, &request);
        diagnostics.extend(result.diagnostics);
        commands::sort_and_deduplicate_diagnostics(&mut diagnostics);
        result
            .cells
            .into_iter()
            .map(|cell| (cell.cell.coordinate, cell.value))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };

    let cells = get_cells(&prepared, &resolved, &calculated_cells)?;

    let status = status(&diagnostics, true);
    let output = GetOutput {
        version: "marksheet-get@1",
        profile: "portable-a1@1",
        status,
        requested_target: target,
        target: Some(JsonResolvedTarget::from(&resolved)),
        calculated,
        cells,
        diagnostics: json_diagnostics(&source, &diagnostics),
        error: None,
    };
    print_json(&output)?;
    Ok(exit_for_status(status))
}

/// Runs canonical formatting with exact, self-reported mutation provenance.
pub(crate) fn format(path: &Path, check: bool) -> Result<ExitCode, commands::CliError> {
    commands::reject_symlink(path)?;
    let Some(source) = commands::read_source_bounded(path, MAX_AUTOMATION_SOURCE_BYTES)? else {
        return print_format_resource_failure(path, check);
    };
    let document = commands::parse_with_extensions(&source);
    let mut diagnostics = document.diagnostics.clone();
    if document.parsed.has_errors() || !document.capabilities_complete {
        return print_format_failure(
            &source,
            check,
            &diagnostics,
            "invalid_base",
            "workbook is incomplete",
        );
    }
    if let Some(workbook) = document.parsed.workbook.as_ref() {
        let formula_diagnostics = commands::validate_formulas(workbook);
        let formula_errors = formula_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        diagnostics.extend(formula_diagnostics);
        commands::sort_and_deduplicate_diagnostics(&mut diagnostics);
        if formula_errors {
            return print_format_failure(
                &source,
                check,
                &diagnostics,
                "invalid_formula",
                "workbook formulas are invalid",
            );
        }
    }
    let formatted = match marksheet_syntax::canonicalize(&document.parsed) {
        Ok(formatted) => formatted,
        Err(mut errors) => {
            diagnostics.append(&mut errors);
            commands::sort_and_deduplicate_diagnostics(&mut diagnostics);
            return print_format_failure(
                &source,
                check,
                &diagnostics,
                "invalid_base",
                "workbook cannot be represented canonically",
            );
        }
    };
    let would_change = source != formatted;
    if would_change && json_string_encoded_len(&formatted) > MAX_FORMAT_REPLACEMENT_JSON_BYTES {
        return print_format_failure(
            &source,
            check,
            &diagnostics,
            "resource_limit",
            "formatted source cannot fit in the bounded exact-patch response",
        );
    }
    let Ok(formatted_diagnostics) = validate_format_candidate(&source, &formatted, &diagnostics)
    else {
        return print_format_regression(&source, &formatted, check, &diagnostics);
    };
    if check {
        let proposed_valid = !formatted_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        return print_format_check(&source, &formatted, &diagnostics, proposed_valid);
    }
    apply_format(
        path,
        &source,
        &formatted,
        &diagnostics,
        &formatted_diagnostics,
    )
}

fn print_format_check(
    source: &[u8],
    formatted: &[u8],
    diagnostics: &[Diagnostic],
    proposed_valid: bool,
) -> Result<ExitCode, commands::CliError> {
    let would_change = source != formatted;
    let output = FormatOutput {
        version: "marksheet-format@1",
        profile: "portable-a1@1",
        status: if would_change { "needs_format" } else { "ok" },
        check_only: true,
        changed: false,
        would_change,
        valid: proposed_valid,
        before: SourceVersion::new(source),
        after: SourceVersion::new(source),
        proposed: would_change.then(|| SourceVersion::new(formatted)),
        patches: Vec::new(),
        // Check mode does not produce the candidate bytes, and the @1
        // envelope has no candidate-source binding. Report the verified
        // candidate verdict while keeping positions scoped to the input.
        diagnostics: json_diagnostics(source, diagnostics),
        error: None,
    };
    print_json(&output)?;
    Ok(if would_change {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn apply_format(
    path: &Path,
    source: &[u8],
    formatted: &[u8],
    source_diagnostics: &[Diagnostic],
    formatted_diagnostics: &[Diagnostic],
) -> Result<ExitCode, commands::CliError> {
    let would_change = source != formatted;
    if would_change && !commands::replace_atomically_if_current(path, formatted, source)? {
        let after = match commands::read_source_bounded(path, MAX_AUTOMATION_SOURCE_BYTES)? {
            Some(current) => SourceVersion::new(&current),
            None => SourceVersion::unhashed(source_length(path)?),
        };
        let output = FormatOutput {
            version: "marksheet-format@1",
            profile: "portable-a1@1",
            status: "conflict",
            check_only: false,
            changed: false,
            would_change: true,
            valid: false,
            before: SourceVersion::new(source),
            after,
            proposed: Some(SourceVersion::new(formatted)),
            patches: Vec::new(),
            diagnostics: json_diagnostics(source, source_diagnostics),
            error: Some(AutomationError {
                kind: "conflict",
                message: "source bytes changed after formatting was planned",
            }),
        };
        print_json(&output)?;
        return Ok(ExitCode::from(1));
    }

    let output = FormatOutput {
        version: "marksheet-format@1",
        profile: "portable-a1@1",
        status: "ok",
        check_only: false,
        changed: would_change,
        would_change,
        valid: !formatted_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        before: SourceVersion::new(source),
        after: SourceVersion::new(formatted),
        proposed: None,
        patches: if would_change {
            vec![JsonPatch {
                start: 0,
                end: u64::try_from(source.len()).expect("automation source limit fits u64"),
                replacement: String::from_utf8_lossy(formatted).into_owned(),
            }]
        } else {
            Vec::new()
        },
        // `formatted` is the source these diagnostics were derived from, and
        // when nothing changed it is byte-identical to `source`, so spans and
        // line index always describe the same bytes.
        diagnostics: json_diagnostics(formatted, formatted_diagnostics),
        error: None,
    };
    print_json(&output)?;
    Ok(ExitCode::SUCCESS)
}

/// Candidate source reparsed through the pipeline that admitted the base
/// workbook, so a mutating command can report the state it actually produced.
struct Revalidated {
    /// Whether the source would pass the same admission gate the base source
    /// passed: parseable, capability-complete, and free of formula errors.
    admissible: bool,
    diagnostics: Vec<Diagnostic>,
}

/// Rechecks candidate source against the admission gate exactly.
///
/// Trusted extension diagnostics are deliberately excluded, because admission
/// excludes them too. A failed assertion is an authoring outcome that
/// formatting neither causes nor repairs, so treating it as a formatter defect
/// would refuse to format a workbook that was already failing that assertion
/// before and after the rewrite.
fn revalidate(source: &[u8]) -> Revalidated {
    let document = commands::parse_with_extensions(source);
    let mut diagnostics = document.diagnostics.clone();
    let mut formula_errors = false;
    if let Some(workbook) = document.parsed.workbook.as_ref() {
        let formula_diagnostics = commands::validate_formulas(workbook);
        formula_errors = formula_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        diagnostics.extend(formula_diagnostics);
    }
    commands::sort_and_deduplicate_diagnostics(&mut diagnostics);
    Revalidated {
        admissible: !document.parsed.has_errors()
            && document.capabilities_complete
            && !formula_errors,
        diagnostics,
    }
}

/// Applies the result-admission gate before either check-only reporting or a
/// write is allowed to describe the canonical candidate.
fn validate_format_candidate(
    source: &[u8],
    formatted: &[u8],
    diagnostics: &[Diagnostic],
) -> Result<Vec<Diagnostic>, Vec<Diagnostic>> {
    if source == formatted {
        return Ok(diagnostics.to_vec());
    }
    let revalidated = revalidate(formatted);
    if revalidated.admissible {
        Ok(revalidated.diagnostics)
    } else {
        Err(revalidated.diagnostics)
    }
}

fn print_format_regression(
    source: &[u8],
    formatted: &[u8],
    check: bool,
    diagnostics: &[Diagnostic],
) -> Result<ExitCode, commands::CliError> {
    let before = SourceVersion::new(source);
    let output = FormatOutput {
        version: "marksheet-format@1",
        profile: "portable-a1@1",
        status: "invalid",
        check_only: check,
        changed: false,
        would_change: true,
        valid: false,
        after: before.clone(),
        before,
        proposed: Some(SourceVersion::new(formatted)),
        patches: Vec::new(),
        // The @1 envelope fingerprints but cannot reconstruct or explicitly
        // bind the uncommitted proposal. Never expose candidate-relative
        // positions that clients would naturally map onto the unchanged file.
        diagnostics: json_diagnostics(source, diagnostics),
        error: Some(AutomationError {
            kind: "invalid_result",
            message: "canonical formatting would produce an invalid workbook; no write performed",
        }),
    };
    print_json(&output)?;
    Ok(ExitCode::from(1))
}

fn get_cells(
    prepared: &PreparedWorkbook,
    resolved: &ResolvedTarget,
    calculated: &BTreeMap<Coordinate, CalcValue>,
) -> Result<Vec<GetCell>, commands::CliError> {
    let sheet = prepared
        .sheet(&resolved.sheet)
        .ok_or(commands::CliError::MissingWorkbook)?;
    coordinates(resolved.range).map(|coordinates| {
        coordinates
            .map(|coordinate| {
                let virtual_formula = sheet
                    .virtual_cell(coordinate)
                    .map(|cell| cell.formula.to_string());
                if let Some(authored) = sheet.authored_cell(coordinate) {
                    GetCell {
                        coordinate: coordinate.to_string(),
                        source: CellSource::Authored,
                        authored: Some(authored.cell.value.clone()),
                        virtual_formula,
                        calculated: calculated.get(&coordinate).cloned(),
                    }
                } else {
                    GetCell {
                        coordinate: coordinate.to_string(),
                        source: if virtual_formula.is_some() {
                            CellSource::Virtual
                        } else {
                            CellSource::Absent
                        },
                        authored: None,
                        virtual_formula,
                        calculated: calculated.get(&coordinate).cloned(),
                    }
                }
            })
            .collect()
    })
}

/// Runs a source-aware single-cell edit and atomically replaces the workbook.
pub(crate) fn set(
    path: &Path,
    target: &str,
    value_or_formula: &str,
) -> Result<ExitCode, commands::CliError> {
    commands::reject_symlink(path)?;
    let Some(source) = commands::read_source_bounded(path, MAX_AUTOMATION_SOURCE_BYTES)? else {
        return print_edit_resource_failure(path, "set_cell", target);
    };
    let document = commands::parse_with_extensions(&source);
    if document.parsed.has_errors() || !document.capabilities_complete {
        return print_edit_failure(
            &source,
            "set_cell",
            target,
            &document.diagnostics,
            "invalid_base",
            "workbook is incomplete",
        );
    }
    let workbook = document
        .parsed
        .workbook
        .as_ref()
        .ok_or(commands::CliError::MissingWorkbook)?;
    let prepared = match PreparedWorkbook::build(workbook, PrepareLimits::default()) {
        Ok(prepared) => prepared,
        Err(error) => {
            return print_edit_failure(
                &source,
                "set_cell",
                target,
                &document.diagnostics,
                "invalid_base",
                &error.to_string(),
            );
        }
    };
    let resolved = match resolve_target(&prepared, target) {
        Ok(resolved) if resolved.settable && resolved.range.start == resolved.range.end => resolved,
        Ok(_) => {
            return print_edit_failure(
                &source,
                "set_cell",
                target,
                &document.diagnostics,
                "ambiguous_target",
                "set requires a target that resolves to exactly one cell",
            );
        }
        Err(message) => {
            return print_edit_failure(
                &source,
                "set_cell",
                target,
                &document.diagnostics,
                "target_not_found",
                &message,
            );
        }
    };
    let value = match Value::parse_strict(value_or_formula) {
        Ok(value) => value,
        Err(error) => {
            return print_edit_failure(
                &source,
                "set_cell",
                target,
                &document.diagnostics,
                "invalid_value",
                &error.to_string(),
            );
        }
    };
    execute_edit(
        path,
        &source,
        target,
        "set_cell",
        document.diagnostics,
        EditOperation::SetCell {
            sheet: resolved.sheet,
            coordinate: resolved.range.start,
            value,
        },
    )
}

/// Runs a source-aware table-row append and atomically replaces the workbook.
pub(crate) fn append_table_row(
    path: &Path,
    table: &str,
    values: &[String],
) -> Result<ExitCode, commands::CliError> {
    commands::reject_symlink(path)?;
    let Some(source) = commands::read_source_bounded(path, MAX_AUTOMATION_SOURCE_BYTES)? else {
        return print_edit_resource_failure(path, "append_table_row", table);
    };
    let document = commands::parse_with_extensions(&source);
    if document.parsed.has_errors() || !document.capabilities_complete {
        return print_edit_failure(
            &source,
            "append_table_row",
            table,
            &document.diagnostics,
            "invalid_base",
            "workbook is incomplete",
        );
    }
    let table_id = match TableId::parse(table) {
        Ok(table) => table,
        Err(error) => {
            return print_edit_failure(
                &source,
                "append_table_row",
                table,
                &document.diagnostics,
                "invalid_identifier",
                &error.to_string(),
            );
        }
    };
    let fields = match values
        .iter()
        .map(|value| Value::parse_strict(value))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(fields) => fields,
        Err(error) => {
            return print_edit_failure(
                &source,
                "append_table_row",
                table,
                &document.diagnostics,
                "invalid_value",
                &error.to_string(),
            );
        }
    };
    execute_edit(
        path,
        &source,
        table,
        "append_table_row",
        document.diagnostics,
        EditOperation::AppendTableRow {
            table: table_id,
            fields,
        },
    )
}

fn execute_edit(
    path: &Path,
    source: &[u8],
    target: &str,
    operation: &'static str,
    diagnostics: Vec<Diagnostic>,
    edit: EditOperation,
) -> Result<ExitCode, commands::CliError> {
    let transaction = EditTransaction::single(edit).expecting_source(source);
    match transaction.execute_with_parse_options(source, &commands::cli_parse_options()) {
        Ok(result) => {
            if result.source.len() > MAX_AUTOMATION_SOURCE_BYTES {
                return print_edit_failure(
                    source,
                    operation,
                    target,
                    &diagnostics,
                    "resource_limit",
                    "edited source would exceed the 32 MiB automation limit",
                );
            }
            let final_document = commands::parse_with_extensions(&result.source);
            let mut final_diagnostics = result.diagnostics.clone();
            final_diagnostics.extend(final_document.diagnostics);
            commands::sort_and_deduplicate_diagnostics(&mut final_diagnostics);
            let valid = !final_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error);
            let status = if valid {
                "ok"
            } else if result.changed() {
                "committed_invalid"
            } else {
                "invalid"
            };
            if result.changed()
                && !commands::replace_atomically_if_current(path, &result.source, source)?
            {
                let Some(current) =
                    commands::read_source_bounded(path, MAX_AUTOMATION_SOURCE_BYTES)?
                else {
                    return print_edit_resource_failure(path, operation, target);
                };
                return print_edit_failure(
                    &current,
                    operation,
                    target,
                    &diagnostics,
                    "conflict",
                    "source bytes changed after the edit was planned",
                );
            }
            let output = EditOutput {
                version: "marksheet-edit@1",
                profile: "portable-a1@1",
                status,
                operation,
                target,
                changed: result.changed(),
                valid,
                before: SourceVersion::from_fingerprint(result.before),
                after: SourceVersion::from_fingerprint(result.after),
                patches: result
                    .patches
                    .patches()
                    .iter()
                    .map(|patch| JsonPatch {
                        start: patch.span.start,
                        end: patch.span.end,
                        replacement: String::from_utf8_lossy(&patch.replacement).into_owned(),
                    })
                    .collect(),
                diagnostics: json_diagnostics(&result.source, &final_diagnostics),
                error: None,
            };
            print_json(&output)?;
            Ok(if valid {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Err(error) => {
            let mut diagnostics = diagnostics;
            diagnostics.extend(error.diagnostics);
            commands::sort_and_deduplicate_diagnostics(&mut diagnostics);
            print_edit_failure(
                source,
                operation,
                target,
                &diagnostics,
                edit_error_kind(error.kind),
                &error.message,
            )
        }
    }
}

fn resolve_target(prepared: &PreparedWorkbook, target: &str) -> Result<ResolvedTarget, String> {
    if let Some((sheet_text, range_text)) = target.split_once('!') {
        if sheet_text.is_empty() || range_text.is_empty() || range_text.contains('!') {
            return Err(format!("invalid explicit target {target:?}"));
        }
        let sheet = SheetId::parse(sheet_text)
            .map_err(|_| "explicit targets must use a stable sheet ID".to_owned())?;
        if prepared.sheet(&sheet).is_none() {
            return Err(format!("sheet {sheet} does not exist"));
        }
        let range = Range::parse(range_text).map_err(|error| error.to_string())?;
        return Ok(ResolvedTarget {
            kind: TargetKind::Range,
            id: None,
            sheet,
            range,
            settable: range.start == range.end,
        });
    }

    if let Ok(name) = NameId::parse(target) {
        if let Some(index) = prepared.names.get(&name) {
            return resolve_name(prepared, &name, &index.target);
        }
    }
    if let Ok(table) = TableId::parse(target) {
        if let Some(index) = prepared.table(&table) {
            return Ok(resolve_table(&table, index));
        }
    }
    Err(format!(
        "target {target:?} is not a workbook name, table, or sheet-qualified A1 range"
    ))
}

fn resolve_name(
    prepared: &PreparedWorkbook,
    name: &NameId,
    target: &NameTarget,
) -> Result<ResolvedTarget, String> {
    let (sheet, range, settable) = match target {
        NameTarget::Cell(cell) => (cell.sheet.clone(), Range::single(cell.coordinate), true),
        NameTarget::Range(range) => (range.sheet.clone(), range.range, false),
        NameTarget::TableColumn { table, header } => {
            let index = prepared
                .table(table)
                .ok_or_else(|| format!("name {name} refers to missing table {table}"))?;
            let range = index.data_column(header).ok_or_else(|| {
                format!("name {name} refers to a header-only or missing table column")
            })?;
            (index.sheet.clone(), range, false)
        }
    };
    Ok(ResolvedTarget {
        kind: TargetKind::Name,
        id: Some(name.to_string()),
        sheet,
        range,
        settable,
    })
}

fn resolve_table(table: &TableId, index: &TableIndex) -> ResolvedTarget {
    ResolvedTarget {
        kind: TargetKind::Table,
        id: Some(table.to_string()),
        sheet: index.sheet.clone(),
        range: index.footprint,
        settable: false,
    }
}

fn cell_count(range: Range) -> Result<u64, commands::CliError> {
    range
        .width()
        .ok()
        .and_then(|width| range.height().ok()?.checked_mul(width))
        .ok_or(commands::CliError::InvalidRange(range))
}

fn coordinates(range: Range) -> Result<impl Iterator<Item = Coordinate>, commands::CliError> {
    let _ = cell_count(range)?;
    Ok((range.start.row..=range.end.row).flat_map(move |row| {
        (range.start.column..=range.end.column).map(move |column| Coordinate { column, row })
    }))
}

fn status(diagnostics: &[Diagnostic], capabilities_complete: bool) -> &'static str {
    if !capabilities_complete
        || diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        "invalid"
    } else {
        "ok"
    }
}

fn exit_for_status(status: &str) -> ExitCode {
    if status == "ok" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn json_diagnostics(source: &[u8], diagnostics: &[Diagnostic]) -> Vec<render::JsonDiagnostic> {
    let line_index = std::str::from_utf8(source).ok().map(LineIndex::new);
    diagnostics
        .iter()
        .map(|diagnostic| render::JsonDiagnostic::from_diagnostic(diagnostic, line_index.as_ref()))
        .collect()
}

fn print_json(value: &impl Serialize) -> Result<(), commands::CliError> {
    let output = serde_json::to_string_pretty(value).map_err(commands::CliError::Serialize)?;
    render::print_stdout(&format!("{output}\n")).map_err(commands::CliError::Render)
}

fn print_get_failure(
    source: &[u8],
    target: &str,
    calculated: bool,
    diagnostics: &[Diagnostic],
    kind: &'static str,
    message: &str,
) -> Result<ExitCode, commands::CliError> {
    let output = GetOutput {
        version: "marksheet-get@1",
        profile: "portable-a1@1",
        status: "invalid",
        requested_target: target,
        target: None,
        calculated,
        cells: Vec::new(),
        diagnostics: json_diagnostics(source, diagnostics),
        error: Some(AutomationError { kind, message }),
    };
    print_json(&output)?;
    Ok(ExitCode::from(1))
}

fn print_inspect_resource_failure(path: &Path) -> Result<ExitCode, commands::CliError> {
    let output = InspectOutput {
        version: "marksheet-inspect@1",
        profile: "portable-a1@1",
        status: "invalid",
        source: SourceVersion::unhashed(source_length(path)?),
        workbook: None,
        diagnostics: Vec::new(),
        error: Some(AutomationError {
            kind: "resource_limit",
            message: "source exceeds the 32 MiB automation limit",
        }),
    };
    print_json(&output)?;
    Ok(ExitCode::from(1))
}

fn print_format_failure(
    source: &[u8],
    check: bool,
    diagnostics: &[Diagnostic],
    kind: &'static str,
    message: &str,
) -> Result<ExitCode, commands::CliError> {
    let version = SourceVersion::new(source);
    let output = FormatOutput {
        version: "marksheet-format@1",
        profile: "portable-a1@1",
        status: "invalid",
        check_only: check,
        changed: false,
        would_change: false,
        valid: false,
        before: version.clone(),
        after: version,
        proposed: None,
        patches: Vec::new(),
        diagnostics: json_diagnostics(source, diagnostics),
        error: Some(AutomationError { kind, message }),
    };
    print_json(&output)?;
    Ok(ExitCode::from(1))
}

fn print_format_resource_failure(path: &Path, check: bool) -> Result<ExitCode, commands::CliError> {
    let version = SourceVersion::unhashed(source_length(path)?);
    let output = FormatOutput {
        version: "marksheet-format@1",
        profile: "portable-a1@1",
        status: "invalid",
        check_only: check,
        changed: false,
        would_change: false,
        valid: false,
        before: version.clone(),
        after: version,
        proposed: None,
        patches: Vec::new(),
        diagnostics: Vec::new(),
        error: Some(AutomationError {
            kind: "resource_limit",
            message: "source exceeds the 32 MiB automation limit",
        }),
    };
    print_json(&output)?;
    Ok(ExitCode::from(1))
}

fn json_string_encoded_len(source: &[u8]) -> usize {
    source.iter().fold(2usize, |length, byte| {
        length.saturating_add(match byte {
            b'"' | b'\\' | b'\x08' | b'\x09' | b'\x0a' | b'\x0c' | b'\x0d' => 2,
            0..=0x1f => 6,
            _ => 1,
        })
    })
}

fn print_get_resource_failure(
    path: &Path,
    target: &str,
    calculated: bool,
) -> Result<ExitCode, commands::CliError> {
    let _ = source_length(path)?;
    print_get_failure(
        &[],
        target,
        calculated,
        &[],
        "resource_limit",
        "source exceeds the 32 MiB automation limit",
    )
}

fn print_edit_resource_failure(
    path: &Path,
    operation: &'static str,
    target: &str,
) -> Result<ExitCode, commands::CliError> {
    let source = SourceVersion::unhashed(source_length(path)?);
    let output = EditOutput {
        version: "marksheet-edit@1",
        profile: "portable-a1@1",
        status: "invalid",
        operation,
        target,
        changed: false,
        valid: false,
        before: source.clone(),
        after: source,
        patches: Vec::new(),
        diagnostics: Vec::new(),
        error: Some(AutomationError {
            kind: "resource_limit",
            message: "source exceeds the 32 MiB automation limit",
        }),
    };
    print_json(&output)?;
    Ok(ExitCode::from(1))
}

fn source_length(path: &Path) -> Result<u64, commands::CliError> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|source| commands::CliError::Read {
            path: path.to_owned(),
            source,
        })
}

fn print_edit_failure(
    source: &[u8],
    operation: &'static str,
    target: &str,
    diagnostics: &[Diagnostic],
    kind: &'static str,
    message: &str,
) -> Result<ExitCode, commands::CliError> {
    let output = EditOutput {
        version: "marksheet-edit@1",
        profile: "portable-a1@1",
        status: "invalid",
        operation,
        target,
        changed: false,
        valid: false,
        before: SourceVersion::new(source),
        after: SourceVersion::new(source),
        patches: Vec::new(),
        diagnostics: json_diagnostics(source, diagnostics),
        error: Some(AutomationError { kind, message }),
    };
    print_json(&output)?;
    Ok(ExitCode::from(1))
}

fn edit_error_kind(kind: EditErrorKind) -> &'static str {
    match kind {
        EditErrorKind::Conflict => "conflict",
        EditErrorKind::InvalidBase => "invalid_base",
        EditErrorKind::UnsupportedOperationCombination => "unsupported_operation_combination",
        EditErrorKind::TargetNotFound => "target_not_found",
        EditErrorKind::AbsentCell => "absent_cell",
        EditErrorKind::VirtualCell => "virtual_cell",
        EditErrorKind::UnlocatableSource => "unlocatable_source",
        EditErrorKind::WidthMismatch => "width_mismatch",
        EditErrorKind::IdentifierCollision => "identifier_collision",
        EditErrorKind::InvalidIdentifier => "invalid_identifier",
        EditErrorKind::PartialFootprint => "partial_footprint",
        EditErrorKind::DestinationOverlap => "destination_overlap",
        EditErrorKind::InvalidMove => "invalid_move",
        EditErrorKind::InvalidValue => "invalid_value",
        EditErrorKind::InvalidNameTarget => "invalid_name_target",
        EditErrorKind::InvalidGeometry => "invalid_geometry",
        EditErrorKind::InvalidStyle => "invalid_style",
        EditErrorKind::ReferenceRewrite => "reference_rewrite",
        EditErrorKind::PatchPlan => "patch_plan",
        EditErrorKind::InvalidResult => "invalid_result",
    }
}

fn inspect_workbook(workbook: &Workbook, prepared: Option<&PreparedWorkbook>) -> InspectWorkbook {
    let sheets = workbook
        .sheets
        .iter()
        .map(|sheet| {
            let index = prepared.and_then(|prepared| prepared.sheet(&sheet.id));
            let authored_cell_count = sheet
                .items
                .iter()
                .map(|item| match item {
                    marksheet_model::SheetItem::Block(block) => {
                        block.cells.iter().map(Vec::len).sum()
                    }
                    marksheet_model::SheetItem::Table(table) => {
                        table.block.cells.iter().map(Vec::len).sum()
                    }
                    _ => 0,
                })
                .sum();
            let fill_count = sheet
                .items
                .iter()
                .filter(|item| matches!(item, marksheet_model::SheetItem::Fill(_)))
                .count();
            InspectSheet {
                id: sheet.id.to_string(),
                label: sheet.label.clone(),
                authored_cell_count,
                virtual_cell_count: index.map_or(0, |index| index.virtual_cells.len()),
                fill_count,
                tables: inspect_source_tables(sheet),
            }
        })
        .collect();
    let names = workbook
        .names
        .iter()
        .map(|name| InspectName {
            id: name.id.to_string(),
            target: InspectNameTarget::from(&name.target),
            resolved: prepared
                .and_then(|prepared| resolve_name(prepared, &name.id, &name.target).ok())
                .map(|resolved| InspectResolvedName {
                    sheet: resolved.sheet.to_string(),
                    range: resolved.range.to_string(),
                }),
        })
        .collect();
    let extensions = workbook
        .extensions
        .iter()
        .map(|declaration| InspectExtensionDeclaration {
            id: extension_id(&declaration.capability),
            required: declaration.required,
        })
        .collect();
    let mut extension_instances = workbook
        .extension_instances
        .iter()
        .map(|instance| InspectExtensionInstance {
            id: extension_id(&instance.capability),
            scope: "workbook".to_owned(),
            name: instance.name.clone(),
        })
        .collect::<Vec<_>>();
    for sheet in &workbook.sheets {
        extension_instances.extend(sheet.items.iter().filter_map(|item| {
            let marksheet_model::SheetItem::Extension(instance) = item else {
                return None;
            };
            Some(InspectExtensionInstance {
                id: extension_id(&instance.capability),
                scope: format!("sheet:{}", sheet.id),
                name: instance.name.clone(),
            })
        }));
    }
    InspectWorkbook {
        settings: workbook.settings.clone(),
        sheets,
        names,
        extensions,
        extension_instances,
    }
}

fn inspect_source_tables(sheet: &marksheet_model::Sheet) -> Vec<InspectTable> {
    sheet
        .items
        .iter()
        .filter_map(|item| {
            let marksheet_model::SheetItem::Table(table) = item else {
                return None;
            };
            let range = table
                .block
                .footprint()
                .ok()
                .and_then(|footprint| footprint.range().ok());
            let data_range = range.and_then(|range| {
                (range.start.row < range.end.row).then(|| Range {
                    start: Coordinate {
                        column: range.start.column,
                        row: range.start.row + 1,
                    },
                    end: range.end,
                })
            });
            let headers = table
                .block
                .cells
                .first()
                .into_iter()
                .flatten()
                .map(|cell| match &cell.value {
                    Value::Text(header) => header.clone(),
                    _ => "<invalid-header>".to_owned(),
                })
                .collect();
            Some(InspectTable {
                id: table.id.to_string(),
                range: range.map(|range| range.to_string()),
                headers,
                data_range: data_range.map(|range| range.to_string()),
            })
        })
        .collect()
}

fn extension_id(id: &ExtensionId) -> String {
    format!("{}@{}", id.id, id.major)
}

#[derive(Serialize)]
struct InspectOutput {
    version: &'static str,
    profile: &'static str,
    status: &'static str,
    source: SourceVersion,
    workbook: Option<InspectWorkbook>,
    diagnostics: Vec<render::JsonDiagnostic>,
    error: Option<AutomationError<'static>>,
}

#[derive(Serialize)]
struct InspectWorkbook {
    settings: marksheet_model::WorkbookSettings,
    sheets: Vec<InspectSheet>,
    names: Vec<InspectName>,
    extensions: Vec<InspectExtensionDeclaration>,
    extension_instances: Vec<InspectExtensionInstance>,
}

#[derive(Serialize)]
struct InspectSheet {
    id: String,
    label: String,
    authored_cell_count: usize,
    virtual_cell_count: usize,
    fill_count: usize,
    tables: Vec<InspectTable>,
}

#[derive(Serialize)]
struct InspectTable {
    id: String,
    range: Option<String>,
    headers: Vec<String>,
    data_range: Option<String>,
}

#[derive(Serialize)]
struct InspectName {
    id: String,
    target: InspectNameTarget,
    resolved: Option<InspectResolvedName>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum InspectNameTarget {
    Cell { sheet: String, coordinate: String },
    Range { sheet: String, range: String },
    TableColumn { table: String, header: String },
}

impl From<&NameTarget> for InspectNameTarget {
    fn from(target: &NameTarget) -> Self {
        match target {
            NameTarget::Cell(cell) => Self::Cell {
                sheet: cell.sheet.to_string(),
                coordinate: cell.coordinate.to_string(),
            },
            NameTarget::Range(range) => Self::Range {
                sheet: range.sheet.to_string(),
                range: range.range.to_string(),
            },
            NameTarget::TableColumn { table, header } => Self::TableColumn {
                table: table.to_string(),
                header: header.clone(),
            },
        }
    }
}

#[derive(Serialize)]
struct InspectResolvedName {
    sheet: String,
    range: String,
}

#[derive(Serialize)]
struct InspectExtensionDeclaration {
    id: String,
    required: bool,
}

#[derive(Serialize)]
struct InspectExtensionInstance {
    id: String,
    scope: String,
    name: String,
}

#[derive(Serialize)]
struct GetOutput<'a> {
    version: &'static str,
    profile: &'static str,
    status: &'static str,
    requested_target: &'a str,
    target: Option<JsonResolvedTarget>,
    calculated: bool,
    cells: Vec<GetCell>,
    diagnostics: Vec<render::JsonDiagnostic>,
    error: Option<AutomationError<'a>>,
}

#[derive(Serialize)]
// These independent booleans are part of the versioned wire contract: clients
// must distinguish requested check mode, actual mutation, planned difference,
// and semantic validity without inferring one from another.
#[allow(clippy::struct_excessive_bools)]
struct FormatOutput<'a> {
    version: &'static str,
    profile: &'static str,
    status: &'static str,
    check_only: bool,
    changed: bool,
    would_change: bool,
    valid: bool,
    before: SourceVersion,
    after: SourceVersion,
    proposed: Option<SourceVersion>,
    patches: Vec<JsonPatch>,
    diagnostics: Vec<render::JsonDiagnostic>,
    error: Option<AutomationError<'a>>,
}

#[derive(Serialize)]
struct JsonResolvedTarget {
    kind: TargetKind,
    id: Option<String>,
    sheet: String,
    range: String,
}

impl From<&ResolvedTarget> for JsonResolvedTarget {
    fn from(target: &ResolvedTarget) -> Self {
        Self {
            kind: target.kind,
            id: target.id.clone(),
            sheet: target.sheet.to_string(),
            range: target.range.to_string(),
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum TargetKind {
    Range,
    Name,
    Table,
}

struct ResolvedTarget {
    kind: TargetKind,
    id: Option<String>,
    sheet: SheetId,
    range: Range,
    settable: bool,
}

#[derive(Serialize)]
struct GetCell {
    coordinate: String,
    source: CellSource,
    authored: Option<Value>,
    virtual_formula: Option<String>,
    calculated: Option<CalcValue>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CellSource {
    Authored,
    Virtual,
    Absent,
}

#[derive(Serialize)]
struct EditOutput<'a> {
    version: &'static str,
    profile: &'static str,
    status: &'static str,
    operation: &'static str,
    target: &'a str,
    changed: bool,
    valid: bool,
    before: SourceVersion,
    after: SourceVersion,
    patches: Vec<JsonPatch>,
    diagnostics: Vec<render::JsonDiagnostic>,
    error: Option<AutomationError<'a>>,
}

#[derive(Serialize)]
struct JsonPatch {
    start: u64,
    end: u64,
    replacement: String,
}

#[derive(Clone, Serialize)]
struct SourceVersion {
    byte_length: u64,
    fnv1a64: Option<String>,
}

impl SourceVersion {
    fn new(source: &[u8]) -> Self {
        Self::from_fingerprint(SourceFingerprint::of(source))
    }

    fn from_fingerprint(fingerprint: SourceFingerprint) -> Self {
        Self {
            byte_length: fingerprint.byte_len,
            fnv1a64: Some(format!("{:016x}", fingerprint.fnv1a64)),
        }
    }

    const fn unhashed(byte_length: u64) -> Self {
        Self {
            byte_length,
            fnv1a64: None,
        }
    }
}

#[derive(Serialize)]
struct AutomationError<'a> {
    kind: &'static str,
    message: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_format_candidate_must_pass_admission() {
        let diagnostics = validate_format_candidate(
            b"#!marksheet 0.1\n",
            b"#!marksheet 0.1\n@sheet broken\n",
            &[],
        )
        .expect_err("an invalid changed candidate must be refused in every mode");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error)
        );
    }
}
