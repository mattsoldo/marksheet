//! Parsing and source-independent manipulation of `portable-a1@1` formulas.
//!
//! The parser accepts complete formula values, including their leading `=`.
//! Spans therefore use byte offsets into the original formula value; the
//! expression itself starts after byte zero. Resolution and evaluation live
//! outside this module so syntactically valid unknown names remain parseable.

mod adjust;
mod ast;
mod format;
mod lexer;
mod parser;
mod rewrite;

pub use adjust::{AdjustmentError, CopyOffset, FormulaTemplate, adjust_references};
pub use ast::{
    A1Reference, BinaryOperator, Expr, ExprKind, Formula, FunctionCall, Literal, RangeReference,
    Reference, StructuredReference, TableRegion, UnaryOperator,
};
pub use format::{FormulaFormatError, format_expression, format_formula};
pub use lexer::{FormulaError, FormulaErrorKind, Token, TokenKind, lex};
pub use parser::{ParseLimits, parse};
pub use rewrite::{
    A1Copy, A1Move, FormulaPatch, FormulaRewrite, FormulaRewriteError, FormulaRewriteResult,
    RenameKind, rewrite_formula, rewrite_formula_text,
};

/// Stable diagnostic code for malformed `portable-a1@1` formulas.
pub const FORMULA_SYNTAX_DIAGNOSTIC: &str = "MS2202";
