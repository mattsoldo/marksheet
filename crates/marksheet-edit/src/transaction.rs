//! Atomic, source-preserving edit transactions.
//!
//! Planning always starts from one valid source snapshot.  Every patch refers
//! to that snapshot, and the patched document is reparsed and formula-validated
//! before any result is returned.  This keeps semantic edits, source patches,
//! and undo data on the same transactional boundary.

use std::{fmt, str};

use marksheet_calc::{
    formula::{A1Move, FormulaPatch, FormulaRewrite, FormulaRewriteError, rewrite_formula_text},
    prepare::{CompileLimits, PrepareLimits, PreparedWorkbook, compile_formulas},
};
use marksheet_model::{
    ApplyTarget, Block, ByteSpan, Coordinate, Diagnostic, Fill, FillTarget, NameId, Range, SheetId,
    SheetItem, StyleId, Table, TableId, TableRegion, Value, Workbook,
};
use marksheet_syntax::{ParsedDocument, SourceMap, parse};
use serde::{Deserialize, Serialize};

use crate::{
    csv::{EncodeError, FieldContext, encode_field},
    inverse::InverseTransaction,
    patch::{PatchError, PatchSet, SourcePatch},
};

/// A deterministic, inexpensive identity for one exact source snapshot.
///
/// [`PatchSet`] additionally binds itself to the complete source bytes.  The
/// fingerprint exists so callers can reject stale intent before planning and
/// can retain a compact precondition alongside an undo/redo entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub byte_len: u64,
    pub fnv1a64: u64,
}

impl SourceFingerprint {
    #[must_use]
    pub fn of(source: &[u8]) -> Self {
        // FNV-1a is stable across platforms and Rust versions.  It is not used
        // as a security primitive: applying the resulting PatchSet still
        // compares every byte of its bound source snapshot.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in source {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self {
            byte_len: u64::try_from(source.len()).unwrap_or(u64::MAX),
            fnv1a64: hash,
        }
    }
}

/// An exact source precondition retained with compact fingerprint metadata.
///
/// The bytes are authoritative. The fingerprint is useful for fast rejection
/// and logging, but never substitutes for the byte-for-byte comparison required
/// at the transaction boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceExpectation {
    pub fingerprint: SourceFingerprint,
    pub bytes: Vec<u8>,
}

impl SourceExpectation {
    #[must_use]
    pub fn capture(source: impl AsRef<[u8]>) -> Self {
        let bytes = source.as_ref().to_vec();
        Self {
            fingerprint: SourceFingerprint::of(&bytes),
            bytes,
        }
    }

    fn matches(&self, source: &[u8], fingerprint: SourceFingerprint) -> bool {
        self.fingerprint == fingerprint && self.bytes == source
    }
}

/// Preconditions checked before a transaction is planned.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EditExpectations {
    /// When present, the source must still be the snapshot on which the caller
    /// based the semantic operation. Exact bytes are authoritative.
    pub source: Option<SourceExpectation>,
}

/// One semantic edit supported by the Milestone 3 transactional core.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditOperation {
    SetCell {
        sheet: SheetId,
        coordinate: Coordinate,
        value: Value,
    },
    AppendTableRow {
        table: TableId,
        fields: Vec<Value>,
    },
    RenameSheetLabel {
        sheet: SheetId,
        label: String,
    },
    RenameSheetId {
        old: SheetId,
        new: SheetId,
    },
    RenameNameId {
        old: NameId,
        new: NameId,
    },
    ApplyStyle {
        sheet: SheetId,
        target: ApplyTarget,
        style: StyleId,
    },
    MoveBlock {
        sheet: SheetId,
        source: Range,
        destination: Coordinate,
    },
}

/// A batch of semantic operations against one source snapshot.
///
/// Operations are planned against the same validated base snapshot. Disjoint
/// edits compose directly, identical replacements deduplicate, and insertions
/// at the same point concatenate in operation order. Conflicting overlapping
/// replacements are rejected as one unsupported combination, without exposing
/// any partial patch plan. [`EditOperation::MoveBlock`] is intentionally
/// single-operation-only: its contextual reference rewrites must see every
/// formula in the semantic snapshot, including formulas another operation
/// could create.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EditTransaction {
    pub operations: Vec<EditOperation>,
    #[serde(default)]
    pub expectations: EditExpectations,
}

impl EditTransaction {
    #[must_use]
    pub fn single(operation: EditOperation) -> Self {
        Self {
            operations: vec![operation],
            expectations: EditExpectations::default(),
        }
    }

    #[must_use]
    pub fn expecting_source(mut self, source: impl AsRef<[u8]>) -> Self {
        self.expectations.source = Some(SourceExpectation::capture(source));
        self
    }

    /// Plans, applies, validates, and constructs undo data atomically.
    ///
    /// # Errors
    ///
    /// Returns a structured error and no patches when a precondition, semantic
    /// target, source-location requirement, or final validation fails.
    pub fn execute(&self, source: &[u8]) -> Result<EditResult, EditError> {
        execute(source, self)
    }
}

/// A committed edit, including exact redo and undo patches.
#[derive(Clone, Debug)]
pub struct EditResult {
    pub operations: Vec<EditOperation>,
    pub patches: PatchSet,
    /// Validated, source-bound undo transaction.
    pub inverse_transaction: InverseTransaction,
    /// Exact inverse patches retained for fixture and API compatibility.
    pub inverse: PatchSet,
    pub source: Vec<u8>,
    pub workbook: Workbook,
    pub diagnostics: Vec<Diagnostic>,
    pub before: SourceFingerprint,
    pub after: SourceFingerprint,
}

impl EditResult {
    #[must_use]
    pub fn changed(&self) -> bool {
        !self.patches.is_empty()
    }
}

/// Stable categories for transaction failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditErrorKind {
    Conflict,
    InvalidBase,
    UnsupportedOperationCombination,
    TargetNotFound,
    AbsentCell,
    VirtualCell,
    UnlocatableSource,
    WidthMismatch,
    IdentifierCollision,
    InvalidIdentifier,
    PartialFootprint,
    DestinationOverlap,
    InvalidMove,
    InvalidValue,
    ReferenceRewrite,
    PatchPlan,
    InvalidResult,
}

/// A transaction failure.  No patch plan is exposed on this path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditError {
    pub kind: EditErrorKind,
    /// Index in [`EditTransaction::operations`], when the failure belongs to a
    /// specific operation rather than the source snapshot as a whole.
    pub operation_index: Option<usize>,
    pub message: String,
    /// Parser/compiler diagnostics are retained when source validation failed.
    pub diagnostics: Vec<Diagnostic>,
}

impl EditError {
    fn new(
        kind: EditErrorKind,
        operation_index: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation_index,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    fn invalid_document(
        kind: EditErrorKind,
        message: impl Into<String>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            kind,
            operation_index: None,
            message: message.into(),
            diagnostics,
        }
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EditError {}

/// Executes a transaction against an exact source snapshot.
///
/// This free-function form is useful for callers that deserialize transaction
/// data before choosing an editor object.
///
/// # Errors
///
/// Returns [`EditError`] without a partial patch plan if a source precondition,
/// operation precondition, patch invariant, or final validation fails.
pub fn execute(source: &[u8], transaction: &EditTransaction) -> Result<EditResult, EditError> {
    let before = SourceFingerprint::of(source);
    if let Some(expected) = &transaction.expectations.source
        && !expected.matches(source, before)
    {
        return Err(EditError::new(
            EditErrorKind::Conflict,
            None,
            "source bytes no longer match the transaction precondition",
        ));
    }
    if transaction.operations.len() > 1
        && transaction
            .operations
            .iter()
            .any(|operation| matches!(operation, EditOperation::MoveBlock { .. }))
    {
        return Err(EditError::new(
            EditErrorKind::UnsupportedOperationCombination,
            None,
            "MoveBlock cannot be combined with another same-base operation",
        ));
    }
    let base = ValidDocument::parse(source, EditErrorKind::InvalidBase)?;
    let mut patches = Vec::new();
    for (index, operation) in transaction.operations.iter().enumerate() {
        plan_operation(source, &base, operation, index, &mut patches)?;
    }
    patches = normalize_combined_patches(patches)?;
    patches.sort_by_key(patch_order);
    remove_unchanged_patches(source, &mut patches)?;

    let patch_set = PatchSet::for_source(source, patches).map_err(|error| patch_error(&error))?;
    let (edited, inverse) = patch_set
        .apply_with_inverse(source)
        .map_err(|error| patch_error(&error))?;
    let validated = ValidDocument::parse(&edited, EditErrorKind::InvalidResult)?;
    let inverse_transaction = InverseTransaction::from_patch_set(inverse.clone());

    Ok(EditResult {
        operations: transaction.operations.clone(),
        patches: patch_set,
        inverse_transaction,
        inverse,
        source: edited.clone(),
        workbook: validated.workbook,
        diagnostics: validated.diagnostics,
        before,
        after: SourceFingerprint::of(&edited),
    })
}

struct ValidDocument {
    document: ParsedDocument,
    workbook: Workbook,
    prepared: PreparedWorkbook,
    diagnostics: Vec<Diagnostic>,
}

impl ValidDocument {
    fn parse(source: &[u8], error_kind: EditErrorKind) -> Result<Self, EditError> {
        let document = parse(source);
        if document.has_errors() {
            return Err(EditError::invalid_document(
                error_kind,
                "transaction source is not a valid Marksheet document",
                document.diagnostics,
            ));
        }
        let Some(workbook) = document.workbook.clone() else {
            return Err(EditError::invalid_document(
                error_kind,
                "transaction source did not produce a complete workbook",
                document.diagnostics,
            ));
        };
        let prepared =
            PreparedWorkbook::build(&workbook, PrepareLimits::default()).map_err(|error| {
                EditError::invalid_document(
                    error_kind,
                    format!("workbook preparation failed: {error}"),
                    document.diagnostics.clone(),
                )
            })?;
        let program =
            compile_formulas(&workbook, &prepared, &CompileLimits::default()).map_err(|error| {
                EditError::invalid_document(
                    error_kind,
                    error.to_string(),
                    document.diagnostics.clone(),
                )
            })?;
        if !program.issues.is_empty() {
            let mut diagnostics = document.diagnostics.clone();
            let formula_diagnostics = program
                .issues
                .iter()
                .map(marksheet_calc::prepare::CompileIssue::to_diagnostic)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    EditError::invalid_document(
                        error_kind,
                        format!("could not construct a formula diagnostic: {error}"),
                        diagnostics.clone(),
                    )
                })?;
            diagnostics.extend(formula_diagnostics);
            return Err(EditError::invalid_document(
                error_kind,
                "transaction source contains invalid or unresolved formulas",
                diagnostics,
            ));
        }
        let diagnostics = document.diagnostics.clone();
        Ok(Self {
            document,
            workbook,
            prepared,
            diagnostics,
        })
    }
}

fn plan_operation(
    source: &[u8],
    base: &ValidDocument,
    operation: &EditOperation,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    match operation {
        EditOperation::SetCell {
            sheet,
            coordinate,
            value,
        } => plan_set_cell(source, base, sheet, *coordinate, value, index, patches),
        EditOperation::AppendTableRow { table, fields } => {
            plan_append_table_row(source, base, table, fields, index, patches)
        }
        EditOperation::RenameSheetLabel { sheet, label } => {
            plan_rename_sheet_label(source, base, sheet, label, index, patches)
        }
        EditOperation::RenameSheetId { old, new } => {
            plan_rename_sheet_id(source, base, old, new, index, patches)
        }
        EditOperation::RenameNameId { old, new } => {
            plan_rename_name_id(source, base, old, new, index, patches)
        }
        EditOperation::ApplyStyle {
            sheet,
            target,
            style,
        } => plan_apply_style(source, base, sheet, target, style, index, patches),
        EditOperation::MoveBlock {
            sheet,
            source: footprint,
            destination,
        } => plan_move_block(
            source,
            base,
            sheet,
            *footprint,
            *destination,
            index,
            patches,
        ),
    }
}

fn plan_set_cell(
    source: &[u8],
    base: &ValidDocument,
    sheet: &SheetId,
    coordinate: Coordinate,
    value: &Value,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    let prepared_sheet = base.prepared.sheet(sheet).ok_or_else(|| {
        operation_error(index, EditErrorKind::TargetNotFound, "sheet does not exist")
    })?;
    // A fill destination has an authored blank placeholder, but its visible
    // value is virtual.  Editing it would materialize over the fill and make
    // the source invalid, so virtual ownership takes precedence here.
    if prepared_sheet.virtual_cell(coordinate).is_some() {
        return Err(operation_error(
            index,
            EditErrorKind::VirtualCell,
            "cannot set a cell represented by an @fill",
        ));
    }
    let Some(authored) = prepared_sheet.authored_cell(coordinate) else {
        return Err(operation_error(
            index,
            EditErrorKind::AbsentCell,
            "cannot set an absent coordinate",
        ));
    };
    if values_equal(&authored.cell.value, value) {
        return Ok(());
    }
    let location = base
        .document
        .source_map
        .cell(sheet, coordinate)
        .ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "authored cell does not have one unambiguous CSV field",
            )
        })?;
    let context = if location.field == location.record {
        FieldContext::SoleFieldRecord
    } else {
        FieldContext::DelimitedRecord
    };
    let replacement = encode_field(value, context).map_err(|error| encode_error(index, error))?;
    push_patch(source, patches, location.field, replacement, index)
}

fn plan_append_table_row(
    source: &[u8],
    base: &ValidDocument,
    table: &TableId,
    fields: &[Value],
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    let semantic_table = base
        .workbook
        .sheets
        .iter()
        .flat_map(|sheet| &sheet.items)
        .find_map(|item| match item {
            SheetItem::Table(candidate) if &candidate.id == table => Some(candidate),
            _ => None,
        })
        .ok_or_else(|| {
            operation_error(index, EditErrorKind::TargetNotFound, "table does not exist")
        })?;
    let width = semantic_table.block.cells.first().map_or(0, Vec::len);
    if fields.len() != width {
        return Err(operation_error(
            index,
            EditErrorKind::WidthMismatch,
            format!(
                "table {table} requires {width} fields, received {}",
                fields.len()
            ),
        ));
    }
    let location = base.document.source_map.table(table).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "table does not have one unambiguous source construct",
        )
    })?;
    let insertion = location.insertion.ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "table does not have a located @end terminator",
        )
    })?;
    let field_context = if width == 1 {
        FieldContext::SoleFieldRecord
    } else {
        FieldContext::DelimitedRecord
    };
    let mut record = Vec::new();
    for (field_index, field) in fields.iter().enumerate() {
        if field_index != 0 {
            record.push(b',');
        }
        record.extend_from_slice(
            &encode_field(field, field_context).map_err(|error| encode_error(index, error))?,
        );
    }
    record.extend_from_slice(table_newline(source, location.body));
    push_patch(source, patches, insertion, record, index)
}

fn plan_rename_sheet_label(
    source: &[u8],
    base: &ValidDocument,
    sheet: &SheetId,
    label: &str,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    let semantic_sheet = base
        .workbook
        .sheets
        .iter()
        .find(|candidate| &candidate.id == sheet)
        .ok_or_else(|| {
            operation_error(index, EditErrorKind::TargetNotFound, "sheet does not exist")
        })?;
    if semantic_sheet.label == label {
        return Ok(());
    }
    let location = base.document.source_map.sheet(sheet).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "sheet does not have one unambiguous declaration",
        )
    })?;
    let label_span = location.label.ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "sheet label token could not be located",
        )
    })?;
    push_patch(
        source,
        patches,
        label_span,
        encode_json_string(label),
        index,
    )
}

fn plan_apply_style(
    source: &[u8],
    base: &ValidDocument,
    sheet: &SheetId,
    target: &ApplyTarget,
    style: &StyleId,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    if !base
        .workbook
        .styles
        .iter()
        .any(|candidate| &candidate.id == style)
    {
        return Err(operation_error(
            index,
            EditErrorKind::TargetNotFound,
            "style does not exist",
        ));
    }
    let semantic_sheet = base
        .workbook
        .sheets
        .iter()
        .find(|candidate| &candidate.id == sheet)
        .ok_or_else(|| {
            operation_error(index, EditErrorKind::TargetNotFound, "sheet does not exist")
        })?;
    validate_apply_target(base, sheet, target, index)?;

    let last_application = semantic_sheet
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            SheetItem::Apply(apply) => Some(apply),
            _ => None,
        });
    if last_application.is_some_and(|apply| {
        &apply.target == target && apply.styles.as_slice() == std::slice::from_ref(style)
    }) {
        return Ok(());
    }

    let (insertion, replacement) =
        apply_style_patch(source, base, semantic_sheet, sheet, target, style, index)?;
    push_patch(source, patches, insertion, replacement, index)
}

#[allow(clippy::too_many_arguments)]
fn apply_style_patch(
    source: &[u8],
    base: &ValidDocument,
    semantic_sheet: &marksheet_model::Sheet,
    sheet: &SheetId,
    target: &ApplyTarget,
    style: &StyleId,
    index: usize,
) -> Result<(ByteSpan, Vec<u8>), EditError> {
    let sheet_location = base.document.source_map.sheet(sheet).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "sheet does not have one unambiguous declaration",
        )
    })?;
    let next_sheet_start = base
        .document
        .source_map
        .sheets()
        .iter()
        .filter_map(|location| {
            (location.directive.line.start > sheet_location.directive.line.start)
                .then_some(location.directive.line.start)
        })
        .min()
        .unwrap_or_else(|| u64::try_from(source.len()).unwrap_or(u64::MAX));
    let apply_locations = base
        .document
        .source_map
        .applies()
        .iter()
        .filter(|location| {
            location.directive.line.start > sheet_location.directive.line.start
                && location.directive.line.start < next_sheet_start
        })
        .collect::<Vec<_>>();
    let semantic_apply_count = semantic_sheet
        .items
        .iter()
        .filter(|item| matches!(item, SheetItem::Apply(_)))
        .count();
    if apply_locations.len() != semantic_apply_count {
        return Err(operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "not every existing style application has an unambiguous source location",
        ));
    }

    let insertion = apply_locations
        .last()
        .map_or_else(
            || {
                base.document.source_map.sheet_insertion(sheet).or_else(|| {
                    // A valid last sheet may end without a physical
                    // newline. Add its separator as part of this insertion.
                    (next_sheet_start == u64::try_from(source.len()).unwrap_or(u64::MAX))
                        .then_some(ByteSpan::empty(next_sheet_start))
                })
            },
            |location| Some(ByteSpan::empty(location.directive.line.end)),
        )
        .ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "sheet does not have a safe directive insertion point",
            )
        })?;
    let line_terminated = newline_before(source, insertion.start);
    let newline = line_terminated
        .or_else(|| newline_at_or_before(source, insertion.start))
        .ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "style application insertion point has no local line-ending convention",
            )
        })?;
    let mut replacement = Vec::new();
    if line_terminated.is_none() {
        replacement.extend_from_slice(newline);
    }
    replacement.extend_from_slice(
        format!("@apply {} {}", format_apply_target(target), style.as_str()).as_bytes(),
    );
    replacement.extend_from_slice(newline);
    Ok((insertion, replacement))
}

fn validate_apply_target(
    base: &ValidDocument,
    sheet: &SheetId,
    target: &ApplyTarget,
    index: usize,
) -> Result<(), EditError> {
    let ApplyTarget::Table { table, region } = target else {
        // Ranges are permitted independently of authored occupancy; this is
        // how a style can intentionally cover future values in sparse sheets.
        return Ok(());
    };
    let table_index = base.prepared.table(table).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::TargetNotFound,
            "style application table does not exist",
        )
    })?;
    if &table_index.sheet != sheet {
        return Err(operation_error(
            index,
            EditErrorKind::TargetNotFound,
            "style application table is not owned by the target sheet",
        ));
    }
    if let TableRegion::Column { header } = region
        && !table_index.headers.contains_key(header)
    {
        return Err(operation_error(
            index,
            EditErrorKind::TargetNotFound,
            "style application table header does not exist",
        ));
    }
    Ok(())
}

fn format_apply_target(target: &ApplyTarget) -> String {
    match target {
        ApplyTarget::Range(range) => range.to_string(),
        ApplyTarget::Table { table, region } => {
            let region = match region {
                TableRegion::Headers => "#Headers".to_owned(),
                TableRegion::Data => "#Data".to_owned(),
                TableRegion::Column { header } => header.replace(']', "]]"),
            };
            format!("{table}[{region}]")
        }
    }
}

#[derive(Clone, Copy)]
enum MoveOwner<'a> {
    Block(&'a Block),
    Table(&'a Table),
}

impl<'a> MoveOwner<'a> {
    fn block(self) -> &'a Block {
        match self {
            Self::Block(block) => block,
            Self::Table(table) => &table.block,
        }
    }
}

fn plan_move_block(
    source_bytes: &[u8],
    base: &ValidDocument,
    sheet_id: &SheetId,
    source: Range,
    destination: Coordinate,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    validate_move_range(source, destination, index)?;
    let sheet = base
        .workbook
        .sheets
        .iter()
        .find(|candidate| &candidate.id == sheet_id)
        .ok_or_else(|| {
            operation_error(index, EditErrorKind::TargetNotFound, "sheet does not exist")
        })?;
    let owner = find_move_owner(sheet, source, index)?;
    let destination_range = validate_move_destination(sheet, source, destination, index)?;
    if destination_range == source {
        return Ok(());
    }
    plan_move_scoped_targets(
        source_bytes,
        base,
        sheet,
        source,
        destination,
        index,
        patches,
    )?;
    let anchor_span = move_anchor_span(base, owner, index)?;
    push_patch(
        source_bytes,
        patches,
        anchor_span,
        destination.to_string().as_bytes(),
        index,
    )?;
    plan_move_reference_rewrites(
        source_bytes,
        base,
        sheet_id,
        source,
        destination,
        index,
        patches,
    )
}

fn find_move_owner(
    sheet: &marksheet_model::Sheet,
    source: Range,
    index: usize,
) -> Result<MoveOwner<'_>, EditError> {
    let mut exact_owner = None;
    let mut intersects_footprint = false;
    for item in &sheet.items {
        let owner = match item {
            SheetItem::Block(block) => MoveOwner::Block(block),
            SheetItem::Table(table) => MoveOwner::Table(table),
            _ => continue,
        };
        let range = owner
            .block()
            .footprint()
            .and_then(marksheet_model::Footprint::range)
            .map_err(|error| {
                operation_error(
                    index,
                    EditErrorKind::InvalidMove,
                    format!("source footprint cannot be represented: {error}"),
                )
            })?;
        if range == source {
            exact_owner = Some(owner);
        } else if range.overlaps(source) {
            intersects_footprint = true;
        }
    }
    let Some(owner) = exact_owner else {
        let (kind, message) = if intersects_footprint {
            (
                EditErrorKind::PartialFootprint,
                "move source must match one complete block or table footprint",
            )
        } else {
            (
                EditErrorKind::TargetNotFound,
                "move source does not identify a block or table footprint",
            )
        };
        return Err(operation_error(index, kind, message));
    };
    Ok(owner)
}

fn validate_move_destination(
    sheet: &marksheet_model::Sheet,
    source: Range,
    destination: Coordinate,
    index: usize,
) -> Result<Range, EditError> {
    let destination_range = moved_range(source, destination).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::InvalidMove,
            "move destination overflows the coordinate domain",
        )
    })?;
    if destination_range == source {
        return Ok(destination_range);
    }
    for item in &sheet.items {
        let other = match item {
            SheetItem::Block(block) => block,
            SheetItem::Table(table) => &table.block,
            _ => continue,
        };
        let other_range = other
            .footprint()
            .and_then(marksheet_model::Footprint::range)
            .map_err(|error| {
                operation_error(
                    index,
                    EditErrorKind::InvalidMove,
                    format!("destination footprint cannot be checked: {error}"),
                )
            })?;
        if other_range != source && other_range.overlaps(destination_range) {
            return Err(operation_error(
                index,
                EditErrorKind::DestinationOverlap,
                "move destination overlaps another authored footprint",
            ));
        }
    }
    Ok(destination_range)
}

fn move_anchor_span(
    base: &ValidDocument,
    owner: MoveOwner<'_>,
    index: usize,
) -> Result<ByteSpan, EditError> {
    let owner_span = owner
        .block()
        .origin
        .map(|origin| origin.span)
        .ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "move source has no source origin",
            )
        })?;
    let mut locations = base
        .document
        .source_map
        .csv_blocks()
        .iter()
        .filter(|location| location.span == owner_span);
    let location = locations.next().ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "move source does not have a located CSV construct",
        )
    })?;
    if locations.next().is_some() {
        return Err(operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "move source has ambiguous CSV construct locations",
        ));
    }
    location.anchor.ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "move source anchor token could not be located",
        )
    })
}

fn validate_move_range(
    source: Range,
    destination: Coordinate,
    index: usize,
) -> Result<(), EditError> {
    if source.start.column == 0
        || source.start.row == 0
        || source.end.column == 0
        || source.end.row == 0
        || source.start.column > source.end.column
        || source.start.row > source.end.row
        || destination.column == 0
        || destination.row == 0
    {
        return Err(operation_error(
            index,
            EditErrorKind::InvalidMove,
            "move coordinates must be ordered and use one-based axes",
        ));
    }
    Ok(())
}

fn moved_range(source: Range, destination: Coordinate) -> Option<Range> {
    let width = source.width().ok()?;
    let height = source.height().ok()?;
    Some(Range {
        start: destination,
        end: destination
            .offset(width.checked_sub(1)?, height.checked_sub(1)?)
            .ok()?,
    })
}

fn shift_move_range(target: Range, source: Range, destination: Coordinate) -> Option<Range> {
    Some(Range {
        start: shift_move_coordinate(target.start, source.start, destination)?,
        end: shift_move_coordinate(target.end, source.start, destination)?,
    })
}

fn shift_move_coordinate(
    coordinate: Coordinate,
    source: Coordinate,
    destination: Coordinate,
) -> Option<Coordinate> {
    Some(Coordinate {
        column: shift_move_axis(coordinate.column, source.column, destination.column)?,
        row: shift_move_axis(coordinate.row, source.row, destination.row)?,
    })
}

fn shift_move_axis(value: u64, source: u64, destination: u64) -> Option<u64> {
    if destination >= source {
        value.checked_add(destination - source)
    } else {
        value
            .checked_sub(source - destination)
            .filter(|axis| *axis > 0)
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_move_scoped_targets(
    source_bytes: &[u8],
    base: &ValidDocument,
    sheet: &marksheet_model::Sheet,
    source: Range,
    destination: Coordinate,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    for item in &sheet.items {
        let candidate = match item {
            SheetItem::Fill(Fill {
                target: FillTarget::Range(target),
                origin,
                ..
            }) => Some((*target, *origin, ScopedTargetKind::Fill)),
            SheetItem::Apply(marksheet_model::Apply {
                target: ApplyTarget::Range(target),
                origin,
                ..
            }) => Some((*target, *origin, ScopedTargetKind::Apply)),
            _ => None,
        };
        let Some((target, origin, kind)) = candidate else {
            continue;
        };
        let Some(rewritten) = moved_scoped_range(target, source, destination, index)? else {
            continue;
        };
        let origin = origin.map(|origin| origin.span).ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                format!("moved {} target has no source origin", kind.description()),
            )
        })?;
        let span = locate_scoped_target(&base.document.source_map, kind, origin, index)?;
        push_patch(
            source_bytes,
            patches,
            span,
            rewritten.to_string().as_bytes(),
            index,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ScopedTargetKind {
    Fill,
    Apply,
}

impl ScopedTargetKind {
    fn description(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Apply => "style",
        }
    }
}

fn locate_scoped_target(
    source_map: &SourceMap,
    kind: ScopedTargetKind,
    origin: ByteSpan,
    index: usize,
) -> Result<ByteSpan, EditError> {
    let targets = match kind {
        ScopedTargetKind::Fill => source_map
            .fills()
            .iter()
            .filter(|location| location.directive.line == origin)
            .map(|location| location.target)
            .collect::<Vec<_>>(),
        ScopedTargetKind::Apply => source_map
            .applies()
            .iter()
            .filter(|location| location.directive.line == origin)
            .map(|location| location.target)
            .collect::<Vec<_>>(),
    };
    if targets.len() != 1 {
        return Err(operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            format!(
                "moved {} target does not have one source location",
                kind.description()
            ),
        ));
    }
    targets[0].ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            format!(
                "moved {} target token could not be located",
                kind.description()
            ),
        )
    })
}

fn moved_scoped_range(
    target: Range,
    source: Range,
    destination: Coordinate,
    index: usize,
) -> Result<Option<Range>, EditError> {
    let fully_contained = source.contains(target.start) && source.contains(target.end);
    if fully_contained {
        return shift_move_range(target, source, destination)
            .map(Some)
            .ok_or_else(|| {
                operation_error(
                    index,
                    EditErrorKind::InvalidMove,
                    "moving a sheet-scoped target exceeds the coordinate domain",
                )
            });
    }
    if source.overlaps(target) {
        return Err(operation_error(
            index,
            EditErrorKind::PartialFootprint,
            "a coordinate fill or style target partially overlaps the moved footprint",
        ));
    }
    Ok(None)
}

fn move_rewrite(
    moved_sheet: &SheetId,
    source: Range,
    destination: Coordinate,
    formula_sheet: &SheetId,
    formula_origin: Option<Coordinate>,
) -> FormulaRewrite {
    FormulaRewrite::MoveA1 {
        movement: A1Move {
            moved_sheet: moved_sheet.clone(),
            source,
            destination,
            formula_sheet: formula_sheet.clone(),
            formula_origin,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_move_reference_rewrites(
    source_bytes: &[u8],
    base: &ValidDocument,
    moved_sheet: &SheetId,
    source: Range,
    destination: Coordinate,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    for sheet in &base.workbook.sheets {
        for item in &sheet.items {
            let block = match item {
                SheetItem::Block(block) => block,
                SheetItem::Table(table) => &table.block,
                _ => continue,
            };
            plan_move_formulas_in_block(
                source_bytes,
                &base.document.source_map,
                &sheet.id,
                block,
                moved_sheet,
                source,
                destination,
                index,
                patches,
            )?;
        }
    }

    for sheet in &base.workbook.sheets {
        for item in &sheet.items {
            let SheetItem::Fill(fill) = item else {
                continue;
            };
            let fill_span = fill.origin.map(|origin| origin.span).ok_or_else(|| {
                operation_error(
                    index,
                    EditErrorKind::UnlocatableSource,
                    "fill formula has no source origin",
                )
            })?;
            let mut locations = base
                .document
                .source_map
                .fills()
                .iter()
                .filter(|location| location.directive.line == fill_span);
            let location = locations.next().ok_or_else(|| {
                operation_error(
                    index,
                    EditErrorKind::UnlocatableSource,
                    "fill formula does not have a source location",
                )
            })?;
            if locations.next().is_some() {
                return Err(operation_error(
                    index,
                    EditErrorKind::UnlocatableSource,
                    "fill formula has ambiguous source locations",
                ));
            }
            let formula_span = location.formula.ok_or_else(|| {
                operation_error(
                    index,
                    EditErrorKind::UnlocatableSource,
                    "fill formula token could not be located",
                )
            })?;
            let formula_origin = fill_formula_origin(base, fill, index)?;
            let rewrite = move_rewrite(
                moved_sheet,
                source,
                destination,
                &sheet.id,
                Some(formula_origin),
            );
            plan_plain_formula_rewrite(source_bytes, formula_span, &rewrite, index, patches)?;
        }
    }

    for name in &base.workbook.names {
        let formula_sheet = match &name.target {
            marksheet_model::NameTarget::Cell(target) => &target.sheet,
            marksheet_model::NameTarget::Range(target) => &target.sheet,
            marksheet_model::NameTarget::TableColumn { .. } => continue,
        };
        let location = base.document.source_map.name(&name.id).ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "direct A1 name target does not have one unambiguous declaration",
            )
        })?;
        let target_span = location.target.ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "direct A1 name target token could not be located",
            )
        })?;
        let rewrite = move_rewrite(moved_sheet, source, destination, formula_sheet, None);
        plan_name_target_rewrite(source_bytes, target_span, &rewrite, index, patches)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn plan_move_formulas_in_block(
    source_bytes: &[u8],
    source_map: &SourceMap,
    formula_sheet: &SheetId,
    block: &Block,
    moved_sheet: &SheetId,
    source: Range,
    destination: Coordinate,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    for (row_offset, row) in block.cells.iter().enumerate() {
        for (column_offset, cell) in row.iter().enumerate() {
            let Value::Formula(formula) = &cell.value else {
                continue;
            };
            let formula_origin = block
                .anchor
                .offset(
                    u64::try_from(column_offset).map_err(|_| {
                        operation_error(
                            index,
                            EditErrorKind::InvalidMove,
                            "formula column offset exceeds the coordinate domain",
                        )
                    })?,
                    u64::try_from(row_offset).map_err(|_| {
                        operation_error(
                            index,
                            EditErrorKind::InvalidMove,
                            "formula row offset exceeds the coordinate domain",
                        )
                    })?,
                )
                .map_err(|error| {
                    operation_error(
                        index,
                        EditErrorKind::InvalidMove,
                        format!("formula origin cannot be represented: {error}"),
                    )
                })?;
            let location = source_map
                .cell(formula_sheet, formula_origin)
                .ok_or_else(|| {
                    operation_error(
                        index,
                        EditErrorKind::UnlocatableSource,
                        "formula cell does not have one unambiguous source field",
                    )
                })?;
            let rewrite = move_rewrite(
                moved_sheet,
                source,
                destination,
                formula_sheet,
                Some(formula_origin),
            );
            let result = rewrite_formula_text(formula.as_str(), &[rewrite])
                .map_err(|error| rewrite_error(index, &error))?;
            for formula_patch in result.patches {
                let raw_span = csv_formula_span(
                    source_bytes,
                    location.field,
                    formula.as_str(),
                    formula_patch.span,
                )
                .ok_or_else(|| {
                    operation_error(
                        index,
                        EditErrorKind::UnlocatableSource,
                        "moved formula token could not be mapped through its CSV spelling",
                    )
                })?;
                push_patch(
                    source_bytes,
                    patches,
                    raw_span,
                    formula_patch.replacement.as_bytes(),
                    index,
                )?;
            }
        }
    }
    Ok(())
}

fn fill_formula_origin(
    base: &ValidDocument,
    fill: &Fill,
    index: usize,
) -> Result<Coordinate, EditError> {
    match &fill.target {
        FillTarget::Range(range) => Ok(range.start),
        FillTarget::TableColumn { table, header } => base
            .prepared
            .table(table)
            .and_then(|table| table.data_column(header))
            .map(|range| range.start)
            .ok_or_else(|| {
                operation_error(
                    index,
                    EditErrorKind::UnlocatableSource,
                    "table fill formula origin could not be resolved",
                )
            }),
    }
}

fn plan_rename_sheet_id(
    source: &[u8],
    base: &ValidDocument,
    old: &SheetId,
    new: &SheetId,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    if !base.workbook.sheets.iter().any(|sheet| &sheet.id == old) {
        return Err(operation_error(
            index,
            EditErrorKind::TargetNotFound,
            "sheet to rename does not exist",
        ));
    }
    if old == new {
        return Ok(());
    }
    if base.workbook.sheets.iter().any(|sheet| &sheet.id == new) {
        return Err(operation_error(
            index,
            EditErrorKind::IdentifierCollision,
            "new sheet identifier is already in use",
        ));
    }
    let declaration = base.document.source_map.sheet(old).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "sheet does not have one unambiguous declaration",
        )
    })?;
    let id_span = declaration.id.ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "sheet identifier token could not be located",
        )
    })?;
    push_patch(source, patches, id_span, new.as_str().as_bytes(), index)?;
    plan_reference_rewrites(
        source,
        base,
        &FormulaRewrite::RenameSheet {
            from: old.clone(),
            to: new.clone(),
        },
        true,
        index,
        patches,
    )
}

fn plan_rename_name_id(
    source: &[u8],
    base: &ValidDocument,
    old: &NameId,
    new: &NameId,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    if !base.workbook.names.iter().any(|name| &name.id == old) {
        return Err(operation_error(
            index,
            EditErrorKind::TargetNotFound,
            "name to rename does not exist",
        ));
    }
    if old == new {
        return Ok(());
    }
    if matches!(new.as_str(), "true" | "false") {
        return Err(operation_error(
            index,
            EditErrorKind::InvalidIdentifier,
            "boolean literals are reserved and cannot be workbook names",
        ));
    }
    let identifier_in_use = base.workbook.names.iter().any(|name| &name.id == new)
        || base
            .workbook
            .sheets
            .iter()
            .flat_map(|sheet| &sheet.items)
            .any(
                |item| matches!(item, SheetItem::Table(table) if table.id.as_str() == new.as_str()),
            );
    if identifier_in_use {
        return Err(operation_error(
            index,
            EditErrorKind::IdentifierCollision,
            "new name identifier is already in use",
        ));
    }
    let declaration = base.document.source_map.name(old).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "name does not have one unambiguous declaration",
        )
    })?;
    let id_span = declaration.id.ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "name identifier token could not be located",
        )
    })?;
    push_patch(source, patches, id_span, new.as_str().as_bytes(), index)?;
    plan_reference_rewrites(
        source,
        base,
        &FormulaRewrite::RenameName {
            from: old.clone(),
            to: new.clone(),
        },
        false,
        index,
        patches,
    )
}

fn plan_reference_rewrites(
    source: &[u8],
    base: &ValidDocument,
    rewrite: &FormulaRewrite,
    rewrite_name_targets: bool,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    for sheet in &base.workbook.sheets {
        for item in &sheet.items {
            match item {
                SheetItem::Block(block) => plan_block_formula_rewrites(
                    source,
                    &base.document.source_map,
                    &sheet.id,
                    block,
                    rewrite,
                    index,
                    patches,
                )?,
                SheetItem::Table(table) => plan_block_formula_rewrites(
                    source,
                    &base.document.source_map,
                    &sheet.id,
                    &table.block,
                    rewrite,
                    index,
                    patches,
                )?,
                _ => {}
            }
        }
    }

    let semantic_fill_count = base
        .workbook
        .sheets
        .iter()
        .flat_map(|sheet| &sheet.items)
        .filter(|item| matches!(item, SheetItem::Fill(_)))
        .count();
    if base.document.source_map.fills().len() != semantic_fill_count {
        return Err(operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "not every @fill formula has an unambiguous source location",
        ));
    }
    for fill in base.document.source_map.fills() {
        let span = fill.formula.ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "@fill formula token could not be located",
            )
        })?;
        plan_plain_formula_rewrite(source, span, rewrite, index, patches)?;
    }

    if rewrite_name_targets {
        if base.document.source_map.names().len() != base.workbook.names.len() {
            return Err(operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "not every @name target has an unambiguous source location",
            ));
        }
        for name in base.document.source_map.names() {
            let span = name.target.ok_or_else(|| {
                operation_error(
                    index,
                    EditErrorKind::UnlocatableSource,
                    "@name target token could not be located",
                )
            })?;
            plan_name_target_rewrite(source, span, rewrite, index, patches)?;
        }
    }
    Ok(())
}

fn plan_block_formula_rewrites(
    source: &[u8],
    source_map: &SourceMap,
    sheet: &SheetId,
    block: &marksheet_model::Block,
    rewrite: &FormulaRewrite,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    for (row_offset, row) in block.cells.iter().enumerate() {
        for (column_offset, cell) in row.iter().enumerate() {
            let Value::Formula(formula) = &cell.value else {
                continue;
            };
            let coordinate = block
                .anchor
                .offset(
                    u64::try_from(column_offset).map_err(|_| {
                        operation_error(
                            index,
                            EditErrorKind::UnlocatableSource,
                            "cell column offset exceeds the coordinate domain",
                        )
                    })?,
                    u64::try_from(row_offset).map_err(|_| {
                        operation_error(
                            index,
                            EditErrorKind::UnlocatableSource,
                            "cell row offset exceeds the coordinate domain",
                        )
                    })?,
                )
                .map_err(|error| {
                    operation_error(
                        index,
                        EditErrorKind::UnlocatableSource,
                        format!("formula coordinate could not be located: {error}"),
                    )
                })?;
            let location = source_map.cell(sheet, coordinate).ok_or_else(|| {
                operation_error(
                    index,
                    EditErrorKind::UnlocatableSource,
                    "formula cell does not have one unambiguous source field",
                )
            })?;
            let result = rewrite_formula_text(formula.as_str(), std::slice::from_ref(rewrite))
                .map_err(|error| rewrite_error(index, &error))?;
            for formula_patch in result.patches {
                let raw_span =
                    csv_formula_span(source, location.field, formula.as_str(), formula_patch.span)
                        .ok_or_else(|| {
                            operation_error(
                                index,
                                EditErrorKind::UnlocatableSource,
                                "formula token could not be mapped through its CSV field spelling",
                            )
                        })?;
                push_patch(
                    source,
                    patches,
                    raw_span,
                    formula_patch.replacement.as_bytes(),
                    index,
                )?;
            }
        }
    }
    Ok(())
}

fn plan_plain_formula_rewrite(
    source: &[u8],
    span: ByteSpan,
    rewrite: &FormulaRewrite,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    let text = source_text(source, span).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "formula source span is not valid UTF-8",
        )
    })?;
    let result = rewrite_formula_text(text, std::slice::from_ref(rewrite))
        .map_err(|error| rewrite_error(index, &error))?;
    append_relative_formula_patches(source, span.start, result.patches, index, patches)
}

fn plan_name_target_rewrite(
    source: &[u8],
    span: ByteSpan,
    rewrite: &FormulaRewrite,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    let target = source_text(source, span).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "name target span is not valid UTF-8",
        )
    })?;
    let synthetic = format!("={target}");
    let result = rewrite_formula_text(&synthetic, std::slice::from_ref(rewrite))
        .map_err(|error| rewrite_error(index, &error))?;
    for formula_patch in result.patches {
        if formula_patch.span.start == 0 {
            return Err(operation_error(
                index,
                EditErrorKind::ReferenceRewrite,
                "name-target rewrite unexpectedly touched the synthetic formula marker",
            ));
        }
        let relative = ByteSpan {
            start: formula_patch.span.start - 1,
            end: formula_patch.span.end - 1,
        };
        let absolute = offset_span(span.start, relative).ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "name-target rewrite span overflowed the source coordinate space",
            )
        })?;
        push_patch(
            source,
            patches,
            absolute,
            formula_patch.replacement.as_bytes(),
            index,
        )?;
    }
    Ok(())
}

fn append_relative_formula_patches(
    source: &[u8],
    base_offset: u64,
    formula_patches: Vec<FormulaPatch>,
    index: usize,
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    for formula_patch in formula_patches {
        let span = offset_span(base_offset, formula_patch.span).ok_or_else(|| {
            operation_error(
                index,
                EditErrorKind::UnlocatableSource,
                "formula rewrite span overflowed the source coordinate space",
            )
        })?;
        push_patch(
            source,
            patches,
            span,
            formula_patch.replacement.as_bytes(),
            index,
        )?;
    }
    Ok(())
}

fn csv_formula_span(
    source: &[u8],
    field_span: ByteSpan,
    decoded_formula: &str,
    decoded_span: ByteSpan,
) -> Option<ByteSpan> {
    if decoded_span.end > u64::try_from(decoded_formula.len()).ok()? {
        return None;
    }
    let field = source_slice(source, field_span)?;
    if !(field.starts_with(b"\"") && field.ends_with(b"\"") && field.len() >= 2) {
        return offset_span(field_span.start, decoded_span)
            .filter(|span| span.end <= field_span.end);
    }

    let content = &field[1..field.len() - 1];
    let mut raw_boundaries = Vec::with_capacity(decoded_formula.len() + 1);
    raw_boundaries.push(field_span.start.checked_add(1)?);
    let mut raw = 0_usize;
    let mut decoded = Vec::with_capacity(decoded_formula.len());
    while raw < content.len() {
        if content[raw] == b'"' && content.get(raw + 1) == Some(&b'"') {
            decoded.push(b'"');
            raw += 2;
        } else {
            decoded.push(content[raw]);
            raw += 1;
        }
        raw_boundaries.push(field_span.start.checked_add(1 + u64::try_from(raw).ok()?)?);
    }
    if decoded != decoded_formula.as_bytes() {
        return None;
    }
    Some(ByteSpan {
        start: *raw_boundaries.get(usize::try_from(decoded_span.start).ok()?)?,
        end: *raw_boundaries.get(usize::try_from(decoded_span.end).ok()?)?,
    })
}

fn push_patch(
    source: &[u8],
    patches: &mut Vec<SourcePatch>,
    span: ByteSpan,
    replacement: impl AsRef<[u8]>,
    index: usize,
) -> Result<(), EditError> {
    let current = source_slice(source, span).ok_or_else(|| {
        operation_error(
            index,
            EditErrorKind::UnlocatableSource,
            "source location is outside the transaction snapshot",
        )
    })?;
    let replacement = replacement.as_ref();
    if current != replacement {
        patches.push(SourcePatch::new(span, replacement.to_vec()));
    }
    Ok(())
}

fn remove_unchanged_patches(
    source: &[u8],
    patches: &mut Vec<SourcePatch>,
) -> Result<(), EditError> {
    for patch in patches.iter() {
        source_slice(source, patch.span).ok_or_else(|| {
            EditError::new(
                EditErrorKind::PatchPlan,
                None,
                "planned patch is outside the source snapshot",
            )
        })?;
    }
    patches.retain(|patch| source_slice(source, patch.span) != Some(&patch.replacement));
    Ok(())
}

fn normalize_combined_patches(patches: Vec<SourcePatch>) -> Result<Vec<SourcePatch>, EditError> {
    let mut normalized: Vec<SourcePatch> = Vec::with_capacity(patches.len());
    'next_patch: for patch in patches {
        for existing in &mut normalized {
            if patch.span == existing.span {
                if patch.span.is_empty() {
                    existing.replacement.extend_from_slice(&patch.replacement);
                    continue 'next_patch;
                }
                if patch.replacement == existing.replacement {
                    continue 'next_patch;
                }
                return Err(EditError::new(
                    EditErrorKind::UnsupportedOperationCombination,
                    None,
                    "operations plan different replacements for the same source span",
                ));
            }
            if patch.span.overlaps(existing.span) {
                return Err(EditError::new(
                    EditErrorKind::UnsupportedOperationCombination,
                    None,
                    "operations plan overlapping source replacements",
                ));
            }
        }
        normalized.push(patch);
    }
    Ok(normalized)
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

fn encode_json_string(value: &str) -> Vec<u8> {
    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\u{08}' => encoded.push_str("\\b"),
            '\u{0c}' => encoded.push_str("\\f"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            '\u{00}'..='\u{1f}' => {
                use fmt::Write as _;
                write!(encoded, "\\u{:04x}", u32::from(character))
                    .expect("writing to a String is infallible");
            }
            _ => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded.into_bytes()
}

fn table_newline(source: &[u8], body: ByteSpan) -> &'static [u8] {
    let bytes = source_slice(source, body).unwrap_or_default();
    if bytes.ends_with(b"\r\n") {
        return b"\r\n";
    }
    if bytes.ends_with(b"\n") {
        return b"\n";
    }
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\r' {
            return if bytes.get(index + 1) == Some(&b'\n') {
                b"\r\n"
            } else {
                b"\r"
            };
        }
        if *byte == b'\n' {
            return b"\n";
        }
    }
    if source.windows(2).any(|window| window == b"\r\n") {
        b"\r\n"
    } else if source.contains(&b'\r') && !source.contains(&b'\n') {
        b"\r"
    } else {
        b"\n"
    }
}

fn newline_before(source: &[u8], offset: u64) -> Option<&'static [u8]> {
    let offset = usize::try_from(offset).ok()?;
    if source.get(offset.saturating_sub(2)..offset) == Some(b"\r\n") {
        Some(b"\r\n")
    } else if source.get(offset.checked_sub(1)?..offset) == Some(b"\n") {
        Some(b"\n")
    } else {
        None
    }
}

fn newline_at_or_before(source: &[u8], offset: u64) -> Option<&'static [u8]> {
    let offset = usize::try_from(offset).ok()?.min(source.len());
    let newline = source[..offset].iter().rposition(|byte| *byte == b'\n')?;
    if newline > 0 && source[newline - 1] == b'\r' {
        Some(b"\r\n")
    } else {
        Some(b"\n")
    }
}

fn patch_order(patch: &SourcePatch) -> (u64, u8, u64) {
    (
        patch.span.start,
        u8::from(!patch.span.is_empty()),
        patch.span.end,
    )
}

fn source_slice(source: &[u8], span: ByteSpan) -> Option<&[u8]> {
    source.get(usize::try_from(span.start).ok()?..usize::try_from(span.end).ok()?)
}

fn source_text(source: &[u8], span: ByteSpan) -> Option<&str> {
    str::from_utf8(source_slice(source, span)?).ok()
}

fn offset_span(base: u64, relative: ByteSpan) -> Option<ByteSpan> {
    Some(ByteSpan {
        start: base.checked_add(relative.start)?,
        end: base.checked_add(relative.end)?,
    })
}

fn operation_error(index: usize, kind: EditErrorKind, message: impl Into<String>) -> EditError {
    EditError::new(kind, Some(index), message)
}

fn encode_error(index: usize, error: EncodeError) -> EditError {
    operation_error(index, EditErrorKind::InvalidValue, error.to_string())
}

fn rewrite_error(index: usize, error: &FormulaRewriteError) -> EditError {
    operation_error(index, EditErrorKind::ReferenceRewrite, error.to_string())
}

fn patch_error(error: &PatchError) -> EditError {
    EditError::new(EditErrorKind::PatchPlan, None, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::FormulaSource;

    fn coordinate(value: &str) -> Coordinate {
        Coordinate::parse(value).unwrap()
    }

    fn sheet(value: &str) -> SheetId {
        SheetId::parse(value).unwrap()
    }

    fn table(value: &str) -> TableId {
        TableId::parse(value).unwrap()
    }

    fn range(value: &str) -> Range {
        Range::parse(value).unwrap()
    }

    fn name(value: &str) -> NameId {
        NameId::parse(value).unwrap()
    }

    fn execute_one(source: &[u8], operation: EditOperation) -> EditResult {
        EditTransaction::single(operation).execute(source).unwrap()
    }

    fn assert_undo(result: &EditResult, original: &[u8]) {
        assert_eq!(result.inverse.apply(&result.source).unwrap(), original);
        assert_eq!(
            result
                .inverse_transaction
                .execute(&result.source)
                .unwrap()
                .source,
            original
        );
    }

    #[test]
    fn set_cell_matches_source_fixture_and_preserves_extension_bytes() {
        let before = include_bytes!("../../../tests/edit/scalar_csv_quote.before.ms");
        let after = include_bytes!("../../../tests/edit/scalar_csv_quote.after.ms");
        let result = execute_one(
            before,
            EditOperation::SetCell {
                sheet: sheet("data"),
                coordinate: coordinate("B2"),
                value: Value::Text("two, too".to_owned()),
            },
        );

        assert_eq!(result.source, after);
        assert_eq!(result.patches.patches().len(), 1);
        assert_eq!(
            result.patches.patches()[0].span,
            ByteSpan { start: 68, end: 69 }
        );
        assert_undo(&result, before);
    }

    #[test]
    fn set_formula_changes_only_the_selected_field() {
        let before = include_bytes!("../../../tests/edit/formula_field.before.ms");
        let after = include_bytes!("../../../tests/edit/formula_field.after.ms");
        let result = execute_one(
            before,
            EditOperation::SetCell {
                sheet: sheet("data"),
                coordinate: coordinate("B2"),
                value: Value::Formula(FormulaSource::new("=A2*3").unwrap()),
            },
        );

        assert_eq!(result.source, after);
        assert_undo(&result, before);
    }

    #[test]
    fn authored_blank_is_editable_but_virtual_and_absent_cells_are_not() {
        let source = b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\nValue,Derived\n4,\n@end\n@fill B2 =A2*2\n";
        let authored_blank = execute_one(
            source,
            EditOperation::SetCell {
                sheet: sheet("data"),
                coordinate: coordinate("B1"),
                value: Value::Number(9.0),
            },
        );
        assert!(authored_blank.changed());

        let virtual_error = EditTransaction::single(EditOperation::SetCell {
            sheet: sheet("data"),
            coordinate: coordinate("B2"),
            value: Value::Number(9.0),
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(virtual_error.kind, EditErrorKind::VirtualCell);

        let absent_error = EditTransaction::single(EditOperation::SetCell {
            sheet: sheet("data"),
            coordinate: coordinate("C3"),
            value: Value::Number(9.0),
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(absent_error.kind, EditErrorKind::AbsentCell);
    }

    #[test]
    fn semantic_no_op_has_no_patches_and_still_has_an_inverse() {
        let source = include_bytes!("../../../tests/edit/no_op.before.ms");
        let result = execute_one(
            source,
            EditOperation::SetCell {
                sheet: sheet("data"),
                coordinate: coordinate("A1"),
                value: Value::Text("unchanged".to_owned()),
            },
        );

        assert!(!result.changed());
        assert!(result.patches.is_empty());
        assert_undo(&result, source);
    }

    #[test]
    fn append_table_row_matches_fixture_and_preserves_crlf() {
        let before = include_bytes!("../../../tests/edit/table_append.before.ms");
        let after = include_bytes!("../../../tests/edit/table_append.after.ms");
        let result = execute_one(
            before,
            EditOperation::AppendTableRow {
                table: table("things"),
                fields: vec![Value::Text("Gadget".to_owned()), Value::Number(2.0)],
            },
        );
        assert_eq!(result.source, after);
        assert_undo(&result, before);

        let crlf = b"#!marksheet 0.1\r\n@sheet s \"S\"\r\n@table t A1 csv\r\nH\r\nold\r\n@end\r\n";
        let crlf_result = execute_one(
            crlf,
            EditOperation::AppendTableRow {
                table: table("t"),
                fields: vec![Value::Text("new".to_owned())],
            },
        );
        assert!(
            crlf_result
                .source
                .windows(5)
                .any(|window| window == b"new\r\n")
        );
    }

    #[test]
    fn append_refuses_wrong_width() {
        let source = include_bytes!("../../../tests/edit/table_append.before.ms");
        let error = EditTransaction::single(EditOperation::AppendTableRow {
            table: table("things"),
            fields: vec![Value::Text("only one".to_owned())],
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(error.kind, EditErrorKind::WidthMismatch);
    }

    #[test]
    fn sheet_label_rename_only_changes_the_json_token() {
        let before = include_bytes!("../../../tests/edit/rename_label.before.ms");
        let after = include_bytes!("../../../tests/edit/rename_label.after.ms");
        let result = execute_one(
            before,
            EditOperation::RenameSheetLabel {
                sheet: sheet("data"),
                label: "New label".to_owned(),
            },
        );
        assert_eq!(result.source, after);
        assert_undo(&result, before);
    }

    #[test]
    fn apply_existing_style_matches_fixture() {
        let before = include_bytes!("../../../tests/edit/apply_existing_style.before.ms");
        let after = include_bytes!("../../../tests/edit/apply_existing_style.after.ms");
        let result = execute_one(
            before,
            EditOperation::ApplyStyle {
                sheet: sheet("data"),
                target: ApplyTarget::Range(marksheet_model::Range::parse("B2").unwrap()),
                style: StyleId::parse("warning").unwrap(),
            },
        );

        assert_eq!(result.source, after);
        assert_eq!(result.patches.patches().len(), 1);
        assert_eq!(
            result.patches.patches()[0],
            SourcePatch::new(ByteSpan::empty(118), b"@apply B2 warning\n".to_vec())
        );
        assert_undo(&result, before);
    }

    #[test]
    fn apply_style_is_focused_ordered_and_idempotent() {
        let source = b"#!marksheet 0.1\n@style warning bold=true\n@sheet data \"Data\"\n@apply A1 warning\n@column A width=12\n";
        let operation = EditOperation::ApplyStyle {
            sheet: sheet("data"),
            target: ApplyTarget::Range(marksheet_model::Range::parse("B2").unwrap()),
            style: StyleId::parse("warning").unwrap(),
        };
        let result = execute_one(source, operation.clone());
        assert_eq!(
            result.source,
            b"#!marksheet 0.1\n@style warning bold=true\n@sheet data \"Data\"\n@apply A1 warning\n@apply B2 warning\n@column A width=12\n"
        );
        assert_undo(&result, source);

        let no_op = execute_one(&result.source, operation);
        assert!(!no_op.changed());

        let no_final_newline = b"#!marksheet 0.1\n@style warning bold=true\n@sheet data \"Data\"";
        let appended = execute_one(
            no_final_newline,
            EditOperation::ApplyStyle {
                sheet: sheet("data"),
                target: ApplyTarget::Range(range("A1")),
                style: StyleId::parse("warning").unwrap(),
            },
        );
        assert_eq!(
            appended.source,
            b"#!marksheet 0.1\n@style warning bold=true\n@sheet data \"Data\"\n@apply A1 warning\n"
        );
    }

    #[test]
    fn apply_style_reasserts_after_a_later_overlapping_override() {
        let source = b"#!marksheet 0.1\n@style warning fill=\"#fff000\"\n@style clear fill=\"#ffffff\"\n@sheet data \"Data\"\n@apply B2 warning\n@apply A1:C3 clear\n";
        let result = execute_one(
            source,
            EditOperation::ApplyStyle {
                sheet: sheet("data"),
                target: ApplyTarget::Range(range("B2")),
                style: StyleId::parse("warning").unwrap(),
            },
        );
        assert_eq!(
            result.source,
            b"#!marksheet 0.1\n@style warning fill=\"#fff000\"\n@style clear fill=\"#ffffff\"\n@sheet data \"Data\"\n@apply B2 warning\n@apply A1:C3 clear\n@apply B2 warning\n"
        );
        assert_undo(&result, source);
    }

    #[test]
    fn apply_style_validates_style_and_structured_target() {
        let source = b"#!marksheet 0.1\n@style money bold=true\n@sheet data \"Data\"\n@table things A1 csv\nCost]Net\n1\n@end\n@sheet other \"Other\"\n";
        let target = ApplyTarget::Table {
            table: table("things"),
            region: TableRegion::Column {
                header: "Cost]Net".to_owned(),
            },
        };
        let result = execute_one(
            source,
            EditOperation::ApplyStyle {
                sheet: sheet("data"),
                target: target.clone(),
                style: StyleId::parse("money").unwrap(),
            },
        );
        assert!(
            str::from_utf8(&result.source)
                .unwrap()
                .contains("@apply things[Cost]]Net] money\n@sheet other")
        );

        let wrong_sheet = EditTransaction::single(EditOperation::ApplyStyle {
            sheet: sheet("other"),
            target,
            style: StyleId::parse("money").unwrap(),
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(wrong_sheet.kind, EditErrorKind::TargetNotFound);

        let missing_style = EditTransaction::single(EditOperation::ApplyStyle {
            sheet: sheet("data"),
            target: ApplyTarget::Range(marksheet_model::Range::parse("A1").unwrap()),
            style: StyleId::parse("missing").unwrap(),
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(missing_style.kind, EditErrorKind::TargetNotFound);
    }

    #[test]
    fn move_block_matches_fixture_and_partial_move_is_refused() {
        let before = include_bytes!("../../../tests/edit/move_block.before.ms");
        let after = include_bytes!("../../../tests/edit/move_block.after.ms");
        let result = execute_one(
            before,
            EditOperation::MoveBlock {
                sheet: sheet("data"),
                source: range("A1:B2"),
                destination: coordinate("C3"),
            },
        );
        assert_eq!(result.source, after);
        assert_eq!(
            result.patches.patches(),
            [
                SourcePatch::new(ByteSpan { start: 42, end: 44 }, b"C3".to_vec()),
                SourcePatch::new(ByteSpan { start: 54, end: 56 }, b"G3".to_vec()),
            ]
        );
        assert_undo(&result, before);

        let partial = include_bytes!("../../../tests/edit/partial_block_move_refused.before.ms");
        let error = EditTransaction::single(EditOperation::MoveBlock {
            sheet: sheet("data"),
            source: range("A1:A2"),
            destination: coordinate("D1"),
        })
        .execute(partial)
        .unwrap_err();
        assert_eq!(error.kind, EditErrorKind::PartialFootprint);
    }

    #[test]
    fn move_table_updates_names_fills_cross_sheet_and_absolute_references() {
        let source = b"#!marksheet 0.1\n@name selected = data!A1:B2\n@sheet data \"Data\"\n@table things A1 csv\nInput,Output\n1,\n@end\n@fill things[Output] =A2*2\n@sheet report \"Report\"\n@block A1 csv\n=data!$A$1+data!B2\n@end\n";
        let result = execute_one(
            source,
            EditOperation::MoveBlock {
                sheet: sheet("data"),
                source: range("A1:B2"),
                destination: coordinate("D4"),
            },
        );
        let edited = str::from_utf8(&result.source).unwrap();
        assert!(edited.contains("@name selected = data!D4:E5"));
        assert!(edited.contains("@table things D4 csv"));
        assert!(edited.contains("@fill things[Output] =D5*2"));
        assert!(edited.contains("=data!$D$4+data!E5"));
        assert_undo(&result, source);
    }

    #[test]
    fn move_adjusts_formula_context_once_and_preserves_unrelated_source() {
        let source = b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\n1,2\n=A1,=G1\n@end\n@extension vendor@1 \"opaque\"\n A1 and G1\n@end\n";
        let result = execute_one(
            source,
            EditOperation::MoveBlock {
                sheet: sheet("data"),
                source: range("A1:B2"),
                destination: coordinate("D4"),
            },
        );
        let edited = str::from_utf8(&result.source).unwrap();
        assert!(edited.contains("@block D4 csv\n1,2\n=D4,=J4\n@end"));
        assert!(edited.contains("@extension vendor@1 \"opaque\"\n A1 and G1\n@end"));
        assert_undo(&result, source);
    }

    #[test]
    fn move_shifts_contained_fill_and_style_ranges_and_rejects_partial_targets() {
        let source = b"#!marksheet 0.1\n@style warning bold=true\n@sheet data \"Data\"\n@block A1 csv\nInput,Output\n1,\n@end\n@fill B2 =A2*2\n@apply A1:B2 warning\n";
        let result = execute_one(
            source,
            EditOperation::MoveBlock {
                sheet: sheet("data"),
                source: range("A1:B2"),
                destination: coordinate("D4"),
            },
        );
        let edited = str::from_utf8(&result.source).unwrap();
        assert!(edited.contains("@fill E5 =D5*2"));
        assert!(edited.contains("@apply D4:E5 warning"));
        assert_undo(&result, source);

        let partial = b"#!marksheet 0.1\n@style warning bold=true\n@sheet data \"Data\"\n@block A1 csv\n1,2\n3,4\n@end\n@apply B2:C3 warning\n";
        let error = EditTransaction::single(EditOperation::MoveBlock {
            sheet: sheet("data"),
            source: range("A1:B2"),
            destination: coordinate("D4"),
        })
        .execute(partial)
        .unwrap_err();
        assert_eq!(error.kind, EditErrorKind::PartialFootprint);
    }

    #[test]
    fn move_refuses_destination_overlap_and_coordinate_overflow() {
        let source = b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\n1,2\n3,4\n@end\n@block D4 csv\noccupied\n@end\n";
        let overlap = EditTransaction::single(EditOperation::MoveBlock {
            sheet: sheet("data"),
            source: range("A1:B2"),
            destination: coordinate("D4"),
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(overlap.kind, EditErrorKind::DestinationOverlap);

        let overflow = EditTransaction::single(EditOperation::MoveBlock {
            sheet: sheet("data"),
            source: range("A1:B2"),
            destination: Coordinate {
                column: u64::MAX,
                row: 1,
            },
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(overflow.kind, EditErrorKind::InvalidMove);
    }

    #[test]
    fn sheet_id_rename_matches_fixture_atomically() {
        let before = include_bytes!("../../../tests/edit/rename_sheet_id.before.ms");
        let after = include_bytes!("../../../tests/edit/rename_sheet_id.after.ms");
        let result = execute_one(
            before,
            EditOperation::RenameSheetId {
                old: sheet("data"),
                new: sheet("values"),
            },
        );
        assert_eq!(result.source, after);
        assert_eq!(result.patches.patches().len(), 3);
        assert_undo(&result, before);
    }

    #[test]
    fn name_id_rename_matches_fixture_atomically() {
        let before = include_bytes!("../../../tests/edit/rename_name_id.before.ms");
        let after = include_bytes!("../../../tests/edit/rename_name_id.after.ms");
        let result = execute_one(
            before,
            EditOperation::RenameNameId {
                old: name("rate"),
                new: name("tax_rate"),
            },
        );
        assert_eq!(result.source, after);
        assert_eq!(result.patches.patches().len(), 2);
        assert_undo(&result, before);
    }

    #[test]
    fn renames_refs_inside_quoted_formula_without_reserializing_field() {
        let source = b"#!marksheet 0.1\n@name rate = data!A2\n@sheet data \"Data\"\n@block A1 csv\nValue,Formula\n1,\"=IF(rate>0, \"\"rate text\"\", rate)\"\n@end\n";
        let result = execute_one(
            source,
            EditOperation::RenameNameId {
                old: name("rate"),
                new: name("tax_rate"),
            },
        );
        assert_eq!(
            result.source,
            b"#!marksheet 0.1\n@name tax_rate = data!A2\n@sheet data \"Data\"\n@block A1 csv\nValue,Formula\n1,\"=IF(tax_rate>0, \"\"rate text\"\", tax_rate)\"\n@end\n"
        );
        assert_undo(&result, source);
    }

    #[test]
    fn rename_preserves_comments_labels_and_opaque_extensions() {
        let source = b"#!marksheet 0.1\n@name n = data!A1\n@sheet data \"data label\"\n# data!A1\n@block A1 csv\n=data!A1\n@end\n@extension vendor@1 \"data\"\n data!A1\n@end\n";
        let result = execute_one(
            source,
            EditOperation::RenameSheetId {
                old: sheet("data"),
                new: sheet("values"),
            },
        );
        let edited = str::from_utf8(&result.source).unwrap();
        assert!(edited.contains("@name n = values!A1"));
        assert!(edited.contains("@sheet values \"data label\""));
        assert!(edited.contains("# data!A1"));
        assert!(edited.contains("@extension vendor@1 \"data\"\n data!A1\n@end"));
    }

    #[test]
    fn exact_source_precondition_rejects_stale_intent() {
        let source = include_bytes!("../../../tests/edit/no_op.before.ms");
        let conflict = EditTransaction::single(EditOperation::SetCell {
            sheet: sheet("data"),
            coordinate: coordinate("A1"),
            value: Value::Text("changed".to_owned()),
        })
        .expecting_source(b"another source")
        .execute(source)
        .unwrap_err();
        assert_eq!(conflict.kind, EditErrorKind::Conflict);
    }

    #[test]
    fn exact_source_precondition_defeats_same_length_drift_and_forged_hash() {
        let expected = b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\n1\n@end\n";
        let current = b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\n2\n@end\n";
        assert_eq!(expected.len(), current.len());
        let operation = EditOperation::SetCell {
            sheet: sheet("data"),
            coordinate: coordinate("A1"),
            value: Value::Number(3.0),
        };

        let accepted = EditTransaction::single(operation.clone())
            .expecting_source(expected)
            .execute(expected)
            .unwrap();
        assert_eq!(
            accepted.source,
            b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\n3\n@end\n"
        );

        let drift = EditTransaction::single(operation.clone())
            .expecting_source(expected)
            .execute(current)
            .unwrap_err();
        assert_eq!(drift.kind, EditErrorKind::Conflict);

        let mut forged = EditTransaction::single(operation);
        forged.expectations.source = Some(SourceExpectation {
            // A caller/deserializer can forge compact metadata, but cannot
            // bypass the retained authoritative bytes.
            fingerprint: SourceFingerprint::of(current),
            bytes: expected.to_vec(),
        });
        let forged_error = forged.execute(current).unwrap_err();
        assert_eq!(forged_error.kind, EditErrorKind::Conflict);
    }

    #[test]
    fn disjoint_operations_commit_as_one_atomic_patch_set() {
        let source = b"#!marksheet 0.1\n@name rate = data!A2\n@sheet data \"Data\"\n@block A1 csv\nFirst,Second,Formula\n1,2,=rate*2\n@end\n";
        let transaction = EditTransaction {
            operations: vec![
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("A2"),
                    value: Value::Number(10.0),
                },
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("B2"),
                    value: Value::Number(20.0),
                },
            ],
            expectations: EditExpectations::default(),
        };
        let result = transaction.execute(source).unwrap();
        assert_eq!(result.patches.patches().len(), 2);
        assert!(
            str::from_utf8(&result.source)
                .unwrap()
                .contains("10,20,=rate*2")
        );
        assert_undo(&result, source);

        let rename_and_set = EditTransaction {
            operations: vec![
                EditOperation::RenameNameId {
                    old: name("rate"),
                    new: name("tax_rate"),
                },
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("A2"),
                    value: Value::Number(10.0),
                },
            ],
            expectations: EditExpectations::default(),
        }
        .execute(source)
        .unwrap();
        let edited = str::from_utf8(&rename_and_set.source).unwrap();
        assert!(edited.contains("@name tax_rate"));
        assert!(edited.contains("10,2,=tax_rate*2"));
        assert_undo(&rename_and_set, source);
    }

    #[test]
    fn conflicting_or_failing_second_operation_rejects_the_whole_batch() {
        let source =
            b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\nFirst,Second\n1,2\n@end\n";
        let unsupported = EditTransaction {
            operations: vec![
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("A2"),
                    value: Value::Number(10.0),
                },
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("A2"),
                    value: Value::Number(20.0),
                },
            ],
            expectations: EditExpectations::default(),
        }
        .execute(source)
        .unwrap_err();
        assert_eq!(
            unsupported.kind,
            EditErrorKind::UnsupportedOperationCombination
        );

        let failure = EditTransaction {
            operations: vec![
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("A2"),
                    value: Value::Number(10.0),
                },
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("Z99"),
                    value: Value::Number(20.0),
                },
            ],
            expectations: EditExpectations::default(),
        }
        .execute(source)
        .unwrap_err();
        assert_eq!(failure.kind, EditErrorKind::AbsentCell);
        assert_eq!(failure.operation_index, Some(1));
    }

    #[test]
    fn move_cannot_batch_with_a_formula_created_from_the_base_snapshot() {
        let source = b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\n1,2\n3,4\n@end\n";
        let error = EditTransaction {
            operations: vec![
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("B2"),
                    value: Value::Formula(FormulaSource::new("=A1").unwrap()),
                },
                EditOperation::MoveBlock {
                    sheet: sheet("data"),
                    source: range("A1:B2"),
                    destination: coordinate("D4"),
                },
            ],
            expectations: EditExpectations::default(),
        }
        .execute(source)
        .unwrap_err();

        assert_eq!(error.kind, EditErrorKind::UnsupportedOperationCombination);
        assert_eq!(error.operation_index, None);
    }

    #[test]
    fn rename_batches_validate_formulas_created_by_set_and_append() {
        let source = b"#!marksheet 0.1\n@name rate = data!A2\n@sheet data \"Data\"\n@table things A1 csv\nValue,Formula\n1,\n@end\n";
        let rename = EditOperation::RenameNameId {
            old: name("rate"),
            new: name("tax_rate"),
        };
        let stale_set = EditTransaction {
            operations: vec![
                rename.clone(),
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("B2"),
                    value: Value::Formula(FormulaSource::new("=rate*2").unwrap()),
                },
            ],
            expectations: EditExpectations::default(),
        }
        .execute(source)
        .unwrap_err();
        assert_eq!(stale_set.kind, EditErrorKind::InvalidResult);

        let stale_append = EditTransaction {
            operations: vec![
                rename.clone(),
                EditOperation::AppendTableRow {
                    table: table("things"),
                    fields: vec![
                        Value::Number(2.0),
                        Value::Formula(FormulaSource::new("=rate*2").unwrap()),
                    ],
                },
            ],
            expectations: EditExpectations::default(),
        }
        .execute(source)
        .unwrap_err();
        assert_eq!(stale_append.kind, EditErrorKind::InvalidResult);

        let current = EditTransaction {
            operations: vec![
                rename,
                EditOperation::SetCell {
                    sheet: sheet("data"),
                    coordinate: coordinate("B2"),
                    value: Value::Formula(FormulaSource::new("=tax_rate*2").unwrap()),
                },
            ],
            expectations: EditExpectations::default(),
        }
        .execute(source)
        .unwrap();
        let edited = str::from_utf8(&current.source).unwrap();
        assert!(edited.contains("@name tax_rate = data!A2"));
        assert!(edited.contains("1,=tax_rate*2"));
        assert_undo(&current, source);
    }

    #[test]
    fn patched_source_must_pass_formula_validation() {
        let source = b"#!marksheet 0.1\n@sheet data \"Data\"\n@block A1 csv\nValue\n1\n@end\n";
        let error = EditTransaction::single(EditOperation::SetCell {
            sheet: sheet("data"),
            coordinate: coordinate("A2"),
            value: Value::Formula(FormulaSource::new("=missing").unwrap()),
        })
        .execute(source)
        .unwrap_err();
        assert_eq!(error.kind, EditErrorKind::InvalidResult);
        assert!(!error.diagnostics.is_empty());
    }
}
