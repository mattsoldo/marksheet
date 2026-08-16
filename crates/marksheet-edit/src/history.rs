//! In-memory transactional history with conservative semantic rebasing.
//!
//! `PatchSet` is intentionally exact-source-bound. That is ideal for normal
//! undo and redo: a same-length external change is rejected before any bytes
//! are produced. An editor can nevertheless receive a freshly-read document
//! after somebody else changes it. This module makes that hand-off explicit:
//! it compares the semantic targets captured with an [`EditIntent`] and only
//! replans the transaction when those targets still mean the same thing.
//! Comments, whitespace, and disjoint cells therefore do not prevent a
//! rebase; a changed edited cell or renamed declaration does.

use std::fmt;

use marksheet_model::{
    Coordinate, Diagnostic, NameId, NameTarget, SheetId, SheetItem, TableId, Value, Workbook,
};
use marksheet_syntax::parse;

use crate::{
    inverse::{InverseEditError, InverseEditErrorKind, InverseEditResult, InverseTransaction},
    transaction::{EditError, EditOperation, EditResult, EditTransaction, SourceFingerprint},
};

/// A conservative semantic snapshot taken before an edit is executed.
///
/// These snapshots intentionally omit source origins. An unrelated comment
/// may shift byte offsets without changing the meaning of the edit target.
#[derive(Clone, Debug, PartialEq)]
pub enum OperationPrecondition {
    SetCell {
        sheet: SheetId,
        coordinate: Coordinate,
        expected: CellPrecondition,
    },
    AppendTableRow {
        table: TableId,
        expected: Option<TablePrecondition>,
    },
    RenameSheetLabel {
        sheet: SheetId,
        expected_label: Option<String>,
    },
    RenameSheetId {
        old: SheetId,
        new: SheetId,
        expected: Option<SheetPrecondition>,
        new_available: bool,
    },
    RenameNameId {
        old: NameId,
        new: NameId,
        expected: Option<NamePrecondition>,
        new_available: bool,
    },
    /// A transaction operation that the current history layer intentionally
    /// does not rebase. It still executes normally on an exact source base.
    UnsupportedExternalRebase,
}

/// The authored state of one target coordinate.
#[derive(Clone, Debug, PartialEq)]
pub enum CellPrecondition {
    MissingSheet,
    Absent,
    Authored(Value),
}

/// The semantic table content relevant to appending a row.
#[derive(Clone, Debug, PartialEq)]
pub struct TablePrecondition {
    pub anchor: Coordinate,
    pub cells: Vec<Vec<Value>>,
}

/// The sheet declaration fields relevant to renaming its identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetPrecondition {
    pub label: String,
}

/// The named declaration fields relevant to renaming its identifier.
#[derive(Clone, Debug, PartialEq)]
pub struct NamePrecondition {
    pub target: NameTarget,
}

/// A transaction together with the semantic state on which its intent rests.
///
/// Create one with [`EditSession::intent`]. The source fingerprint records
/// where the intent was authored; the operation snapshots enable an explicit
/// rebase to a different, externally supplied source.
#[derive(Clone, Debug, PartialEq)]
pub struct EditIntent {
    transaction: EditTransaction,
    source: SourceFingerprint,
    preconditions: Vec<OperationPrecondition>,
    // The fingerprint is convenient for display and telemetry, but it cannot
    // prove byte identity. Direct execution compares this snapshot exactly.
    base_source: Vec<u8>,
}

impl EditIntent {
    /// The immutable semantic operation list captured with this intent.
    #[must_use]
    pub fn transaction(&self) -> &EditTransaction {
        &self.transaction
    }

    /// The source fingerprint captured for display or telemetry.
    ///
    /// This is not an authority check; execution always compares the retained
    /// byte snapshot exactly.
    #[must_use]
    pub fn fingerprint(&self) -> SourceFingerprint {
        self.source
    }

    /// Read-only semantic guards captured alongside the transaction.
    #[must_use]
    pub fn preconditions(&self) -> &[OperationPrecondition] {
        &self.preconditions
    }
}

/// An in-memory document editor with exact undo/redo and opt-in rebasing.
#[derive(Clone, Debug)]
pub struct EditSession {
    source: Vec<u8>,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

impl EditSession {
    /// Starts an editor session from exact document bytes. No implicit parse or
    /// rewrite is performed; a transaction validates the document before it
    /// creates patches.
    #[must_use]
    pub fn new(source: impl Into<Vec<u8>>) -> Self {
        Self {
            source: source.into(),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Returns the current exact document bytes.
    #[must_use]
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    /// Returns the exact identity of the current document bytes.
    #[must_use]
    pub fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint::of(&self.source)
    }

    /// Returns how many committed edits can be undone.
    #[must_use]
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Returns how many undone edits can be redone.
    #[must_use]
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Replaces the in-memory bytes after an external read.
    ///
    /// History is deliberately retained rather than silently discarded. Its
    /// exact patch preconditions will reject unsafe undo/redo, and callers can
    /// use [`Self::rebase_and_execute`] to create a new edit on the external
    /// baseline. This makes an external change visible instead of losing an
    /// unsaved history entry behind the caller's back.
    pub fn replace_source(&mut self, source: impl Into<Vec<u8>>) -> bool {
        let source = source.into();
        let changed = source != self.source;
        self.source = source;
        changed
    }

    /// Discards recorded undo and redo entries without changing current bytes.
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Captures a transaction's semantic targets against the current source.
    ///
    /// An intent can be saved while a UI awaits an external refresh, then
    /// passed to [`Self::rebase_and_execute`] with that refreshed source.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryErrorKind::InvalidSource`] when current bytes do not
    /// form a complete Marksheet workbook.
    pub fn intent(&self, transaction: EditTransaction) -> Result<EditIntent, HistoryError> {
        let workbook = parse_workbook(&self.source)?;
        Ok(EditIntent {
            source: SourceFingerprint::of(&self.source),
            base_source: self.source.clone(),
            preconditions: transaction
                .operations
                .iter()
                .map(|operation| capture_precondition(&workbook, operation))
                .collect(),
            transaction,
        })
    }

    /// Executes a transaction against this session's exact current bytes.
    ///
    /// # Errors
    ///
    /// Returns a transaction validation error, or a conflict if the captured
    /// intent cannot be applied to the current exact bytes.
    pub fn execute(&mut self, transaction: EditTransaction) -> Result<EditResult, HistoryError> {
        let intent = self.intent(transaction)?;
        self.execute_intent(intent)
    }

    /// Executes an intent only when the current bytes still match its base.
    ///
    /// Use [`Self::rebase_and_execute`] rather than this method after an
    /// external change so the semantic preconditions are checked explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryErrorKind::Conflict`] unless the intent's retained
    /// byte snapshot exactly equals the current source.
    pub fn execute_intent(&mut self, intent: EditIntent) -> Result<EditResult, HistoryError> {
        validate_intent(&intent)?;
        if intent.base_source != self.source {
            return Err(HistoryError::conflict(
                "the document changed after this edit intent was created; rebase it explicitly",
            ));
        }
        self.commit(intent)
    }

    /// Replans an intent against externally supplied bytes when its semantic
    /// targets have not changed.
    ///
    /// A transaction with an explicit `expecting_source` precondition is
    /// intentionally strict: rebasing it to different bytes is a conflict.
    /// Successful rebases establish a new document baseline, so earlier undo
    /// entries are discarded; only the newly committed edit is undoable.
    ///
    /// # Errors
    ///
    /// Returns a conflict when an edited semantic target changed externally,
    /// or an invalid-source/edit error without changing this session.
    pub fn rebase_and_execute(
        &mut self,
        external_source: &[u8],
        intent: EditIntent,
    ) -> Result<EditResult, HistoryError> {
        if external_source == self.source && intent.base_source == self.source {
            return self.execute_intent(intent);
        }
        validate_intent(&intent)?;
        if intent.transaction.expectations.source.is_some() {
            return Err(HistoryError::conflict(
                "a source-fingerprint precondition cannot be semantically rebased",
            ));
        }
        let external_workbook = parse_workbook(external_source)?;
        verify_preconditions(&external_workbook, &intent.preconditions)?;

        // The transaction may now be planned on the external formatting and
        // spans. Its semantic operation list stays unchanged.
        let rebased = EditIntent {
            source: SourceFingerprint::of(external_source),
            base_source: external_source.to_vec(),
            preconditions: intent
                .transaction
                .operations
                .iter()
                .map(|operation| capture_precondition(&external_workbook, operation))
                .collect(),
            transaction: intent.transaction,
        };
        // Plan and validate before changing the session. A bad formula or an
        // otherwise invalid result must leave both its current bytes and
        // history untouched, just like any other failed transaction.
        let result = rebased
            .transaction
            .execute(external_source)
            .map_err(HistoryError::from_edit)?;
        self.clear_history();
        Ok(self.record(rebased, result))
    }

    /// Convenience form that captures intent from the current source and then
    /// attempts to apply it to externally supplied bytes.
    ///
    /// This method is only appropriate before the session adopts an external
    /// source. After [`Self::replace_source`], the original semantic base is
    /// no longer available here: callers must retain an [`EditIntent`] made
    /// beforehand and pass it to [`Self::rebase_and_execute`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::intent`] and
    /// [`Self::rebase_and_execute`].
    pub fn rebase_transaction(
        &mut self,
        external_source: &[u8],
        transaction: EditTransaction,
    ) -> Result<EditResult, HistoryError> {
        let intent = self.intent(transaction)?;
        self.rebase_and_execute(external_source, intent)
    }

    /// Restores the immediately preceding edit exactly and returns its
    /// validated semantic result.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryErrorKind::NothingToUndo`] for an empty history, or
    /// [`HistoryErrorKind::PatchPrecondition`] when external bytes changed.
    pub fn undo_edit(&mut self) -> Result<InverseEditResult, HistoryError> {
        let mut entry = self
            .undo
            .pop()
            .ok_or_else(|| HistoryError::new(HistoryErrorKind::NothingToUndo, "nothing to undo"))?;
        let restored = match entry.inverse.execute(&self.source) {
            Ok(restored) => restored,
            Err(error) => {
                self.undo.push(entry);
                return Err(history_inverse_error(error, &self.source));
            }
        };
        self.source.clone_from(&restored.source);
        // Keep the inverse generated from the validated application. This
        // binds redo to the exact restored source rather than relying on an
        // equivalent patch captured before a later history transition.
        entry.forward = restored.inverse.clone();
        self.redo.push(entry);
        Ok(restored)
    }

    /// Restores the previous source bytes for callers that only need the
    /// compatibility byte-oriented undo result.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::undo_edit`].
    pub fn undo(&mut self) -> Result<Vec<u8>, HistoryError> {
        self.undo_edit().map(|result| result.source)
    }

    /// Reapplies the most recently undone edit exactly and returns its
    /// validated semantic result.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryErrorKind::NothingToRedo`] for an empty redo stack, or
    /// [`HistoryErrorKind::PatchPrecondition`] when external bytes changed.
    pub fn redo_edit(&mut self) -> Result<InverseEditResult, HistoryError> {
        let mut entry = self
            .redo
            .pop()
            .ok_or_else(|| HistoryError::new(HistoryErrorKind::NothingToRedo, "nothing to redo"))?;
        let reapplied = match entry.forward.execute(&self.source) {
            Ok(reapplied) => reapplied,
            Err(error) => {
                self.redo.push(entry);
                return Err(history_inverse_error(error, &self.source));
            }
        };
        self.source.clone_from(&reapplied.source);
        entry.inverse = reapplied.inverse.clone();
        self.undo.push(entry);
        Ok(reapplied)
    }

    /// Returns the previous byte-oriented redo result for compatibility.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::redo_edit`].
    pub fn redo(&mut self) -> Result<Vec<u8>, HistoryError> {
        self.redo_edit().map(|result| result.source)
    }

    fn commit(&mut self, intent: EditIntent) -> Result<EditResult, HistoryError> {
        let result = intent
            .transaction
            .execute(&self.source)
            .map_err(HistoryError::from_edit)?;
        // `EditResult::source` was constructed by applying the same exact
        // PatchSet that we retain for redo. Preserve it rather than rendering
        // a second time, so no additional failure path can split state.
        Ok(self.record(intent, result))
    }

    fn record(&mut self, intent: EditIntent, result: EditResult) -> EditResult {
        self.source.clone_from(&result.source);
        self.undo.push(HistoryEntry {
            transaction: intent.transaction,
            preconditions: intent.preconditions,
            forward: InverseTransaction::from_patch_set(result.patches.clone()),
            inverse: result.inverse_transaction.clone(),
        });
        // A successful new edit, including a semantic rebase, invalidates the
        // redo branch by the standard editor history rule.
        self.redo.clear();
        result
    }
}

#[derive(Clone, Debug)]
struct HistoryEntry {
    #[allow(dead_code)]
    transaction: EditTransaction,
    #[allow(dead_code)]
    preconditions: Vec<OperationPrecondition>,
    forward: InverseTransaction,
    inverse: InverseTransaction,
}

/// Stable categories for edit-history failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryErrorKind {
    Conflict,
    InvalidSource,
    Edit,
    PatchPrecondition,
    NothingToUndo,
    NothingToRedo,
}

/// An edit-history failure. No source bytes are modified on this path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryError {
    pub kind: HistoryErrorKind,
    pub message: String,
    details: Box<HistoryErrorDetails>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct HistoryErrorDetails {
    diagnostics: Vec<Diagnostic>,
    edit: Option<Box<EditError>>,
}

impl HistoryError {
    fn new(kind: HistoryErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Box::default(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(HistoryErrorKind::Conflict, message)
    }

    fn from_edit(error: EditError) -> Self {
        Self {
            kind: HistoryErrorKind::Edit,
            message: error.to_string(),
            details: Box::new(HistoryErrorDetails {
                diagnostics: error.diagnostics.clone(),
                edit: Some(Box::new(error)),
            }),
        }
    }

    /// Diagnostics retained from parsing or transaction validation.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.details.diagnostics
    }

    /// Underlying edit failure, when a transaction planner produced one.
    #[must_use]
    pub fn edit(&self) -> Option<&EditError> {
        self.details.edit.as_deref()
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HistoryError {}

fn parse_workbook(source: &[u8]) -> Result<Workbook, HistoryError> {
    let document = parse(source);
    if document.has_errors() {
        return Err(HistoryError {
            kind: HistoryErrorKind::InvalidSource,
            message: "history source is not a valid Marksheet document".to_owned(),
            details: Box::new(HistoryErrorDetails {
                diagnostics: document.diagnostics,
                edit: None,
            }),
        });
    }
    document.workbook.ok_or_else(|| HistoryError {
        kind: HistoryErrorKind::InvalidSource,
        message: "history source did not produce a complete workbook".to_owned(),
        details: Box::new(HistoryErrorDetails {
            diagnostics: document.diagnostics,
            edit: None,
        }),
    })
}

/// Ensures an intent has not been internally corrupted between capture and
/// execution. Public callers cannot mutate these private fields, but this
/// check keeps the safety property local even if a future crate-internal API
/// reconstructs an intent from storage.
fn validate_intent(intent: &EditIntent) -> Result<(), HistoryError> {
    if intent.source != SourceFingerprint::of(&intent.base_source) {
        return Err(HistoryError::conflict(
            "the edit intent fingerprint does not match its retained source snapshot",
        ));
    }
    let workbook = parse_workbook(&intent.base_source)?;
    let expected = intent
        .transaction
        .operations
        .iter()
        .map(|operation| capture_precondition(&workbook, operation))
        .collect::<Vec<_>>();
    if expected != intent.preconditions {
        return Err(HistoryError::conflict(
            "the edit intent operations no longer match their captured semantic preconditions",
        ));
    }
    Ok(())
}

fn capture_precondition(workbook: &Workbook, operation: &EditOperation) -> OperationPrecondition {
    match operation {
        EditOperation::SetCell {
            sheet, coordinate, ..
        } => OperationPrecondition::SetCell {
            sheet: sheet.clone(),
            coordinate: *coordinate,
            expected: cell_precondition(workbook, sheet, *coordinate),
        },
        EditOperation::AppendTableRow { table, .. } => OperationPrecondition::AppendTableRow {
            table: table.clone(),
            expected: table_precondition(workbook, table),
        },
        EditOperation::RenameSheetLabel { sheet, .. } => OperationPrecondition::RenameSheetLabel {
            sheet: sheet.clone(),
            expected_label: workbook
                .sheets
                .iter()
                .find(|candidate| candidate.id == *sheet)
                .map(|candidate| candidate.label.clone()),
        },
        EditOperation::RenameSheetId { old, new } => OperationPrecondition::RenameSheetId {
            old: old.clone(),
            new: new.clone(),
            expected: workbook
                .sheets
                .iter()
                .find(|candidate| candidate.id == *old)
                .map(|candidate| SheetPrecondition {
                    label: candidate.label.clone(),
                }),
            new_available: !workbook.sheets.iter().any(|candidate| candidate.id == *new),
        },
        EditOperation::RenameNameId { old, new } => OperationPrecondition::RenameNameId {
            old: old.clone(),
            new: new.clone(),
            expected: workbook
                .names
                .iter()
                .find(|candidate| candidate.id == *old)
                .map(|candidate| NamePrecondition {
                    target: candidate.target.clone(),
                }),
            new_available: name_identifier_available(workbook, new),
        },
        EditOperation::ApplyStyle { .. } | EditOperation::MoveBlock { .. } => {
            OperationPrecondition::UnsupportedExternalRebase
        }
    }
}

fn verify_preconditions(
    workbook: &Workbook,
    preconditions: &[OperationPrecondition],
) -> Result<(), HistoryError> {
    for precondition in preconditions {
        let matches = match precondition {
            OperationPrecondition::SetCell {
                sheet,
                coordinate,
                expected,
            } => *expected == cell_precondition(workbook, sheet, *coordinate),
            OperationPrecondition::AppendTableRow { table, expected } => {
                *expected == table_precondition(workbook, table)
            }
            OperationPrecondition::RenameSheetLabel {
                sheet,
                expected_label,
            } => {
                *expected_label
                    == workbook
                        .sheets
                        .iter()
                        .find(|candidate| candidate.id == *sheet)
                        .map(|candidate| candidate.label.clone())
            }
            OperationPrecondition::RenameSheetId {
                old,
                new,
                expected,
                new_available,
            } => {
                *expected
                    == workbook
                        .sheets
                        .iter()
                        .find(|candidate| candidate.id == *old)
                        .map(|candidate| SheetPrecondition {
                            label: candidate.label.clone(),
                        })
                    && *new_available
                        != workbook.sheets.iter().any(|candidate| candidate.id == *new)
            }
            OperationPrecondition::RenameNameId {
                old,
                new,
                expected,
                new_available,
            } => {
                *expected
                    == workbook
                        .names
                        .iter()
                        .find(|candidate| candidate.id == *old)
                        .map(|candidate| NamePrecondition {
                            target: candidate.target.clone(),
                        })
                    && *new_available == name_identifier_available(workbook, new)
            }
            OperationPrecondition::UnsupportedExternalRebase => false,
        };
        if !matches {
            return Err(HistoryError::conflict(
                "an external change modified a transaction target or precondition",
            ));
        }
    }
    Ok(())
}

fn cell_precondition(
    workbook: &Workbook,
    sheet: &SheetId,
    coordinate: Coordinate,
) -> CellPrecondition {
    let Some(sheet) = workbook
        .sheets
        .iter()
        .find(|candidate| candidate.id == *sheet)
    else {
        return CellPrecondition::MissingSheet;
    };
    for item in &sheet.items {
        let block = match item {
            SheetItem::Block(block) => block,
            SheetItem::Table(table) => &table.block,
            _ => continue,
        };
        let Some(column_offset) = coordinate.column.checked_sub(block.anchor.column) else {
            continue;
        };
        let Some(row_offset) = coordinate.row.checked_sub(block.anchor.row) else {
            continue;
        };
        let (Ok(column), Ok(row)) = (usize::try_from(column_offset), usize::try_from(row_offset))
        else {
            continue;
        };
        if let Some(cell) = block.cells.get(row).and_then(|cells| cells.get(column)) {
            return CellPrecondition::Authored(cell.value.clone());
        }
    }
    CellPrecondition::Absent
}

fn table_precondition(workbook: &Workbook, table: &TableId) -> Option<TablePrecondition> {
    workbook
        .sheets
        .iter()
        .flat_map(|sheet| &sheet.items)
        .find_map(|item| match item {
            SheetItem::Table(candidate) if candidate.id == *table => Some(TablePrecondition {
                anchor: candidate.block.anchor,
                cells: candidate
                    .block
                    .cells
                    .iter()
                    .map(|row| row.iter().map(|cell| cell.value.clone()).collect())
                    .collect(),
            }),
            _ => None,
        })
}

fn name_identifier_available(workbook: &Workbook, new: &NameId) -> bool {
    !workbook.names.iter().any(|candidate| candidate.id == *new)
        && !workbook.sheets.iter().flat_map(|sheet| &sheet.items).any(
            |item| matches!(item, SheetItem::Table(table) if table.id.as_str() == new.as_str()),
        )
}

/// Diagnoses changed bytes before reporting an exact-patch precondition
/// failure. Parsing is read-only and happens before the history entry moves,
/// so a malformed external replacement cannot conceal its diagnostics or
/// alter undo/redo state.
fn history_inverse_error(error: InverseEditError, current_source: &[u8]) -> HistoryError {
    let kind = match error.kind {
        InverseEditErrorKind::PatchPrecondition => HistoryErrorKind::PatchPrecondition,
        InverseEditErrorKind::Patch | InverseEditErrorKind::InvalidResult => HistoryErrorKind::Edit,
    };
    let diagnostics = error.diagnostics().to_vec();
    let mut result = HistoryError {
        kind,
        message: error.message,
        details: Box::new(HistoryErrorDetails {
            diagnostics,
            edit: None,
        }),
    };
    if error.kind != InverseEditErrorKind::PatchPrecondition {
        return result;
    }
    if let Err(validation) = parse_workbook(current_source) {
        result.message = format!(
            "{}; changed source also failed validation: {}",
            result.message, validation.message
        );
        result.details = validation.details;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(value: &str) -> SheetId {
        SheetId::parse(value).unwrap()
    }

    fn coordinate(value: &str) -> Coordinate {
        Coordinate::parse(value).unwrap()
    }

    fn sample() -> Vec<u8> {
        b"#!marksheet 0.1\n# stable comment\n@sheet data \"Data\"\n@block A1 csv\nFirst,Second\n1,2\n@end\n".to_vec()
    }

    fn set_a2(value: f64) -> EditTransaction {
        EditTransaction::single(EditOperation::SetCell {
            sheet: sheet("data"),
            coordinate: coordinate("A2"),
            value: Value::Number(value),
        })
    }

    fn replace_bytes(source: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let start = source
            .windows(from.len())
            .position(|window| window == from)
            .expect("test fixture contains replacement bytes");
        let mut result = source.to_vec();
        result.splice(start..start + from.len(), to.iter().copied());
        result
    }

    #[test]
    fn undo_and_redo_restore_exact_original_bytes() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let result = session.execute(set_a2(10.0)).unwrap();
        assert_eq!(session.source(), result.source);

        assert_eq!(session.undo().unwrap(), source);
        assert_eq!(session.redo().unwrap(), result.source);
        assert_eq!(session.undo_len(), 1);
        assert_eq!(session.redo_len(), 0);
    }

    #[test]
    fn rich_undo_and_redo_results_are_validated_and_reversible() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let edited = session.execute(set_a2(10.0)).unwrap().source;

        let undone = session.undo_edit().unwrap();
        assert_eq!(undone.source, source);
        assert_eq!(undone.workbook.sheets.len(), 1);
        assert_eq!(
            undone.inverse.execute(&undone.source).unwrap().source,
            edited
        );

        let redone = session.redo_edit().unwrap();
        assert_eq!(redone.source, edited);
        assert_eq!(redone.workbook.sheets.len(), 1);
        assert_eq!(
            redone.inverse.execute(&redone.source).unwrap().source,
            source
        );
    }

    #[test]
    fn same_length_drift_rejects_undo_without_moving_history() {
        let mut session = EditSession::new(sample());
        session.execute(set_a2(10.0)).unwrap();
        let mut drift = session.source().to_vec();
        let offset = drift.windows(2).position(|window| window == b"10").unwrap();
        drift[offset] = b'9';
        assert!(session.replace_source(drift));

        let error = session.undo().unwrap_err();
        assert_eq!(error.kind, HistoryErrorKind::PatchPrecondition);
        assert_eq!(session.undo_len(), 1);
        assert_eq!(session.redo_len(), 0);
    }

    #[test]
    fn direct_execution_compares_exact_intent_bytes_not_only_fingerprint() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let mut intent = session.intent(set_a2(10.0)).unwrap();
        let external = replace_bytes(&source, b"1,2", b"9,2");
        session.replace_source(external.clone());
        // Deliberately forge the public metadata to the new source. The
        // private base snapshot must still reject this stale intent.
        intent.source = SourceFingerprint::of(&external);

        assert_eq!(
            session.execute_intent(intent).unwrap_err().kind,
            HistoryErrorKind::Conflict
        );
        assert_eq!(session.source(), external);
    }

    #[test]
    fn internally_retargeted_intent_is_rejected_before_rebase() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let mut intent = session.intent(set_a2(10.0)).unwrap();
        // This mutation is only possible inside this module's test. The
        // public API exposes `transaction()` by shared reference only.
        intent.transaction.operations[0] = EditOperation::SetCell {
            sheet: sheet("data"),
            coordinate: coordinate("B2"),
            value: Value::Number(20.0),
        };
        let external = replace_bytes(&source, b"1,2", b"1,9");

        assert_eq!(
            session
                .rebase_and_execute(&external, intent)
                .unwrap_err()
                .kind,
            HistoryErrorKind::Conflict
        );
        assert_eq!(session.source(), source);
    }

    #[test]
    fn unrelated_comment_rebases_and_preserves_external_bytes() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let intent = session.intent(set_a2(10.0)).unwrap();
        let external = replace_bytes(&source, b"# stable comment", b"# external comment");

        let result = session.rebase_and_execute(&external, intent).unwrap();
        let edited = String::from_utf8(result.source).unwrap();
        assert!(edited.contains("# external comment"));
        assert!(edited.contains("10,2"));
        assert_eq!(session.undo().unwrap(), external);
    }

    #[test]
    fn adopted_external_source_can_rebase_a_pre_captured_intent() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let intent = session.intent(set_a2(10.0)).unwrap();
        let external = replace_bytes(&source, b"# stable comment", b"# external comment");
        session.replace_source(external.clone());
        let current = session.source().to_vec();

        let result = session.rebase_and_execute(&current, intent).unwrap();
        let edited = String::from_utf8(result.source).unwrap();
        assert!(edited.contains("# external comment"));
        assert!(edited.contains("10,2"));
    }

    #[test]
    fn same_cell_external_change_conflicts_without_adopting_bytes() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let intent = session.intent(set_a2(10.0)).unwrap();
        let external = replace_bytes(&source, b"1,2", b"9,2");

        let error = session.rebase_and_execute(&external, intent).unwrap_err();
        assert_eq!(error.kind, HistoryErrorKind::Conflict);
        assert_eq!(session.source(), source);
        assert_eq!(session.undo_len(), 0);
    }

    #[test]
    fn successful_new_edit_invalidates_redo() {
        let mut session = EditSession::new(sample());
        session.execute(set_a2(10.0)).unwrap();
        session.undo().unwrap();
        assert_eq!(session.redo_len(), 1);

        session.execute(set_a2(20.0)).unwrap();
        assert_eq!(session.redo_len(), 0);
        assert_eq!(
            session.redo().unwrap_err().kind,
            HistoryErrorKind::NothingToRedo
        );
    }

    #[test]
    fn strict_source_expectation_does_not_rebase() {
        let source = sample();
        let mut session = EditSession::new(source.clone());
        let transaction = set_a2(10.0).expecting_source(&source);
        let intent = session.intent(transaction).unwrap();
        let external = replace_bytes(&source, b"# stable comment", b"# external comment");
        assert_eq!(
            session
                .rebase_and_execute(&external, intent)
                .unwrap_err()
                .kind,
            HistoryErrorKind::Conflict
        );
    }

    #[test]
    fn invalid_drift_is_reparsed_before_undo_conflict() {
        let mut session = EditSession::new(sample());
        session.execute(set_a2(10.0)).unwrap();
        let invalid = b"not a marksheet document\n".to_vec();
        session.replace_source(invalid.clone());

        let error = session.undo().unwrap_err();
        assert_eq!(error.kind, HistoryErrorKind::PatchPrecondition);
        assert!(!error.diagnostics().is_empty());
        assert_eq!(session.source(), invalid);
        assert_eq!(session.undo_len(), 1);
    }
}
