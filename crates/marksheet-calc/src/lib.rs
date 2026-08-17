//! Deterministic calculation for the `portable-a1@1` formula profile.
//!
//! This crate owns formula semantics and calculation state while keeping the
//! source-authored workbook model independent of any evaluator. The public API
//! is intentionally engine-neutral so experimental adapters cannot leak their
//! private workbook or AST types into Marksheet.

#![forbid(unsafe_code)]

pub mod engine;
pub mod eval;
pub mod formula;
pub mod graph;
pub mod prepare;

pub use engine::{
    CALC_RESOURCE_LIMIT_DIAGNOSTIC, CalcEngine, CalcLimits, CalcStats, CalculatedCell,
    CalculationRequest, CalculationResult, ChangeError, ChangeSet, DirtySet,
    FORMULA_CYCLE_DIAGNOSTIC, PrepareReport, PreparedCalculation, ReferenceCalcEngine, WorkLimits,
};
pub use prepare::{PrepareError, PrepareLimits, PreparedWorkbook, UNRESOLVED_REFERENCE_DIAGNOSTIC};
