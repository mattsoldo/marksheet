//! Diagnostic rendering helpers.
//!
//! Keeping presentation separate from command execution makes the JSON shape a
//! stable, machine-facing contract and lets `fmt` reuse the human renderer.

use std::{
    fmt,
    io::{self, Write},
    path::Path,
};

use marksheet_calc::{CalculationRequest, CalculationResult, eval::CalcValue};
use marksheet_edit::diff::{
    CellChange, ComparisonScope, SemanticChange, SemanticDiff, SemanticSheetItem,
    SemanticStyleEffectComponent, SemanticStyleProperties, SemanticValue,
};
use marksheet_model::{ByteSpan, Diagnostic, ExtensionId, FillTarget, LineIndex, Severity};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{CalcOutputFormat, DiffOutputFormat, OutputFormat};

/// One source-scoped diagnostic produced while validating a diff input.
///
/// Inputs remain borrowed so the command can render diagnostics for both
/// workbooks without copying potentially large source files.
pub(crate) struct DiffDiagnostic<'a> {
    pub(crate) path: &'a Path,
    pub(crate) source: &'a [u8],
    pub(crate) diagnostic: Diagnostic,
}

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

/// Renders validation errors for either input of `marksheet diff` without
/// producing a partial change report.
pub(crate) fn render_diff_diagnostics(
    diagnostics: &[DiffDiagnostic<'_>],
    format: DiffOutputFormat,
) -> io::Result<()> {
    match format {
        DiffOutputFormat::Human => {
            for diagnostic in diagnostics {
                render_human(
                    diagnostic.path,
                    diagnostic.source,
                    std::slice::from_ref(&diagnostic.diagnostic),
                )?;
            }
            Ok(())
        }
        DiffOutputFormat::Json => {
            let diagnostics: Vec<_> = diagnostics
                .iter()
                .map(|entry| {
                    let line_index = std::str::from_utf8(entry.source).ok().map(LineIndex::new);
                    JsonDiffDiagnostic {
                        path: entry.path.display().to_string(),
                        diagnostic: JsonDiagnostic::from_diagnostic(
                            &entry.diagnostic,
                            line_index.as_ref(),
                        ),
                    }
                })
                .collect();
            let output = JsonDiffInvalid {
                version: "marksheet-diff@1",
                profile: "portable-a1@1",
                status: "invalid",
                diagnostics,
            };
            let mut stdout = io::stdout().lock();
            serde_json::to_writer_pretty(&mut stdout, &output).map_err(io::Error::other)?;
            writeln!(stdout)
        }
    }
}

/// Formats a completed semantic diff. Human output is intentionally empty for
/// an equivalent pair, while JSON always emits a versioned envelope that is
/// safe for automation to parse.
pub(crate) fn format_semantic_diff(
    diff: &SemanticDiff,
    format: DiffOutputFormat,
) -> Result<String, serde_json::Error> {
    match format {
        DiffOutputFormat::Human => Ok(format_human_diff(diff)),
        DiffOutputFormat::Json => serde_json::to_string_pretty(&JsonSemanticDiff::new(diff))
            .map(|output| format!("{output}\n")),
    }
}

fn format_human_diff(diff: &SemanticDiff) -> String {
    let mut output = String::new();
    for change in &diff.changes {
        match change {
            SemanticChange::SettingsChanged { .. } => {
                output.push_str("changed workbook settings\n");
            }
            SemanticChange::SheetOrderChanged { before, after } => {
                append_formatted(
                    &mut output,
                    format_args!(
                        "changed sheet order: {} -> {}\n",
                        format_identifiers(before),
                        format_identifiers(after)
                    ),
                );
            }
            SemanticChange::SheetAdded { sheet, label } => {
                append_formatted(
                    &mut output,
                    format_args!("added sheet {sheet} ({label:?})\n"),
                );
            }
            SemanticChange::SheetRemoved { sheet, label } => {
                append_formatted(
                    &mut output,
                    format_args!("removed sheet {sheet} ({label:?})\n"),
                );
            }
            SemanticChange::SheetLabelChanged {
                sheet,
                before,
                after,
            } => append_formatted(
                &mut output,
                format_args!("changed sheet label {sheet}: {before:?} -> {after:?}\n"),
            ),
            SemanticChange::CellsChanged { sheet, cells } => {
                for cell in cells {
                    append_human_cell_change(&mut output, sheet.as_str(), cell);
                }
            }
            SemanticChange::SheetItemsChanged {
                sheet,
                before,
                after,
            } => append_human_sheet_item_changes(&mut output, sheet.as_str(), before, after),
            SemanticChange::StyleEffectsChanged {
                sheet,
                before,
                after,
            } => append_human_style_effect_changes(&mut output, sheet.as_str(), before, after),
            SemanticChange::StyleAdded { style } => {
                append_formatted(&mut output, format_args!("added style {}\n", style.id));
            }
            SemanticChange::StyleRemoved { style } => {
                append_formatted(&mut output, format_args!("removed style {}\n", style.id));
            }
            SemanticChange::StyleChanged { id, .. } => {
                append_formatted(&mut output, format_args!("changed style {id}\n"));
            }
            SemanticChange::NameAdded { name } => {
                append_formatted(&mut output, format_args!("added name {}\n", name.id));
            }
            SemanticChange::NameRemoved { name } => {
                append_formatted(&mut output, format_args!("removed name {}\n", name.id));
            }
            SemanticChange::NameChanged { id, .. } => {
                append_formatted(&mut output, format_args!("changed name {id}\n"));
            }
            SemanticChange::ExtensionDeclarationsChanged { .. } => {
                output.push_str("changed extension declarations\n");
            }
            SemanticChange::WorkbookExtensionsChanged { .. } => {
                output.push_str("changed workbook extensions\n");
            }
            SemanticChange::UnsupportedComparison(issue) => append_formatted(
                &mut output,
                format_args!(
                    "unsupported semantic comparison at {}: {}\n",
                    format_scope(&issue.scope),
                    issue.explanation
                ),
            ),
        }
    }
    output
}

fn append_human_cell_change(output: &mut String, sheet: &str, change: &CellChange) {
    append_formatted(
        output,
        format_args!(
            "changed cell {sheet}!{}: {} -> {}\n",
            change.coordinate,
            change
                .before
                .as_ref()
                .map_or_else(|| "<absent>".to_owned(), format_semantic_value),
            change
                .after
                .as_ref()
                .map_or_else(|| "<absent>".to_owned(), format_semantic_value),
        ),
    );
}

fn append_human_sheet_item_changes(
    output: &mut String,
    sheet: &str,
    before: &[SemanticSheetItem],
    after: &[SemanticSheetItem],
) {
    let item_count = before.len().max(after.len());
    for index in 0..item_count {
        let before_item = before.get(index);
        let after_item = after.get(index);
        if before_item == after_item {
            continue;
        }
        append_formatted(
            output,
            format_args!(
                "changed sheet item {sheet}[{index}]: {} -> {}\n",
                before_item.map_or_else(|| "<absent>".to_owned(), format_sheet_item),
                after_item.map_or_else(|| "<absent>".to_owned(), format_sheet_item),
            ),
        );
    }
}

fn format_sheet_item(item: &SemanticSheetItem) -> String {
    match item {
        SemanticSheetItem::Table(table) => format!(
            "table {} at {} ({} headers, {} data rows)",
            table.id,
            table.anchor,
            table.headers.len(),
            table.data_row_count
        ),
        SemanticSheetItem::Fill(fill) => {
            format!(
                "fill {} with {}",
                format_fill_target(&fill.target),
                fill.formula
            )
        }
        SemanticSheetItem::ColumnGeometry(geometry) => format!(
            "column geometry {}:{} width {}",
            geometry.columns.start, geometry.columns.end, geometry.width.value
        ),
        SemanticSheetItem::RowGeometry(geometry) => format!(
            "row geometry {}:{} height {}",
            geometry.rows.start, geometry.rows.end, geometry.height.value
        ),
        // Extension payloads are deliberately omitted: they can be arbitrarily
        // large and are opaque to the core. Placement, capability, and name
        // still uniquely explain which extension instance changed.
        SemanticSheetItem::Extension(extension) => format!(
            "extension {} {} at source item {}",
            format_extension_id(&extension.extension.capability),
            extension.extension.name,
            extension.item_ordinal
        ),
    }
}

fn format_fill_target(target: &FillTarget) -> String {
    match target {
        FillTarget::Range(range) => range.to_string(),
        FillTarget::TableColumn { table, header } => format!("{table}[{header}]"),
    }
}

fn format_extension_id(capability: &ExtensionId) -> String {
    format!("{}@{}", capability.id, capability.major)
}

fn append_human_style_effect_changes(
    output: &mut String,
    sheet: &str,
    before: &[SemanticStyleEffectComponent],
    after: &[SemanticStyleEffectComponent],
) {
    let component_count = before.len().max(after.len());
    for index in 0..component_count {
        let before_component = before.get(index);
        let after_component = after.get(index);
        if before_component == after_component {
            continue;
        }
        append_formatted(
            output,
            format_args!(
                "changed style effects {sheet}[{index}]: {} -> {}\n",
                before_component
                    .map_or_else(|| "<absent>".to_owned(), format_style_effect_component),
                after_component
                    .map_or_else(|| "<absent>".to_owned(), format_style_effect_component),
            ),
        );
    }
}

fn format_style_effect_component(component: &SemanticStyleEffectComponent) -> String {
    let effects = component
        .effects
        .iter()
        .map(|effect| {
            format!(
                "{} ({})",
                effect.range,
                format_style_property_keys(&effect.properties)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("style effects [{effects}]")
}

fn format_style_property_keys(properties: &SemanticStyleProperties) -> String {
    let mut keys = Vec::new();
    if properties.bold.is_some() {
        keys.push("bold");
    }
    if properties.italic.is_some() {
        keys.push("italic");
    }
    if properties.wrap.is_some() {
        keys.push("wrap");
    }
    if properties.text_color.is_some() {
        keys.push("text-color");
    }
    if properties.fill.is_some() {
        keys.push("fill");
    }
    if properties.font_size.is_some() {
        keys.push("font-size");
    }
    if properties.align.is_some() {
        keys.push("align");
    }
    if properties.valign.is_some() {
        keys.push("valign");
    }
    if properties.number.is_some() {
        keys.push("number");
    }
    if properties.decimals.is_some() {
        keys.push("decimals");
    }
    if properties.currency.is_some() {
        keys.push("currency");
    }
    keys.join(",")
}

fn append_formatted(output: &mut String, arguments: fmt::Arguments<'_>) {
    use std::fmt::Write as _;

    output
        .write_fmt(arguments)
        .expect("writing to a String cannot fail");
}

fn format_identifiers<T: std::fmt::Display>(identifiers: &[T]) -> String {
    identifiers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn format_semantic_value(value: &SemanticValue) -> String {
    match value {
        SemanticValue::Blank => "blank".to_owned(),
        SemanticValue::Text(value) => format!("text {value:?}"),
        SemanticValue::Number(value) => format!("number {}", value.value),
        SemanticValue::Boolean(value) => format!("boolean {value}"),
        SemanticValue::Date(value) => format!("date {value}"),
        SemanticValue::DateTime(value) => format!("datetime {}", value.value),
        SemanticValue::Formula(value) => format!("formula {value}"),
        SemanticValue::Error(value) => format!("error {value}"),
    }
}

fn format_scope(scope: &ComparisonScope) -> String {
    match scope {
        ComparisonScope::Workbook => "workbook".to_owned(),
        ComparisonScope::Settings => "settings".to_owned(),
        ComparisonScope::Styles => "styles".to_owned(),
        ComparisonScope::Names => "names".to_owned(),
        ComparisonScope::ExtensionDeclarations => "extension declarations".to_owned(),
        ComparisonScope::WorkbookExtensions => "workbook extensions".to_owned(),
        ComparisonScope::Sheet(sheet) => format!("sheet {sheet}"),
        ComparisonScope::Cell { sheet, coordinate } => format!("cell {sheet}!{coordinate}"),
        ComparisonScope::SheetItem { sheet, index } => format!("sheet item {sheet}[{index}]"),
    }
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
    if let Some(line_index) = line_index {
        if let Ok(position) = line_index.line_column(span.start) {
            return write!(writer, ":{}:{}", position.line, position.column);
        }
    }
    write!(writer, ":byte {}..{}", span.start, span.end)
}

fn render_json(source: &[u8], diagnostics: &[Diagnostic]) -> io::Result<()> {
    let line_index = std::str::from_utf8(source).ok().map(LineIndex::new);
    let diagnostics: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| JsonDiagnostic::from_diagnostic(diagnostic, line_index.as_ref()))
        .collect();
    let output = JsonCheck {
        version: "marksheet-check@1",
        profile: "portable-a1@1",
        status: if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error")
        {
            "invalid"
        } else {
            "ok"
        },
        diagnostics,
    };
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &output).map_err(io::Error::other)?;
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
pub(crate) struct JsonDiagnostic {
    code: String,
    severity: &'static str,
    message: String,
    primary: JsonSpan,
    related: Vec<JsonRelatedDiagnostic>,
    context: Option<marksheet_model::DiagnosticContext>,
    suggestion: Option<marksheet_model::Suggestion>,
}

#[derive(Serialize)]
struct JsonCheck {
    version: &'static str,
    profile: &'static str,
    status: &'static str,
    diagnostics: Vec<JsonDiagnostic>,
}

#[derive(Serialize)]
struct JsonDiffDiagnostic {
    path: String,
    #[serde(flatten)]
    diagnostic: JsonDiagnostic,
}

#[derive(Serialize)]
struct JsonDiffInvalid {
    version: &'static str,
    profile: &'static str,
    status: &'static str,
    diagnostics: Vec<JsonDiffDiagnostic>,
}

#[derive(Serialize)]
struct JsonSemanticDiff {
    version: &'static str,
    profile: &'static str,
    equivalent: bool,
    change_count: usize,
    changes: Vec<Value>,
}

impl JsonSemanticDiff {
    fn new(diff: &SemanticDiff) -> Self {
        Self {
            version: "marksheet-diff@1",
            profile: "portable-a1@1",
            equivalent: diff.is_empty(),
            change_count: diff.changes.len(),
            changes: diff.changes.iter().map(json_change).collect(),
        }
    }
}

fn json_change(change: &SemanticChange) -> Value {
    match change {
        SemanticChange::SettingsChanged { before, after } => serde_json::json!({
            "kind": "settings_changed",
            "before": before,
            "after": after,
        }),
        SemanticChange::SheetOrderChanged { before, after } => serde_json::json!({
            "kind": "sheet_order_changed",
            "before": before,
            "after": after,
        }),
        SemanticChange::SheetAdded { sheet, label } => serde_json::json!({
            "kind": "sheet_added",
            "sheet": sheet.as_str(),
            "label": label,
        }),
        SemanticChange::SheetRemoved { sheet, label } => serde_json::json!({
            "kind": "sheet_removed",
            "sheet": sheet.as_str(),
            "label": label,
        }),
        SemanticChange::SheetLabelChanged {
            sheet,
            before,
            after,
        } => serde_json::json!({
            "kind": "sheet_label_changed",
            "sheet": sheet.as_str(),
            "before": before,
            "after": after,
        }),
        SemanticChange::CellsChanged { sheet, cells } => serde_json::json!({
            "kind": "cells_changed",
            "sheet": sheet.as_str(),
            "cells": cells.iter().map(json_cell_change).collect::<Vec<_>>(),
        }),
        SemanticChange::SheetItemsChanged {
            sheet,
            before,
            after,
        } => serde_json::json!({
            "kind": "sheet_items_changed",
            "sheet": sheet.as_str(),
            "before_count": before.len(),
            "after_count": after.len(),
            "items": json_sheet_item_changes(before, after),
        }),
        SemanticChange::StyleEffectsChanged {
            sheet,
            before,
            after,
        } => serde_json::json!({
            "kind": "style_effects_changed",
            "sheet": sheet.as_str(),
            "before_count": before.len(),
            "after_count": after.len(),
            "components": json_style_effect_changes(before, after),
        }),
        SemanticChange::StyleAdded { style } => serde_json::json!({
            "kind": "style_added",
            "style": style.id.as_str(),
        }),
        SemanticChange::StyleRemoved { style } => serde_json::json!({
            "kind": "style_removed",
            "style": style.id.as_str(),
        }),
        SemanticChange::StyleChanged { id, .. } => serde_json::json!({
            "kind": "style_changed",
            "style": id.as_str(),
        }),
        SemanticChange::NameAdded { name } => serde_json::json!({
            "kind": "name_added",
            "name": name.id.as_str(),
        }),
        SemanticChange::NameRemoved { name } => serde_json::json!({
            "kind": "name_removed",
            "name": name.id.as_str(),
        }),
        SemanticChange::NameChanged { id, .. } => serde_json::json!({
            "kind": "name_changed",
            "name": id.as_str(),
        }),
        SemanticChange::ExtensionDeclarationsChanged { before, after } => serde_json::json!({
            "kind": "extension_declarations_changed",
            "before_count": before.len(),
            "after_count": after.len(),
        }),
        SemanticChange::WorkbookExtensionsChanged { before, after } => serde_json::json!({
            "kind": "workbook_extensions_changed",
            "before_count": before.len(),
            "after_count": after.len(),
        }),
        SemanticChange::UnsupportedComparison(issue) => serde_json::json!({
            "kind": "unsupported_comparison",
            "scope": json_scope(&issue.scope),
            "explanation": issue.explanation,
        }),
    }
}

fn json_cell_change(change: &CellChange) -> Value {
    serde_json::json!({
        "coordinate": change.coordinate.to_string(),
        "before": change.before.as_ref().map(json_semantic_value),
        "after": change.after.as_ref().map(json_semantic_value),
    })
}

fn json_sheet_item_changes(
    before: &[SemanticSheetItem],
    after: &[SemanticSheetItem],
) -> Vec<Value> {
    let item_count = before.len().max(after.len());
    (0..item_count)
        .filter_map(|index| {
            let before_item = before.get(index);
            let after_item = after.get(index);
            (before_item != after_item).then(|| {
                serde_json::json!({
                    "index": index,
                    "before": before_item.map(json_sheet_item),
                    "after": after_item.map(json_sheet_item),
                })
            })
        })
        .collect()
}

fn json_sheet_item(item: &SemanticSheetItem) -> Value {
    match item {
        SemanticSheetItem::Table(table) => serde_json::json!({
            "kind": "table",
            "id": table.id.as_str(),
            "anchor": table.anchor.to_string(),
            "header_count": table.headers.len(),
            "data_row_count": table.data_row_count,
        }),
        SemanticSheetItem::Fill(fill) => serde_json::json!({
            "kind": "fill",
            "target": json_fill_target(&fill.target),
            "formula": fill.formula,
        }),
        SemanticSheetItem::ColumnGeometry(geometry) => serde_json::json!({
            "kind": "column_geometry",
            "columns": { "start": geometry.columns.start, "end": geometry.columns.end },
            "width": geometry.width.value,
        }),
        SemanticSheetItem::RowGeometry(geometry) => serde_json::json!({
            "kind": "row_geometry",
            "rows": { "start": geometry.rows.start, "end": geometry.rows.end },
            "height": geometry.height.value,
        }),
        SemanticSheetItem::Extension(extension) => serde_json::json!({
            "kind": "extension",
            "item_ordinal": extension.item_ordinal,
            "capability": format_extension_id(&extension.extension.capability),
            "name": extension.extension.name,
        }),
    }
}

fn json_fill_target(target: &FillTarget) -> Value {
    match target {
        FillTarget::Range(range) => serde_json::json!({
            "kind": "range",
            "range": range.to_string(),
        }),
        FillTarget::TableColumn { table, header } => serde_json::json!({
            "kind": "table_column",
            "table": table.as_str(),
            "header": header,
        }),
    }
}

fn json_style_effect_changes(
    before: &[SemanticStyleEffectComponent],
    after: &[SemanticStyleEffectComponent],
) -> Vec<Value> {
    let component_count = before.len().max(after.len());
    (0..component_count)
        .filter_map(|index| {
            let before_component = before.get(index);
            let after_component = after.get(index);
            (before_component != after_component).then(|| {
                serde_json::json!({
                    "index": index,
                    "before": before_component.map(json_style_effect_component),
                    "after": after_component.map(json_style_effect_component),
                })
            })
        })
        .collect()
}

fn json_style_effect_component(component: &SemanticStyleEffectComponent) -> Value {
    serde_json::json!({
        "effects": component.effects.iter().map(|effect| serde_json::json!({
            "range": effect.range.to_string(),
            "properties": json_style_properties(&effect.properties),
        })).collect::<Vec<_>>(),
    })
}

fn json_style_properties(properties: &SemanticStyleProperties) -> Value {
    serde_json::json!({
        "bold": properties.bold,
        "italic": properties.italic,
        "wrap": properties.wrap,
        "text_color": properties.text_color.as_ref().map(marksheet_model::Color::as_str),
        "fill": properties.fill.as_ref().map(marksheet_model::Color::as_str),
        "font_size": properties.font_size.map(|size| size.value),
        "align": properties.align,
        "valign": properties.valign,
        "number": properties.number,
        "decimals": properties.decimals,
        "currency": properties.currency,
    })
}

fn json_semantic_value(value: &SemanticValue) -> Value {
    let mut object = Map::new();
    match value {
        SemanticValue::Blank => {
            object.insert("kind".to_owned(), Value::String("blank".to_owned()));
        }
        SemanticValue::Text(value) => {
            object.insert("kind".to_owned(), Value::String("text".to_owned()));
            object.insert("value".to_owned(), Value::String(value.clone()));
        }
        SemanticValue::Number(value) => {
            object.insert("kind".to_owned(), Value::String("number".to_owned()));
            object.insert("value".to_owned(), serde_json::json!(value.value));
        }
        SemanticValue::Boolean(value) => {
            object.insert("kind".to_owned(), Value::String("boolean".to_owned()));
            object.insert("value".to_owned(), Value::Bool(*value));
        }
        SemanticValue::Date(value) => {
            object.insert("kind".to_owned(), Value::String("date".to_owned()));
            object.insert("value".to_owned(), Value::String(value.to_string()));
        }
        SemanticValue::DateTime(value) => {
            object.insert("kind".to_owned(), Value::String("datetime".to_owned()));
            object.insert("value".to_owned(), Value::String(value.value.to_string()));
        }
        SemanticValue::Formula(value) => {
            object.insert("kind".to_owned(), Value::String("formula".to_owned()));
            object.insert("value".to_owned(), Value::String(value.clone()));
        }
        SemanticValue::Error(value) => {
            object.insert("kind".to_owned(), Value::String("error".to_owned()));
            object.insert("value".to_owned(), Value::String(value.to_string()));
        }
    }
    Value::Object(object)
}

fn json_scope(scope: &ComparisonScope) -> Value {
    match scope {
        ComparisonScope::Workbook => serde_json::json!({ "kind": "workbook" }),
        ComparisonScope::Settings => serde_json::json!({ "kind": "settings" }),
        ComparisonScope::Styles => serde_json::json!({ "kind": "styles" }),
        ComparisonScope::Names => serde_json::json!({ "kind": "names" }),
        ComparisonScope::ExtensionDeclarations => {
            serde_json::json!({ "kind": "extension_declarations" })
        }
        ComparisonScope::WorkbookExtensions => {
            serde_json::json!({ "kind": "workbook_extensions" })
        }
        ComparisonScope::Sheet(sheet) => serde_json::json!({
            "kind": "sheet",
            "sheet": sheet.as_str(),
        }),
        ComparisonScope::Cell { sheet, coordinate } => serde_json::json!({
            "kind": "cell",
            "sheet": sheet.as_str(),
            "coordinate": coordinate.to_string(),
        }),
        ComparisonScope::SheetItem { sheet, index } => serde_json::json!({
            "kind": "sheet_item",
            "sheet": sheet.as_str(),
            "index": index,
        }),
    }
}

impl JsonDiagnostic {
    pub(crate) fn from_diagnostic(diagnostic: &Diagnostic, line_index: Option<&LineIndex>) -> Self {
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
