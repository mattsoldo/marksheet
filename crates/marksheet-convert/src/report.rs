//! Stable `marksheet-conversion@1` reporting shared by every converter.

use std::{cmp::Ordering, fmt, ops::Deref};

use marksheet_model::{ByteSpan, Coordinate, Range, SheetId, TableId};
use serde::{Deserialize, Serialize};

/// The wire identifier for [`ConversionReport`].
pub const REPORT_SCHEMA: &str = "marksheet-conversion@1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FormatDescriptor {
    pub format: String,
    pub version: String,
}

impl FormatDescriptor {
    #[must_use]
    pub fn marksheet_ir() -> Self {
        Self::new("marksheet", "0.1")
    }
    #[must_use]
    pub fn csv() -> Self {
        Self::new("csv", "rfc4180-marksheet-scalars@1")
    }
    #[must_use]
    pub fn xlsx() -> Self {
        Self::new("xlsx", "office-open-xml@1")
    }
    #[must_use]
    pub fn new(format: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            format: normalize_nonempty(format.into(), "unknown"),
            version: normalize_nonempty(version.into(), "unspecified"),
        }
    }

    /// Defensively restores constructor invariants when a caller has used the
    /// public fields in a struct literal or mutation.
    fn normalized(self) -> Self {
        Self::new(self.format, self.version)
    }
}

/// Stable top-level fidelity. This value is derived, never caller supplied.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    Lossless,
    Lossy,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureOutcome {
    Exact,
    Approximated,
    Omitted,
    Unsupported,
}

/// Common feature identifiers. [`ConversionEvent::new`] also accepts a
/// caller-defined stable identifier for registered extensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConversionFeature {
    WorkbookSettings,
    Sheet,
    Cell,
    Formula,
    Table,
    Name,
    Style,
    ColumnWidth,
    RowHeight,
    Extension,
    Macro,
    ExternalLink,
    Chart,
    PivotTable,
    AdvancedFormatting,
    Other(String),
}

impl From<ConversionFeature> for String {
    fn from(feature: ConversionFeature) -> Self {
        match feature {
            ConversionFeature::WorkbookSettings => "workbook_settings".to_owned(),
            ConversionFeature::Sheet => "sheets".to_owned(),
            ConversionFeature::Cell => "scalar_cells".to_owned(),
            ConversionFeature::Formula => "portable_formulas".to_owned(),
            ConversionFeature::Table => "tables".to_owned(),
            ConversionFeature::Name => "named_ranges".to_owned(),
            ConversionFeature::Style => "core_styles".to_owned(),
            ConversionFeature::ColumnWidth | ConversionFeature::RowHeight => {
                "row_column_geometry".to_owned()
            }
            ConversionFeature::Extension => "extensions".to_owned(),
            ConversionFeature::Macro => "macro".to_owned(),
            ConversionFeature::ExternalLink => "external_link".to_owned(),
            ConversionFeature::Chart => "chart".to_owned(),
            ConversionFeature::PivotTable => "pivot_table".to_owned(),
            ConversionFeature::AdvancedFormatting => "advanced_formatting".to_owned(),
            ConversionFeature::Other(value) => value,
        }
    }
}

/// Wire-shaped location. Cell/range spellings are canonical A1 strings so a
/// report consumer does not need to understand the Rust model representation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversionLocation {
    Cell {
        sheet: SheetId,
        cell: String,
    },
    Range {
        sheet: SheetId,
        range: String,
    },
    Table {
        #[serde(skip_serializing_if = "Option::is_none")]
        sheet: Option<SheetId>,
        table: TableId,
    },
    Sheet {
        sheet: SheetId,
    },
    Source {
        source: String,
    },
    Xlsx {
        part: String,
        reference: Option<String>,
    },
}

impl ConversionLocation {
    #[must_use]
    pub fn cell(sheet: SheetId, coordinate: Coordinate) -> Self {
        Self::Cell {
            sheet,
            cell: coordinate.to_string(),
        }
    }
    #[must_use]
    pub fn range(sheet: SheetId, range: Range) -> Self {
        Self::Range {
            sheet,
            range: range.to_string(),
        }
    }
    #[must_use]
    pub fn table(table: TableId) -> Self {
        Self::Table { sheet: None, table }
    }
    #[must_use]
    pub fn table_on_sheet(sheet: SheetId, table: TableId) -> Self {
        Self::Table {
            sheet: Some(sheet),
            table,
        }
    }
    #[must_use]
    pub fn source(description: impl Into<String>) -> Self {
        Self::Source {
            source: normalize_nonempty(description.into(), "unknown"),
        }
    }
    #[must_use]
    pub fn source_span(span: ByteSpan) -> Self {
        Self::source(format!("bytes:{}-{}", span.start, span.end))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormulaDisposition {
    Preserved,
    Translated,
    Replaced,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversionEvent {
    pub feature: String,
    pub outcome: FeatureOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula: Option<FormulaDisposition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub locations: Vec<ConversionLocation>,
}

impl ConversionEvent {
    #[must_use]
    pub fn new(feature: impl Into<String>, detail: impl Into<String>) -> Self {
        let feature = feature.into();
        Self {
            feature: normalize_feature_id(&feature),
            outcome: FeatureOutcome::Exact,
            formula: None,
            detail: Some(normalize_nonempty(detail.into(), "conversion event")),
            locations: Vec::new(),
        }
    }
    #[must_use]
    pub fn at(mut self, location: ConversionLocation) -> Self {
        self.locations.push(location);
        self
    }
    #[must_use]
    pub const fn formula(mut self, disposition: FormulaDisposition) -> Self {
        self.formula = Some(disposition);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversionDiagnostic {
    pub code: String,
    pub severity: ConversionDiagnosticSeverity,
    pub message: String,
    pub locations: Vec<ConversionLocation>,
}

/// Versioned, deterministic fidelity evidence. Outcome insertion order is the
/// source traversal order; callers cannot mutate fidelity independently.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversionReport {
    schema: String,
    source: FormatDescriptor,
    destination: FormatDescriptor,
    fidelity: Fidelity,
    outcomes: Vec<ConversionEvent>,
    diagnostics: Vec<ConversionDiagnostic>,
}

impl ConversionReport {
    pub(crate) fn new(source: FormatDescriptor, destination: FormatDescriptor) -> Self {
        Self {
            schema: REPORT_SCHEMA.to_owned(),
            source: source.normalized(),
            destination: destination.normalized(),
            fidelity: Fidelity::Lossless,
            outcomes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }
    #[must_use]
    pub const fn source(&self) -> &FormatDescriptor {
        &self.source
    }
    #[must_use]
    pub const fn destination(&self) -> &FormatDescriptor {
        &self.destination
    }
    #[must_use]
    pub const fn fidelity(&self) -> Fidelity {
        self.fidelity
    }
    #[must_use]
    pub const fn is_lossless(&self) -> bool {
        matches!(self.fidelity, Fidelity::Lossless)
    }
    #[must_use]
    pub fn outcomes(&self) -> &[ConversionEvent] {
        &self.outcomes
    }
    #[must_use]
    pub fn diagnostics(&self) -> &[ConversionDiagnostic] {
        &self.diagnostics
    }
    pub(crate) fn exact_event(&mut self, mut event: ConversionEvent) {
        event.outcome = FeatureOutcome::Exact;
        self.outcomes.push(event);
    }
    pub(crate) fn approximate(&mut self, mut event: ConversionEvent) {
        event.outcome = FeatureOutcome::Approximated;
        self.fidelity = Fidelity::Lossy;
        self.add_lossy_diagnostic(&event);
        self.outcomes.push(event);
    }
    pub(crate) fn omit(&mut self, mut event: ConversionEvent) {
        event.outcome = FeatureOutcome::Omitted;
        self.fidelity = Fidelity::Lossy;
        self.add_lossy_diagnostic(&event);
        self.outcomes.push(event);
    }
    pub(crate) fn formula(&mut self, event: FormulaEvent) {
        let mut outcome = ConversionEvent::new(
            ConversionFeature::Formula,
            match (&event.source, &event.destination) {
                (Some(source), Some(destination)) if source == destination => {
                    "formula source spelling was preserved".to_owned()
                }
                _ => "formula was translated between supported syntaxes".to_owned(),
            },
        )
        .formula(event.disposition)
        .at_many(event.locations);
        if event.disposition == FormulaDisposition::Replaced {
            outcome.outcome = FeatureOutcome::Approximated;
            self.approximate(outcome);
        } else {
            self.exact_event(outcome);
        }
    }
    /// Withdraws the outcomes already recorded for one feature at the locations
    /// `covers` accepts, together with the lossy diagnostics they raised.
    ///
    /// Outcomes are otherwise append-only: each records an independent decision
    /// in source traversal order. A converter that only learns later that an
    /// earlier decision no longer holds — a formula it first translated, then
    /// had to replace once a defined name turned out to be unimportable — has
    /// to retract the superseded claim before recording the new one. Leaving
    /// both would make the finalized report state two contradictory outcomes
    /// for the same feature and location, and `finish` sorts `Exact` ahead of
    /// `Approximated`, so a consumer reading the first outcome recorded for a
    /// location would read the stale claim rather than the true one.
    pub(crate) fn retract(
        &mut self,
        feature: ConversionFeature,
        covers: impl Fn(&ConversionLocation) -> bool,
    ) {
        let feature = String::from(feature);
        let superseded = |event: &ConversionEvent| {
            event.feature == feature
                && !event.locations.is_empty()
                && event.locations.iter().all(&covers)
        };
        let mut retracted = Vec::new();
        self.outcomes.retain(|event| {
            if superseded(event) {
                retracted.push((event.detail.clone(), event.locations.clone()));
                false
            } else {
                true
            }
        });
        if retracted.is_empty() {
            return;
        }
        self.diagnostics.retain(|diagnostic| {
            diagnostic.code != "MS4102"
                || !retracted.iter().any(|(detail, locations)| {
                    detail.as_deref() == Some(diagnostic.message.as_str())
                        && *locations == diagnostic.locations
                })
        });
        self.recompute_fidelity();
    }
    /// Rederives fidelity from the surviving outcomes. Fidelity is evidence,
    /// never independent state, so retracting the only lossy outcome has to
    /// restore `Lossless` rather than leave the report claiming a loss it can
    /// no longer point at.
    fn recompute_fidelity(&mut self) {
        if matches!(self.fidelity, Fidelity::Unsupported) {
            return;
        }
        self.fidelity = if self
            .outcomes
            .iter()
            .all(|event| matches!(event.outcome, FeatureOutcome::Exact))
        {
            Fidelity::Lossless
        } else {
            Fidelity::Lossy
        };
    }
    /// Applies the protocol's canonical ordering before a report crosses a
    /// crate boundary. Locations sort first using their semantic values (not
    /// their JSON spellings), followed by feature/code and outcome/severity,
    /// so traversal or map insertion order cannot leak.
    #[must_use]
    pub(crate) fn finish(mut self) -> Self {
        self.outcomes.sort_by(|left, right| {
            compare_locations(&left.locations, &right.locations)
                .then_with(|| left.feature.cmp(&right.feature))
                .then_with(|| outcome_rank(left.outcome).cmp(&outcome_rank(right.outcome)))
        });
        self.diagnostics.sort_by(|left, right| {
            compare_locations(&left.locations, &right.locations)
                .then_with(|| left.code.cmp(&right.code))
                .then_with(|| severity_rank(left.severity).cmp(&severity_rank(right.severity)))
        });
        self
    }
    fn add_lossy_diagnostic(&mut self, event: &ConversionEvent) {
        self.diagnostics.push(ConversionDiagnostic {
            code: "MS4102".to_owned(),
            severity: ConversionDiagnosticSeverity::Warning,
            message: event
                .detail
                .clone()
                .unwrap_or_else(|| "conversion is not lossless".to_owned()),
            locations: event.locations.clone(),
        });
    }
}

trait EventLocations {
    fn at_many(self, locations: Vec<ConversionLocation>) -> Self;
}
impl EventLocations for ConversionEvent {
    fn at_many(mut self, locations: Vec<ConversionLocation>) -> Self {
        self.locations = locations;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormulaEvent {
    pub disposition: FormulaDisposition,
    pub source: Option<String>,
    pub destination: Option<String>,
    pub locations: Vec<ConversionLocation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conversion<T> {
    pub value: T,
    pub report: ConversionReport,
}

/// A rejected conversion always carries both its typed cause and the
/// finalized `marksheet-conversion@1` unsupported report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversionFailure {
    pub error: ConvertError,
    pub report: Box<ConversionReport>,
}

impl ConversionFailure {
    #[must_use]
    pub fn new(
        error: ConvertError,
        source: FormatDescriptor,
        destination: FormatDescriptor,
        feature: impl Into<String>,
    ) -> Self {
        let error = error.normalized();
        let report = Box::new(error.unsupported_report(
            source.normalized(),
            destination.normalized(),
            normalize_feature_id(&feature.into()),
        ));
        Self { error, report }
    }

    #[must_use]
    pub fn into_parts(self) -> (ConvertError, ConversionReport) {
        (self.error, *self.report)
    }
}

impl Deref for ConversionFailure {
    type Target = ConvertError;

    fn deref(&self) -> &Self::Target {
        &self.error
    }
}

impl fmt::Display for ConversionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ConversionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Public conversion result: success and refusal both carry a report.
pub type ConversionResult<T> = Result<Conversion<T>, ConversionFailure>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvertErrorCode {
    InvalidSelection,
    InvalidCsv,
    InvalidPackage,
    UnsupportedPackage,
    InvalidWorkbook,
    ResourceLimit,
    OutputLimit,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConvertError {
    pub code: ConvertErrorCode,
    pub message: String,
    pub location: Option<ConversionLocation>,
}

impl ConvertError {
    #[must_use]
    pub fn new(code: ConvertErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: normalize_nonempty(message.into(), "conversion failed"),
            location: None,
        }
    }

    /// Defensively restores constructor invariants when a caller has used the
    /// public fields in a struct literal or mutation.
    fn normalized(mut self) -> Self {
        self.message = normalize_nonempty(self.message, "conversion failed");
        self.location = self.location.map(normalize_location);
        self
    }
    #[must_use]
    pub fn at(mut self, location: ConversionLocation) -> Self {
        self.location = Some(normalize_location(location));
        self
    }
    #[must_use]
    pub fn invalid_selection(message: impl Into<String>) -> Self {
        Self::new(ConvertErrorCode::InvalidSelection, message)
    }
    #[must_use]
    pub fn invalid_workbook(message: impl Into<String>) -> Self {
        Self::new(ConvertErrorCode::InvalidWorkbook, message)
    }
    /// Creates the mandatory unsupported report for a rejected conversion.
    #[must_use]
    pub fn unsupported_report(
        &self,
        source: FormatDescriptor,
        destination: FormatDescriptor,
        feature: impl Into<String>,
    ) -> ConversionReport {
        let error = self.clone().normalized();
        let feature = normalize_feature_id(&feature.into());
        let mut report = ConversionReport::new(source, destination);
        report.fidelity = Fidelity::Unsupported;
        report.outcomes.push(ConversionEvent {
            feature: feature.clone(),
            outcome: FeatureOutcome::Unsupported,
            formula: None,
            detail: Some(error.message.clone()),
            locations: error.location.clone().into_iter().collect(),
        });
        report.diagnostics.push(ConversionDiagnostic {
            code: if matches!(
                error.code,
                ConvertErrorCode::ResourceLimit | ConvertErrorCode::OutputLimit
            ) {
                "MS4101"
            } else if feature == "csv_selection" {
                "MS4103"
            } else if feature == "csv_import_target" {
                "MS4104"
            } else {
                "MS4105"
            }
            .to_owned(),
            severity: ConversionDiagnosticSeverity::Error,
            message: error.message,
            locations: error.location.into_iter().collect(),
        });
        report.finish()
    }
}

/// Preserves valid values exactly while replacing semantically blank public
/// inputs with a deterministic schema-valid fallback.
fn normalize_nonempty(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

/// Canonicalizes a caller-provided feature identifier to the report schema's
/// `[a-z][a-z0-9_.-]*` grammar. Valid identifiers are returned unchanged.
fn normalize_feature_id(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len().max("feature".len()));
    for character in value.chars() {
        if character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '_' | '.' | '-')
        {
            normalized.push(character);
        } else if character.is_ascii_uppercase() {
            normalized.push(character.to_ascii_lowercase());
        } else {
            normalized.push('_');
        }
    }
    if !normalized
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_lowercase)
    {
        normalized.insert_str(0, "feature_");
    }
    normalized
}

/// Restores the wire schema's location spelling invariants at the public error
/// boundary. `ConversionLocation` remains public for API compatibility, so a
/// caller can still construct a variant literal with unchecked strings.
fn normalize_location(location: ConversionLocation) -> ConversionLocation {
    match location {
        ConversionLocation::Cell { sheet, cell } => ConversionLocation::Cell {
            sheet,
            cell: Coordinate::parse(&cell)
                .map_or_else(|_| "A1".to_owned(), |coordinate| coordinate.to_string()),
        },
        ConversionLocation::Range { sheet, range } => ConversionLocation::Range {
            sheet,
            range: Range::parse(&range).map_or_else(|_| "A1".to_owned(), |range| range.to_string()),
        },
        ConversionLocation::Source { source } => ConversionLocation::Source {
            source: normalize_nonempty(source, "unknown"),
        },
        ConversionLocation::Xlsx { part, reference } => ConversionLocation::Xlsx {
            part: normalize_nonempty(part, "unknown"),
            reference,
        },
        location @ (ConversionLocation::Table { .. } | ConversionLocation::Sheet { .. }) => {
            location
        }
    }
}

/// Compares a location list lexicographically. An event may identify multiple
/// locations, and retaining that order avoids discarding the caller's
/// source-to-destination correspondence while still giving the report a total
/// order.
fn compare_locations(left: &[ConversionLocation], right: &[ConversionLocation]) -> Ordering {
    for (left_location, right_location) in left.iter().zip(right) {
        let ordering = compare_location(left_location, right_location);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// Gives each wire location a stable semantic order. In particular, A1 strings
/// cannot be sorted lexically: `C11` follows `D3` in row-major grid order, and
/// `bytes:10-11` follows `bytes:2-3` numerically.
fn compare_location(left: &ConversionLocation, right: &ConversionLocation) -> Ordering {
    let kind_ordering = location_kind_rank(left).cmp(&location_kind_rank(right));
    if kind_ordering != Ordering::Equal {
        return kind_ordering;
    }

    match (left, right) {
        (
            ConversionLocation::Cell {
                sheet: left_sheet,
                cell: left_cell,
            },
            ConversionLocation::Cell {
                sheet: right_sheet,
                cell: right_cell,
            },
        ) => left_sheet
            .cmp(right_sheet)
            .then_with(|| compare_a1(left_cell, right_cell)),
        (
            ConversionLocation::Range {
                sheet: left_sheet,
                range: left_range,
            },
            ConversionLocation::Range {
                sheet: right_sheet,
                range: right_range,
            },
        ) => left_sheet
            .cmp(right_sheet)
            .then_with(|| compare_range(left_range, right_range)),
        (
            ConversionLocation::Table {
                sheet: left_sheet,
                table: left_table,
            },
            ConversionLocation::Table {
                sheet: right_sheet,
                table: right_table,
            },
        ) => left_sheet
            .cmp(right_sheet)
            .then_with(|| left_table.cmp(right_table)),
        (
            ConversionLocation::Sheet { sheet: left_sheet },
            ConversionLocation::Sheet { sheet: right_sheet },
        ) => left_sheet.cmp(right_sheet),
        (
            ConversionLocation::Source {
                source: left_source,
            },
            ConversionLocation::Source {
                source: right_source,
            },
        ) => compare_source(left_source, right_source),
        (
            ConversionLocation::Xlsx {
                part: left_part,
                reference: left_reference,
            },
            ConversionLocation::Xlsx {
                part: right_part,
                reference: right_reference,
            },
        ) => left_part
            .cmp(right_part)
            .then_with(|| left_reference.cmp(right_reference)),
        // Equal kind ranks always mean equal enum variants. This branch makes
        // that invariant explicit if a future variant is added without a rank.
        _ => Ordering::Equal,
    }
}

const fn location_kind_rank(location: &ConversionLocation) -> u8 {
    match location {
        ConversionLocation::Source { .. } => 0,
        ConversionLocation::Sheet { .. } => 1,
        ConversionLocation::Cell { .. } => 2,
        ConversionLocation::Range { .. } => 3,
        ConversionLocation::Table { .. } => 4,
        ConversionLocation::Xlsx { .. } => 5,
    }
}

fn compare_source(left: &str, right: &str) -> Ordering {
    match (parse_byte_span(left), parse_byte_span(right)) {
        (Some(left_span), Some(right_span)) => left_span.cmp(&right_span),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.cmp(right),
    }
}

fn parse_byte_span(value: &str) -> Option<(u64, u64)> {
    let (start, end) = value.strip_prefix("bytes:")?.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

fn compare_range(left: &str, right: &str) -> Ordering {
    match (Range::parse(left), Range::parse(right)) {
        (Ok(left_range), Ok(right_range)) => {
            compare_coordinate(left_range.start, right_range.start)
                .then_with(|| compare_coordinate(left_range.end, right_range.end))
        }
        _ => left.cmp(right),
    }
}

fn compare_a1(left: &str, right: &str) -> Ordering {
    match (Coordinate::parse(left), Coordinate::parse(right)) {
        (Ok(left_coordinate), Ok(right_coordinate)) => {
            compare_coordinate(left_coordinate, right_coordinate)
        }
        _ => left.cmp(right),
    }
}

fn compare_coordinate(left: Coordinate, right: Coordinate) -> Ordering {
    left.row
        .cmp(&right.row)
        .then_with(|| left.column.cmp(&right.column))
}

const fn outcome_rank(outcome: FeatureOutcome) -> u8 {
    match outcome {
        FeatureOutcome::Exact => 0,
        FeatureOutcome::Approximated => 1,
        FeatureOutcome::Omitted => 2,
        FeatureOutcome::Unsupported => 3,
    }
}

const fn severity_rank(severity: ConversionDiagnosticSeverity) -> u8 {
    match severity {
        ConversionDiagnosticSeverity::Warning => 0,
        ConversionDiagnosticSeverity::Error => 1,
    }
}

impl fmt::Display for ConvertError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for ConvertError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serializes_to_the_normative_shape() {
        let mut report =
            ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
        report.exact_event(ConversionEvent::new(
            ConversionFeature::Cell,
            "cells retained",
        ));
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["schema"], REPORT_SCHEMA);
        assert_eq!(value["fidelity"], "lossless");
        assert!(value.get("exact").is_none());
        assert!(value.get("lossless").is_none());
        assert!(value["diagnostics"].as_array().unwrap().is_empty());
    }

    #[test]
    fn omission_mechanically_produces_lossy_warning() {
        let mut report =
            ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
        report.omit(ConversionEvent::new(
            ConversionFeature::Chart,
            "charts omitted",
        ));
        assert_eq!(report.fidelity(), Fidelity::Lossy);
        assert_eq!(report.diagnostics()[0].code, "MS4102");
    }

    #[test]
    fn finish_sorts_independently_of_insertion_order() {
        let mut first =
            ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
        first.exact_event(ConversionEvent::new("z_feature", "z"));
        first.exact_event(ConversionEvent::new("a_feature", "a"));
        let mut second =
            ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
        second.exact_event(ConversionEvent::new("a_feature", "a"));
        second.exact_event(ConversionEvent::new("z_feature", "z"));
        assert_eq!(first.finish(), second.finish());
    }

    #[test]
    fn finish_sorts_grid_locations_in_row_major_order() {
        let sheet = SheetId::parse("summary").unwrap();
        let mut report =
            ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
        for cell in ["C11", "D3", "D2"] {
            report.exact_event(ConversionEvent::new("scalar_cells", cell).at(
                ConversionLocation::cell(sheet.clone(), Coordinate::parse(cell).unwrap()),
            ));
        }

        let sorted = report.finish();
        let cells: Vec<_> = sorted
            .outcomes()
            .iter()
            .map(|outcome| match &outcome.locations[0] {
                ConversionLocation::Cell { cell, .. } => cell.as_str(),
                location => panic!("expected cell location, found {location:?}"),
            })
            .collect();
        assert_eq!(cells, ["D2", "D3", "C11"]);
    }

    #[test]
    fn finish_sorts_ranges_by_their_numeric_endpoints() {
        let sheet = SheetId::parse("summary").unwrap();
        let mut report =
            ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
        for range in ["C11:D11", "D3:E3", "D2:E2"] {
            report.exact_event(ConversionEvent::new("selected_range", range).at(
                ConversionLocation::range(sheet.clone(), Range::parse(range).unwrap()),
            ));
        }

        let sorted = report.finish();
        let ranges: Vec<_> = sorted
            .outcomes()
            .iter()
            .map(|outcome| match &outcome.locations[0] {
                ConversionLocation::Range { range, .. } => range.as_str(),
                location => panic!("expected range location, found {location:?}"),
            })
            .collect();
        assert_eq!(ranges, ["D2:E2", "D3:E3", "C11:D11"]);
    }

    #[test]
    fn finish_sorts_source_byte_spans_numerically() {
        let mut report =
            ConversionReport::new(FormatDescriptor::marksheet_ir(), FormatDescriptor::xlsx());
        for span in [
            ByteSpan::try_new(10, 11).unwrap(),
            ByteSpan::try_new(2, 3).unwrap(),
        ] {
            report.exact_event(
                ConversionEvent::new("source_feature", "source")
                    .at(ConversionLocation::source_span(span)),
            );
        }

        let sorted = report.finish();
        let locations: Vec<_> = sorted
            .outcomes()
            .iter()
            .map(|outcome| match &outcome.locations[0] {
                ConversionLocation::Source { source } => source.as_str(),
                location => panic!("expected source location, found {location:?}"),
            })
            .collect();
        assert_eq!(locations, ["bytes:2-3", "bytes:10-11"]);
    }

    #[test]
    fn retraction_removes_only_the_superseded_claim() {
        let sheet = SheetId::parse("summary").unwrap();
        let target = ConversionLocation::cell(sheet.clone(), Coordinate::parse("A3").unwrap());
        let neighbor = ConversionLocation::cell(sheet, Coordinate::parse("A4").unwrap());
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        report.exact_event(
            ConversionEvent::new(ConversionFeature::Formula, "translated")
                .at(target.clone())
                .formula(FormulaDisposition::Translated),
        );
        report.exact_event(
            ConversionEvent::new(ConversionFeature::Cell, "cell imported").at(target.clone()),
        );
        report.exact_event(
            ConversionEvent::new(ConversionFeature::Formula, "translated").at(neighbor),
        );

        report.retract(ConversionFeature::Formula, |location| *location == target);

        let surviving: Vec<_> = report
            .outcomes()
            .iter()
            .map(|event| (event.feature.as_str(), event.locations.len()))
            .collect();
        assert_eq!(
            surviving,
            [("scalar_cells", 1), ("portable_formulas", 1)],
            "{:?}",
            report.outcomes()
        );
    }

    #[test]
    fn retracting_the_only_lossy_outcome_restores_lossless_fidelity() {
        let location = ConversionLocation::source("bytes:0-1");
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        report.approximate(
            ConversionEvent::new(ConversionFeature::Formula, "approximated")
                .at(location.clone())
                .formula(FormulaDisposition::Replaced),
        );
        assert_eq!(report.fidelity(), Fidelity::Lossy);

        report.retract(ConversionFeature::Formula, |candidate| {
            *candidate == location
        });

        assert_eq!(report.fidelity(), Fidelity::Lossless);
        assert!(report.outcomes().is_empty());
        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );
    }

    /// An unrelated diagnostic that happens to carry the same message must
    /// survive: retraction withdraws one claim, not a whole message class.
    #[test]
    fn retraction_keeps_diagnostics_it_did_not_raise() {
        let sheet = SheetId::parse("summary").unwrap();
        let target = ConversionLocation::cell(sheet.clone(), Coordinate::parse("A3").unwrap());
        let neighbor = ConversionLocation::cell(sheet, Coordinate::parse("A4").unwrap());
        let mut report =
            ConversionReport::new(FormatDescriptor::xlsx(), FormatDescriptor::marksheet_ir());
        report.approximate(
            ConversionEvent::new(ConversionFeature::Formula, "same detail").at(target.clone()),
        );
        report.approximate(
            ConversionEvent::new(ConversionFeature::Formula, "same detail").at(neighbor.clone()),
        );

        report.retract(ConversionFeature::Formula, |candidate| *candidate == target);

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].locations, [neighbor]);
        assert_eq!(report.fidelity(), Fidelity::Lossy);
    }

    #[test]
    fn general_rejection_uses_ms4105() {
        let report = ConvertError::new(ConvertErrorCode::InvalidPackage, "bad package")
            .unsupported_report(
                FormatDescriptor::xlsx(),
                FormatDescriptor::marksheet_ir(),
                "package",
            );
        assert_eq!(report.diagnostics()[0].code, "MS4105");
    }

    #[test]
    fn output_limit_rejection_uses_ms4101() {
        let report = ConvertError::new(ConvertErrorCode::OutputLimit, "output too large")
            .unsupported_report(
                FormatDescriptor::marksheet_ir(),
                FormatDescriptor::xlsx(),
                "resource_limit.output_bytes",
            );
        assert_eq!(report.fidelity(), Fidelity::Unsupported);
        assert_eq!(report.diagnostics()[0].code, "MS4101");
    }

    #[test]
    fn public_constructors_produce_schema_safe_failure_reports() {
        let failure = ConversionFailure::new(
            ConvertError::new(ConvertErrorCode::InvalidPackage, " \t\n "),
            FormatDescriptor::new("", " "),
            FormatDescriptor::new("xlsx", ""),
            " 1 Bad/Feature ",
        );

        let value = serde_json::to_value(&failure.report).unwrap();
        assert_eq!(value["source"]["format"], "unknown");
        assert_eq!(value["source"]["version"], "unspecified");
        assert_eq!(value["destination"]["format"], "xlsx");
        assert_eq!(value["destination"]["version"], "unspecified");
        assert_eq!(value["outcomes"][0]["detail"], "conversion failed");
        assert_eq!(value["diagnostics"][0]["message"], "conversion failed");
        assert_schema_feature_id(value["outcomes"][0]["feature"].as_str().unwrap());
    }

    #[test]
    fn conversion_failure_defends_against_mutated_public_inputs() {
        let failure = ConversionFailure::new(
            ConvertError {
                code: ConvertErrorCode::InvalidPackage,
                message: String::new(),
                location: None,
            },
            FormatDescriptor {
                format: String::new(),
                version: String::new(),
            },
            FormatDescriptor {
                format: String::new(),
                version: String::new(),
            },
            "!",
        );

        let value = serde_json::to_value(&failure.report).unwrap();
        assert_eq!(value["source"]["format"], "unknown");
        assert_eq!(value["destination"]["version"], "unspecified");
        assert_eq!(value["diagnostics"][0]["message"], "conversion failed");
        assert_schema_feature_id(value["outcomes"][0]["feature"].as_str().unwrap());
    }

    #[test]
    fn public_error_location_inputs_are_normalized_before_serialization() {
        let source_report = ConvertError::new(ConvertErrorCode::InvalidPackage, "bad package")
            .at(ConversionLocation::Source {
                source: " \t".to_owned(),
            })
            .unsupported_report(
                FormatDescriptor::xlsx(),
                FormatDescriptor::marksheet_ir(),
                "package",
            );
        let source_json = serde_json::to_value(source_report).unwrap();
        assert_eq!(
            source_json["outcomes"][0]["locations"][0]["source"],
            "unknown"
        );

        let failure = ConversionFailure::new(
            ConvertError {
                code: ConvertErrorCode::InvalidPackage,
                message: "bad package".to_owned(),
                location: Some(ConversionLocation::Xlsx {
                    part: String::new(),
                    reference: None,
                }),
            },
            FormatDescriptor::xlsx(),
            FormatDescriptor::marksheet_ir(),
            "package",
        );
        let failure_json = serde_json::to_value(&failure.report).unwrap();
        assert_eq!(
            failure_json["outcomes"][0]["locations"][0]["part"],
            "unknown"
        );
    }

    fn assert_schema_feature_id(feature: &str) {
        assert!(
            feature
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
                && feature.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'.' | b'-')),
            "feature ID must satisfy the report schema: {feature:?}",
        );
    }
}
