//! Helpers for the shared, stable diagnostic representation.

pub use marksheet_model::Diagnostic;
use marksheet_model::{ByteSpan, DiagnosticCode, LabeledSpan, Severity};

use crate::cst::Span;

#[must_use]
pub(crate) fn error(code: &'static str, message: impl Into<String>, primary: Span) -> Diagnostic {
    diagnostic(code, Severity::Error, message, primary)
}

#[must_use]
pub(crate) fn warning(code: &'static str, message: impl Into<String>, primary: Span) -> Diagnostic {
    diagnostic(code, Severity::Warning, message, primary)
}

fn diagnostic(
    code: &'static str,
    severity: Severity,
    message: impl Into<String>,
    primary: Span,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code).expect("syntax diagnostics use registered MS codes"),
        severity,
        message: message.into(),
        primary: LabeledSpan {
            span: ByteSpan {
                start: primary.start as u64,
                end: primary.end as u64,
            },
            label: None,
        },
        related: Vec::new(),
        context: None,
        suggestion: None,
    }
}
