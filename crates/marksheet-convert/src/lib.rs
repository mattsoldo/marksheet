//! Deterministic, bounded conversions over [`marksheet_model::Workbook`].
//!
//! This crate deliberately accepts and returns bytes rather than paths. File
//! selection, atomic replacement, permissions, and conflict detection belong
//! to the embedding application. Every successful conversion includes a
//! versioned fidelity report; approximations and omissions mechanically make
//! that report non-lossless.

mod csv;
mod formula_profile;
mod limits;
mod project;
mod report;
mod xlsx;

pub use csv::{CsvExportSelection, CsvImportSelection, export_csv, import_csv};
pub use limits::ConversionLimits;
pub use report::{
    Conversion, ConversionDiagnostic, ConversionDiagnosticSeverity, ConversionEvent,
    ConversionFailure, ConversionFeature, ConversionLocation, ConversionReport, ConversionResult,
    ConvertError, ConvertErrorCode, FeatureOutcome, Fidelity, FormatDescriptor, FormulaDisposition,
    FormulaEvent, REPORT_SCHEMA,
};
pub use xlsx::{export_xlsx, import_xlsx};
