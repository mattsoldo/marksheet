//! Validated execution of source-bound inverse transactions.
//!
//! A raw [`PatchSet`] is sufficient to restore bytes, but an editor also needs
//! the resulting semantic workbook and formula diagnostics before it may
//! publish an undo or redo result. This module keeps those guarantees on the
//! inverse path instead of treating undo as an unvalidated byte operation.

use std::{fmt, sync::Arc};

use marksheet_calc::prepare::{CompileLimits, PrepareLimits, PreparedWorkbook, compile_formulas};
use marksheet_model::{Diagnostic, Workbook};
use marksheet_syntax::{ParseOptions, parse_with_options};

use crate::patch::{PatchError, PatchSet, SourcePatch};

/// An exact-source-bound transaction used for undo or redo.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InverseTransaction {
    patches: PatchSet,
}

impl InverseTransaction {
    /// Wraps a source-bound patch set as a validated inverse transaction.
    #[must_use]
    pub fn from_patch_set(patches: PatchSet) -> Self {
        Self { patches }
    }

    /// Ordered byte patches that this transaction applies.
    #[must_use]
    pub fn patches(&self) -> &[SourcePatch] {
        self.patches.patches()
    }

    /// Returns whether the transaction changes no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    /// The shared snapshot this transaction is bound to.
    pub(crate) fn shared_base(&self) -> &Arc<Vec<u8>> {
        self.patches.shared_base()
    }

    /// Applies the transaction, then validates the resulting workbook and all
    /// formulas before returning an editor-visible result.
    ///
    /// # Errors
    ///
    /// Returns [`InverseEditErrorKind::PatchPrecondition`] when `source` is
    /// not the exact snapshot bound to these patches. Returns
    /// [`InverseEditErrorKind::InvalidResult`] without exposing a result when
    /// the patched bytes do not parse, prepare, or compile cleanly.
    pub fn execute(&self, source: &[u8]) -> Result<InverseEditResult, InverseEditError> {
        self.execute_with_parse_options(source, &ParseOptions::default())
    }

    /// Applies and validates this inverse with host extension capabilities.
    ///
    /// # Errors
    ///
    /// Returns the same atomic failures as [`Self::execute`].
    pub fn execute_with_parse_options(
        &self,
        source: &[u8],
        options: &ParseOptions,
    ) -> Result<InverseEditResult, InverseEditError> {
        let (edited, reverse) = self
            .patches
            .apply_with_inverse(source)
            .map_err(|error| InverseEditError::from_patch(&error))?;
        let (workbook, diagnostics) = validate_result(&edited, options)?;
        Ok(InverseEditResult {
            patches: self.patches.patches().to_vec(),
            source: edited,
            workbook,
            diagnostics,
            inverse: Self::from_patch_set(reverse),
        })
    }
}

/// A fully validated undo or redo result.
#[derive(Clone, Debug)]
pub struct InverseEditResult {
    /// Ordered patches applied to the source passed to
    /// [`InverseTransaction::execute`].
    pub patches: Vec<SourcePatch>,
    /// Exact resulting source bytes.
    pub source: Vec<u8>,
    /// Validated semantic workbook produced by `source`.
    pub workbook: Workbook,
    /// Parser diagnostics retained for the validated source.
    pub diagnostics: Vec<Diagnostic>,
    /// Source-bound reverse transaction, suitable for the next redo or undo.
    pub inverse: InverseTransaction,
}

/// Stable categories for inverse transaction failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InverseEditErrorKind {
    PatchPrecondition,
    Patch,
    InvalidResult,
}

/// An inverse transaction failure. No partial result is exposed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InverseEditError {
    pub kind: InverseEditErrorKind,
    pub message: String,
    details: Box<InverseEditErrorDetails>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct InverseEditErrorDetails {
    diagnostics: Vec<Diagnostic>,
}

impl InverseEditError {
    fn from_patch(error: &PatchError) -> Self {
        let kind = match error {
            PatchError::BaseMismatch { .. } => InverseEditErrorKind::PatchPrecondition,
            _ => InverseEditErrorKind::Patch,
        };
        Self {
            kind,
            message: error.to_string(),
            details: Box::default(),
        }
    }

    fn invalid_result(message: impl Into<String>, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            kind: InverseEditErrorKind::InvalidResult,
            message: message.into(),
            details: Box::new(InverseEditErrorDetails { diagnostics }),
        }
    }

    /// Diagnostics retained while validating the resulting source.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.details.diagnostics
    }
}

impl fmt::Display for InverseEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for InverseEditError {}

fn validate_result(
    source: &[u8],
    options: &ParseOptions,
) -> Result<(Workbook, Vec<Diagnostic>), InverseEditError> {
    let document = parse_with_options(source, options);
    if document.has_errors() {
        return Err(InverseEditError::invalid_result(
            "inverse transaction result is not a valid Marksheet document",
            document.diagnostics,
        ));
    }
    let workbook = document.workbook.ok_or_else(|| {
        InverseEditError::invalid_result(
            "inverse transaction result did not produce a complete workbook",
            document.diagnostics.clone(),
        )
    })?;
    let prepared =
        PreparedWorkbook::build(&workbook, PrepareLimits::default()).map_err(|error| {
            InverseEditError::invalid_result(
                format!("inverse transaction workbook preparation failed: {error}"),
                document.diagnostics.clone(),
            )
        })?;
    let program =
        compile_formulas(&workbook, &prepared, &CompileLimits::default()).map_err(|error| {
            InverseEditError::invalid_result(error.to_string(), document.diagnostics.clone())
        })?;
    if !program.issues.is_empty() {
        let mut diagnostics = document.diagnostics.clone();
        diagnostics.extend(
            program
                .issues
                .iter()
                .filter_map(|issue| issue.to_diagnostic().ok()),
        );
        return Err(InverseEditError::invalid_result(
            "inverse transaction result contains invalid or unresolved formulas",
            diagnostics,
        ));
    }
    Ok((workbook, document.diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::ByteSpan;

    #[test]
    fn executes_exact_patches_and_produces_a_validated_reverse() {
        let source = b"#!marksheet 0.1\n@sheet s \"S\"\n@block A1 csv\n1\n@end\n";
        let value_offset = source
            .windows(3)
            .position(|window| window == b"\n1\n")
            .map(|offset| offset + 1)
            .unwrap();
        let patch = PatchSet::for_source(
            source,
            vec![SourcePatch::new(
                ByteSpan {
                    start: u64::try_from(value_offset).unwrap(),
                    end: u64::try_from(value_offset + 1).unwrap(),
                },
                b"2",
            )],
        )
        .unwrap();
        let result = InverseTransaction::from_patch_set(patch)
            .execute(source)
            .unwrap();
        assert!(result.source.windows(1).any(|byte| byte == b"2"));
        assert_eq!(
            result.inverse.execute(&result.source).unwrap().source,
            source
        );
    }

    #[test]
    fn rejects_same_length_drift_before_validation() {
        let source = b"#!marksheet 0.1\n@sheet s \"S\"\n";
        let patch = PatchSet::for_source(
            source,
            vec![SourcePatch::new(ByteSpan { start: 26, end: 27 }, b"T")],
        )
        .unwrap();
        let error = InverseTransaction::from_patch_set(patch)
            .execute(b"#!marksheet 0.1\n@sheet s \"X\"\n")
            .unwrap_err();
        assert_eq!(error.kind, InverseEditErrorKind::PatchPrecondition);
    }
}
