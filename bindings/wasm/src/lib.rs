//! A bounded, batched Marksheet interface intended for a Web Worker.
//!
//! The browser-facing API is JSON rather than a collection of cell getters:
//! every request carries a protocol version, request id, and document revision.
//! This keeps a restarted worker, a stale UI event, and an external source
//! replacement from being silently conflated. The sparse projection in this
//! crate delegates presentation concerns to `marksheet-view` while keeping the
//! worker protocol stable and browser-oriented.

#![forbid(unsafe_code)]

use std::{
    fmt,
    io::{self, Write},
};

use marksheet_calc::{
    CalcEngine, CalcLimits, CalculationRequest, CalculationResult, PreparedCalculation,
    ReferenceCalcEngine,
};
use marksheet_edit::{
    patch::SourcePatch,
    transaction::{EditExpectations, EditOperation, EditTransaction, SourceExpectation},
};
use marksheet_extensions::{
    AVAILABILITY_REQUIRED_DIAGNOSTIC, AVAILABILITY_WARNING_DIAGNOSTIC, CapabilityAvailability,
    ExtensionLimits, ExtensionRegistry, ExtensionReport, ExtensionScope, InstanceOutcome,
};
use marksheet_model::{
    ApplyTarget, ByteSpan, Coordinate, Diagnostic, ExtensionId, FillTarget, NameTarget, Range,
    Severity, Sheet, SheetId, SheetItem, Value, Workbook,
};
use marksheet_syntax::{ParseOptions, ParsedDocument, parse_with_options};
use marksheet_view::{
    ViewLimits, VisibleRegion as ViewVisibleRegion, VisibleRegionRequest, WorkbookView,
};
use serde::{Deserialize, Serialize};

/// The only protocol revision understood by this binding.
pub const PROTOCOL_VERSION: &str = "marksheet-worker@1";
const MAX_JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_DIAGNOSTIC_CANDIDATES: usize = 4_096;
const MAX_SOURCE_RECORDS: usize = 4_096;
const MAX_FORMULA_CANDIDATES: usize = 4_096;
const MAX_CSV_FIELD_DELIMITERS: usize = 4_096;
/// Fixed ceiling for one raw JSON message before it reaches `serde_json`.
///
/// This intentionally matches the default response cap.  A maximum-size
/// 5 MiB lossless source is represented as a JSON byte array, so the normal
/// browser request remains comfortably below this limit.
pub const MAX_REQUEST_JSON_BYTES: usize = 32 * 1024 * 1024;
/// One browser edit is bounded independently from the source and response
/// budgets so a single transaction cannot turn into an unbounded planning
/// workload.
pub const MAX_EDIT_OPERATIONS: usize = 1_024;
/// Recursive JSON payload budget for one browser edit transaction.
pub const MAX_EDIT_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Resource limits enforced before an untrusted worker request reaches core
/// parsing, sparse projection, or the calculation engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionLimits {
    /// Maximum input size accepted by `open` and `replace_source`.
    pub max_source_bytes: usize,
    /// Maximum rectangular area requested by `visible_region`.
    pub max_viewport_cells: u64,
    /// Maximum rectangular area calculated or emitted by `calculate`.
    pub max_calculation_cells: u64,
    /// Maximum authored cells returned by one sparse projection.
    pub max_presented_cells: usize,
    /// Maximum diagnostics retained in a browser response.
    pub max_diagnostics: usize,
    /// Maximum serialized JSON response emitted by one worker request.
    pub max_response_bytes: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 5 * 1024 * 1024,
            max_viewport_cells: 250_000,
            max_calculation_cells: 100_000,
            max_presented_cells: 100_000,
            max_diagnostics: 1_000,
            // `source_bytes` and source patches use lossless `number[]` JSON
            // today, whose worst case is four bytes per source byte plus the
            // small response envelope. Keep the response bound above that
            // encoded form for the accepted 5 MiB source limit.
            max_response_bytes: 32 * 1024 * 1024,
        }
    }
}

/// A complete client-to-worker message. `source` is a byte array so lossless
/// documents never need to cross the boundary through a lossy UTF-8 string.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol: String,
    pub request_id: String,
    pub revision: u64,
    pub request: WorkerRequest,
}

/// Requests deliberately return document- or range-level data, never a getter
/// for a single cell or a dense used-range materialization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerRequest {
    Open { source: Vec<u8> },
    ReplaceSource { source: Vec<u8> },
    WorkbookSnapshot,
    VisibleRegion { sheet: String, range: Range },
    Calculate { sheet: String, range: Range },
    Edit { transaction: WorkerEditTransaction },
    SourceBytes,
}

/// Browser-facing edit transaction.
///
/// The core edit type serializes its default expectations, but accepts the
/// field when omitted during deserialization. This explicit wire DTO preserves
/// that asymmetric inbound contract in generated TypeScript instead of asking
/// a serialization-only schema generator to guess about `serde(default)`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerEditTransaction {
    pub operations: Vec<EditOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expectations: Option<WorkerEditExpectations>,
}

/// Browser-safe edit preconditions.
///
/// The core representation includes a `u64` FNV fingerprint. JSON numbers
/// cannot represent every `u64` exactly, so the worker never accepts it from
/// JavaScript. Exact bytes remain the authoritative precondition and are used
/// to derive fresh core metadata after crossing the binding boundary.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerEditExpectations {
    #[serde(default)]
    pub source: Option<WorkerSourceExpectation>,
}

/// Exact source bytes supplied by a browser edit precondition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerSourceExpectation {
    pub bytes: Vec<u8>,
}

impl From<EditTransaction> for WorkerEditTransaction {
    fn from(transaction: EditTransaction) -> Self {
        Self {
            operations: transaction.operations,
            expectations: Some(WorkerEditExpectations {
                source: transaction
                    .expectations
                    .source
                    .map(|source| WorkerSourceExpectation {
                        bytes: source.bytes,
                    }),
            }),
        }
    }
}

impl From<WorkerEditTransaction> for EditTransaction {
    fn from(transaction: WorkerEditTransaction) -> Self {
        Self {
            operations: transaction.operations,
            expectations: EditExpectations {
                source: transaction
                    .expectations
                    .and_then(|expectations| expectations.source)
                    .map(|source| SourceExpectation::capture(source.bytes)),
            },
        }
    }
}

/// A complete worker response. Request ids are echoed even for protocol and
/// parsing failures, allowing the host to safely discard stale promises.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol: String,
    pub request_id: String,
    pub revision: u64,
    pub response: WorkerResponse,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerResponse {
    Opened {
        snapshot: WorkbookSnapshot,
    },
    Replaced {
        snapshot: WorkbookSnapshot,
    },
    Snapshot {
        snapshot: WorkbookSnapshot,
    },
    VisibleRegion {
        region: ViewVisibleRegion,
        diagnostics_omitted: usize,
    },
    Calculation {
        calculation: CalculationResult,
        diagnostics_omitted: usize,
    },
    Edited {
        changed: bool,
        patches: Vec<PatchSummary>,
        snapshot: WorkbookSnapshot,
    },
    SourceBytes {
        source: Vec<u8>,
    },
    Error {
        error: WorkerError,
    },
}

/// A source patch encoded without exposing the editing crate's internal patch
/// set or its exact source snapshot across the worker boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PatchSummary {
    pub span: ByteSpan,
    pub replacement: Vec<u8>,
}

impl From<&SourcePatch> for PatchSummary {
    fn from(patch: &SourcePatch) -> Self {
        Self {
            span: patch.span,
            replacement: patch.replacement.clone(),
        }
    }
}

/// Metadata sufficient for a UI to choose sheets and render headers without
/// receiving the entire semantic IR on every request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkbookSnapshot {
    pub revision: u64,
    /// Parser, formula, and trusted-extension diagnostics for the exact source
    /// at `revision`.
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostics_omitted: usize,
    /// Source/formula errors disable editing. Extension validation findings do
    /// not: editing is how a user can repair a failed assertion.
    pub editable: bool,
    pub locale: String,
    pub timezone: String,
    pub formula_profile: String,
    pub sheets: Vec<SheetSummary>,
    pub style_count: usize,
    /// Typed workbook names in declaration order, suitable for a name box.
    pub names: Vec<NameSummary>,
    pub name_count: usize,
    /// Trusted extension declarations and instances, without opaque payload
    /// bytes crossing the worker boundary.
    pub extension_declarations: Vec<ExtensionDeclarationSummary>,
    pub extension_instances: Vec<ExtensionInstanceSummary>,
    /// Exact host support and independently meaningful completeness claims.
    pub extension_support: ExtensionSupportSummary,
}

/// One exact workbook-level extension declaration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDeclarationSummary {
    pub capability: String,
    pub required: bool,
    pub availability: ExtensionAvailabilitySummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_span: Option<ByteSpan>,
}

/// Availability is exact-major: a registered `id@1` never satisfies `id@2`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionAvailabilitySummary {
    Available,
    UnavailableOptional,
    UnavailableRequired,
}

/// Opaque extension placement. Payload content deliberately remains private.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionScopeSummary {
    Workbook,
    Sheet { sheet: String },
}

/// Host disposition for one opaque instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionInstanceOutcomeSummary {
    Processed,
    SkippedUnavailable,
    SkippedUndeclared,
    RejectedDuplicate,
    RejectedByLimit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionInstanceSummary {
    pub capability: String,
    pub name: String,
    pub scope: ExtensionScopeSummary,
    pub declared: bool,
    pub supported: bool,
    pub outcome: ExtensionInstanceOutcomeSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_span: Option<ByteSpan>,
}

/// Extension host state for this exact document revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
// These are deliberately independent protocol claims, matching the extension
// host report rather than collapsing distinct capability and validity states.
#[allow(clippy::struct_excessive_bools)]
pub struct ExtensionSupportSummary {
    /// Statically linked exact capabilities, in deterministic lexical order.
    pub supported_capabilities: Vec<String>,
    pub capabilities_complete: bool,
    pub calculation_complete: bool,
    pub rendering_complete: bool,
    pub validation_complete: bool,
    pub valid: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SheetSummary {
    pub id: String,
    pub label: String,
    pub authored_cell_count: u64,
    pub table_count: usize,
}

/// A workbook-scoped name exposed without flattening its semantic target into
/// display text. Consumers can distinguish a cell, range, and table column.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NameSummary {
    pub id: String,
    pub target: NameTarget,
    /// Concrete data destination used by the name box. A header-only table
    /// column intentionally has no data range and remains `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedNameTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_span: Option<ByteSpan>,
}

/// A sheet-qualified coordinate range resolved from a semantic name target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedNameTarget {
    pub sheet: String,
    pub range: Range,
}

/// Machine-actionable worker error classes. Parser and compiler diagnostics
/// stay structured rather than being flattened into browser-only strings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerErrorCode {
    Protocol,
    Session,
    StaleRevision,
    Limit,
    InvalidSource,
    Calculation,
    Edit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerError {
    pub code: WorkerErrorCode,
    pub message: String,
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostics_omitted: usize,
}

impl WorkerError {
    fn new(code: WorkerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            diagnostics: Vec::new(),
            diagnostics_omitted: 0,
        }
    }

    fn with_diagnostics(
        code: WorkerErrorCode,
        message: impl Into<String>,
        diagnostics: Vec<Diagnostic>,
        diagnostics_omitted: usize,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            diagnostics,
            diagnostics_omitted,
        }
    }
}

/// One mutable document session. It is purposefully usable from native tests
/// as well as Wasm; the worker host owns scheduling and cancellation by
/// replacing/restarting a `WorkerRuntime` between request messages.
#[derive(Debug)]
pub struct WorkbenchSession {
    source: Vec<u8>,
    workbook: Workbook,
    view: WorkbookView,
    calculation: Option<PreparedCalculation>,
    extension_report: ExtensionReport,
    calculation_complete: bool,
    rendering_complete: bool,
    editable: bool,
    /// Full source diagnostics retained within the independently bounded
    /// source-structure budget. Browser responses receive the capped subset.
    persistent_diagnostics: Vec<Diagnostic>,
    diagnostics: Vec<Diagnostic>,
    diagnostics_omitted: usize,
    revision: u64,
    limits: SessionLimits,
}

impl WorkbenchSession {
    /// Opens one valid or capability-incomplete recoverable source snapshot at
    /// revision one.
    ///
    /// # Errors
    ///
    /// Returns structured diagnostics for invalid source or a resource limit.
    pub fn open(source: Vec<u8>, limits: SessionLimits) -> Result<Self, WorkerError> {
        validate_source_size(&source, &limits)?;
        let prepared = prepare_source(&source, &limits)?;
        let (diagnostics, diagnostics_omitted) = bounded_diagnostics(
            prepared.persistent_diagnostics.clone(),
            limits.max_diagnostics,
        );
        Ok(Self {
            source,
            workbook: prepared.workbook,
            view: prepared.view,
            calculation: None,
            extension_report: prepared.extension_report,
            calculation_complete: prepared.calculation_complete,
            rendering_complete: prepared.rendering_complete,
            editable: prepared.editable,
            persistent_diagnostics: prepared.persistent_diagnostics,
            diagnostics,
            diagnostics_omitted,
            revision: 1,
            limits,
        })
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn source_bytes(&self) -> &[u8] {
        &self.source
    }

    #[must_use]
    pub fn snapshot(&self) -> WorkbookSnapshot {
        snapshot(self)
    }

    /// Replaces the complete source atomically. Reparse succeeds before the
    /// previous session state is discarded.
    ///
    /// # Errors
    ///
    /// Returns an error when the replacement exceeds resource limits or is
    /// not a recoverable document that can be projected by `marksheet-view`.
    pub fn replace_source(&mut self, source: Vec<u8>) -> Result<WorkbookSnapshot, WorkerError> {
        let replacement = Self::open(source, self.limits.clone())?;
        self.source = replacement.source;
        self.workbook = replacement.workbook;
        self.view = replacement.view;
        self.calculation = None;
        self.extension_report = replacement.extension_report;
        self.calculation_complete = replacement.calculation_complete;
        self.rendering_complete = replacement.rendering_complete;
        self.editable = replacement.editable;
        self.persistent_diagnostics = replacement.persistent_diagnostics;
        self.diagnostics = replacement.diagnostics;
        self.diagnostics_omitted = replacement.diagnostics_omitted;
        self.revision = self.revision.wrapping_add(1);
        Ok(self.snapshot())
    }

    /// Returns only authored cells inside the requested viewport. It does not
    /// allocate a dense used extent; fills, styles, and geometry are projected
    /// by `marksheet-view` only for the requested coordinates.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid sheet, an oversized viewport, or an
    /// invalid sparse presentation request.
    pub fn visible_region(
        &mut self,
        sheet_id: &str,
        range: Range,
    ) -> Result<ViewVisibleRegion, WorkerError> {
        self.visible_region_with_diagnostic_metadata(sheet_id, range)
            .map(|(region, _)| region)
    }

    fn visible_region_with_diagnostic_metadata(
        &mut self,
        sheet_id: &str,
        range: Range,
    ) -> Result<(ViewVisibleRegion, usize), WorkerError> {
        validate_area(range, self.limits.max_viewport_cells, "viewport")?;
        let sheet = parse_sheet_id(sheet_id)?;
        let mut request = VisibleRegionRequest::new(sheet, range);
        // Formula calculation remains a separate worker operation.
        request.calculate = false;
        let mut region = self.view.visible_region(&request).map_err(|error| {
            WorkerError::new(
                WorkerErrorCode::Session,
                format!("unable to project visible region: {error}"),
            )
        })?;
        region.completeness.calculation_complete &= self.calculation_complete;
        region.completeness.rendering_complete &= self.rendering_complete;
        let (diagnostics, diagnostics_omitted) = bounded_diagnostics(
            merge_diagnostics(
                &self.persistent_diagnostics,
                &std::mem::take(&mut region.diagnostics),
            ),
            self.limits.max_diagnostics,
        );
        region.diagnostics = diagnostics;
        Ok((region, diagnostics_omitted))
    }

    /// Calculates a bounded explicit selection. Preparation is lazy and is
    /// invalidated only by source replacement or a committed semantic edit.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid sheet, an oversized request, or a
    /// workbook that cannot be prepared by the calculation engine.
    pub fn calculate(
        &mut self,
        sheet_id: &str,
        range: Range,
    ) -> Result<CalculationResult, WorkerError> {
        self.calculate_with_diagnostic_metadata(sheet_id, range)
            .map(|(calculation, _)| calculation)
    }

    fn calculate_with_diagnostic_metadata(
        &mut self,
        sheet_id: &str,
        range: Range,
    ) -> Result<(CalculationResult, usize), WorkerError> {
        validate_area(range, self.limits.max_calculation_cells, "calculation")?;
        if !self.calculation_complete {
            return Err(WorkerError::with_diagnostics(
                WorkerErrorCode::Calculation,
                "calculated values are unavailable because workbook capabilities are incomplete",
                self.diagnostics.clone(),
                self.diagnostics_omitted,
            ));
        }
        let sheet = parse_sheet_id(sheet_id)?;
        if self.calculation.is_none() {
            let engine = ReferenceCalcEngine;
            let mut limits = CalcLimits::default();
            limits.work.max_output_cells = self.limits.max_calculation_cells;
            limits.work.max_graph_nodes = bounded_work_limit(self.limits.max_calculation_cells);
            limits.work.max_dirty_cells = bounded_work_limit(self.limits.max_calculation_cells);
            limits.work.max_evaluated_cells = bounded_work_limit(self.limits.max_calculation_cells);
            let report = engine.prepare(&self.workbook, limits);
            self.calculation = report.calculation;
            if self.calculation.is_none() {
                let (diagnostics, diagnostics_omitted) = bounded_diagnostics(
                    merge_diagnostics(&self.persistent_diagnostics, &report.diagnostics),
                    self.limits.max_diagnostics,
                );
                return Err(WorkerError::with_diagnostics(
                    WorkerErrorCode::Calculation,
                    "workbook could not be prepared for calculation",
                    diagnostics,
                    diagnostics_omitted,
                ));
            }
        }
        let calculation = self.calculation.as_mut().ok_or_else(|| {
            WorkerError::new(
                WorkerErrorCode::Calculation,
                "calculation state was not initialized",
            )
        })?;
        let engine = ReferenceCalcEngine;
        let mut result = engine.calculate(calculation, &CalculationRequest::new(sheet, range));
        let (diagnostics, diagnostics_omitted) = bounded_diagnostics(
            merge_diagnostics(&self.persistent_diagnostics, &result.diagnostics),
            self.limits.max_diagnostics,
        );
        result.diagnostics = diagnostics;
        Ok((result, diagnostics_omitted))
    }

    /// Applies one source-preserving semantic transaction atomically, resets
    /// calculation state, and advances the document revision exactly once.
    ///
    /// # Errors
    ///
    /// Returns the editing engine's structured failure when the transaction is
    /// invalid, conflicts with source, or cannot be represented as patches.
    pub fn edit(
        &mut self,
        transaction: &EditTransaction,
    ) -> Result<(bool, Vec<PatchSummary>, WorkbookSnapshot), WorkerError> {
        if !self.editable {
            return Err(WorkerError::with_diagnostics(
                WorkerErrorCode::Edit,
                "workbook is not editable until its error diagnostics are resolved",
                self.diagnostics.clone(),
                self.diagnostics_omitted,
            ));
        }
        let options = extension_parse_options();
        let result = transaction
            .execute_with_parse_options(&self.source, &options)
            .map_err(|error| {
                let message =
                    if error.kind == marksheet_edit::transaction::EditErrorKind::VirtualCell {
                        format!("virtual cell edit refused: {}", error.message)
                    } else {
                        error.message
                    };
                WorkerError::with_diagnostics(WorkerErrorCode::Edit, message, error.diagnostics, 0)
            })?;
        validate_source_size(&result.source, &self.limits)?;
        // Keep syntax-backed source locations aligned with the edited bytes.
        // The edit engine has already validated these bytes; rebuilding the
        // view before mutating this session preserves atomic worker semantics.
        let prepared = prepare_source(&result.source, &self.limits)?;
        let changed = result.changed();
        let patches = result
            .patches
            .patches()
            .iter()
            .map(PatchSummary::from)
            .collect();
        if changed {
            self.source = result.source;
            self.workbook = prepared.workbook;
            let persistent_diagnostics = prepared.persistent_diagnostics;
            let (diagnostics, omitted) =
                bounded_diagnostics(persistent_diagnostics.clone(), self.limits.max_diagnostics);
            self.persistent_diagnostics = persistent_diagnostics;
            self.diagnostics = diagnostics;
            self.diagnostics_omitted = omitted;
            self.view = prepared.view;
            self.extension_report = prepared.extension_report;
            self.calculation_complete = prepared.calculation_complete;
            self.rendering_complete = prepared.rendering_complete;
            self.editable = prepared.editable;
            self.calculation = None;
            self.revision = self.revision.wrapping_add(1);
        }
        Ok((changed, patches, self.snapshot()))
    }
}

/// Stateful dispatcher designed to be owned by one Web Worker. Hosts can
/// terminate a busy worker and create another runtime to provide cancellation;
/// no background task mutates a session after a response has been returned.
#[derive(Debug)]
pub struct WorkerRuntime {
    limits: SessionLimits,
    session: Option<WorkbenchSession>,
}

impl WorkerRuntime {
    #[must_use]
    pub fn new(limits: SessionLimits) -> Self {
        Self {
            limits,
            session: None,
        }
    }

    #[must_use]
    pub fn dispatch(&mut self, request: RequestEnvelope) -> ResponseEnvelope {
        let request_id = request.request_id.clone();
        let response = self.dispatch_inner(request);
        let revision = self.session.as_ref().map_or(0, WorkbenchSession::revision);
        ResponseEnvelope {
            protocol: PROTOCOL_VERSION.to_owned(),
            request_id,
            revision,
            response,
        }
    }

    /// Parses and emits a JSON protocol message. Malformed JSON becomes a
    /// normal protocol error instead of escaping across the Wasm boundary.
    #[must_use]
    pub fn dispatch_json(&mut self, request_json: &str) -> String {
        if request_json.len() > MAX_REQUEST_JSON_BYTES {
            return serialize_unbounded_error(&ResponseEnvelope {
                protocol: PROTOCOL_VERSION.to_owned(),
                request_id: bounded_request_id(request_json),
                revision: self.session.as_ref().map_or(0, WorkbenchSession::revision),
                response: WorkerResponse::Error {
                    error: WorkerError::new(
                        WorkerErrorCode::Limit,
                        format!(
                            "request exceeds the {MAX_REQUEST_JSON_BYTES} byte worker JSON limit"
                        ),
                    ),
                },
            });
        }
        match serde_json::from_str::<RequestEnvelope>(request_json) {
            Ok(request) => {
                let request_id = request.request_id.clone();
                let response = self.dispatch(request);
                serialize_response(&response, self.limits.max_response_bytes).unwrap_or_else(
                    |error| {
                        serialize_unbounded_error(&ResponseEnvelope {
                            protocol: PROTOCOL_VERSION.to_owned(),
                            request_id,
                            revision: self.session.as_ref().map_or(0, WorkbenchSession::revision),
                            response: WorkerResponse::Error {
                                error: WorkerError::new(WorkerErrorCode::Limit, error),
                            },
                        })
                    },
                )
            }
            Err(error) => serialize_unbounded_error(&ResponseEnvelope {
                protocol: PROTOCOL_VERSION.to_owned(),
                // Request envelopes may be structurally invalid while still
                // carrying a usable id. Echo it so a browser client can
                // reject the matching promise instead of waiting forever.
                request_id: bounded_request_id(request_json),
                revision: self.session.as_ref().map_or(0, WorkbenchSession::revision),
                response: WorkerResponse::Error {
                    error: WorkerError::new(WorkerErrorCode::Protocol, error.to_string()),
                },
            }),
        }
    }

    fn dispatch_inner(&mut self, envelope: RequestEnvelope) -> WorkerResponse {
        if envelope.protocol != PROTOCOL_VERSION {
            return WorkerResponse::Error {
                error: WorkerError::new(
                    WorkerErrorCode::Protocol,
                    format!("unsupported protocol {:?}", envelope.protocol),
                ),
            };
        }
        if envelope.revision > MAX_JS_SAFE_INTEGER {
            return WorkerResponse::Error {
                error: WorkerError::new(
                    WorkerErrorCode::Limit,
                    format!(
                        "request revision exceeds JavaScript's maximum safe integer ({MAX_JS_SAFE_INTEGER})"
                    ),
                ),
            };
        }

        let is_open = matches!(envelope.request, WorkerRequest::Open { .. });
        if is_open {
            if self.session.is_some() {
                return WorkerResponse::Error {
                    error: WorkerError::new(
                        WorkerErrorCode::Protocol,
                        "a workbook is already open; use replace_source to advance its revision",
                    ),
                };
            }
            if envelope.revision != 0 {
                return stale_revision(envelope.revision, 0);
            }
        } else {
            let Some(session) = self.session.as_ref() else {
                return WorkerResponse::Error {
                    error: WorkerError::new(WorkerErrorCode::Session, "no workbook is open"),
                };
            };
            if envelope.revision != session.revision() {
                return stale_revision(envelope.revision, session.revision());
            }
        }

        match envelope.request {
            WorkerRequest::Open { source } => {
                match WorkbenchSession::open(source, self.limits.clone()) {
                    Ok(session) => {
                        let snapshot = session.snapshot();
                        self.session = Some(session);
                        WorkerResponse::Opened { snapshot }
                    }
                    Err(error) => WorkerResponse::Error { error },
                }
            }
            WorkerRequest::ReplaceSource { source } => match session_mut(&mut self.session)
                .and_then(|session| session.replace_source(source))
            {
                Ok(snapshot) => WorkerResponse::Replaced { snapshot },
                Err(error) => WorkerResponse::Error { error },
            },
            WorkerRequest::WorkbookSnapshot => match session_ref(self.session.as_ref()) {
                Ok(session) => WorkerResponse::Snapshot {
                    snapshot: session.snapshot(),
                },
                Err(error) => WorkerResponse::Error { error },
            },
            WorkerRequest::VisibleRegion { sheet, range } => match session_mut(&mut self.session)
                .and_then(|session| session.visible_region_with_diagnostic_metadata(&sheet, range))
            {
                Ok((region, diagnostics_omitted)) => WorkerResponse::VisibleRegion {
                    region,
                    diagnostics_omitted,
                },
                Err(error) => WorkerResponse::Error { error },
            },
            WorkerRequest::Calculate { sheet, range } => match session_mut(&mut self.session)
                .and_then(|session| session.calculate_with_diagnostic_metadata(&sheet, range))
            {
                Ok((calculation, diagnostics_omitted)) => WorkerResponse::Calculation {
                    calculation,
                    diagnostics_omitted,
                },
                Err(error) => WorkerResponse::Error { error },
            },
            WorkerRequest::Edit { transaction } => self.dispatch_edit(transaction),
            WorkerRequest::SourceBytes => match session_ref(self.session.as_ref()) {
                Ok(session) => WorkerResponse::SourceBytes {
                    source: session.source_bytes().to_vec(),
                },
                Err(error) => WorkerResponse::Error { error },
            },
        }
    }

    fn dispatch_edit(&mut self, transaction: WorkerEditTransaction) -> WorkerResponse {
        if let Err(error) = validate_worker_edit_transaction(&transaction, &self.limits) {
            return WorkerResponse::Error { error };
        }
        let transaction = EditTransaction::from(transaction);
        if let Err(error) = validate_browser_safe_transaction(&transaction) {
            return WorkerResponse::Error { error };
        }
        match session_mut(&mut self.session).and_then(|session| session.edit(&transaction)) {
            Ok((changed, patches, snapshot)) => WorkerResponse::Edited {
                changed,
                patches,
                snapshot,
            },
            Err(error) => WorkerResponse::Error { error },
        }
    }
}

/// Extracts a top-level `request_id` without deserializing an arbitrarily
/// large request. The scanner only retains bounded key and value strings, and
/// recognizes JSON escapes through `serde_json` after it has isolated each
/// small literal. It is solely an error-correlation aid; normal validation is
/// still performed by deserializing `RequestEnvelope` below the raw cap.
fn bounded_request_id(request_json: &str) -> String {
    const MAX_REQUEST_ID_LITERAL_BYTES: usize = 1_024;
    // Oversized inputs must not turn error correlation into another unbounded
    // parser. Canonical browser envelopes place the id immediately after the
    // protocol field, so a small prefix is sufficient for matching every
    // request the bundled client can issue. Direct callers that hide an id
    // after the prefix receive the documented `invalid` fallback.
    const MAX_ID_SCAN_BYTES: usize = 4 * 1024;
    let bytes = &request_json.as_bytes()[..request_json.len().min(MAX_ID_SCAN_BYTES)];
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return "invalid".to_owned();
    }
    index += 1;
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b'}') {
            return "invalid".to_owned();
        }
        let Some((key_end, key)) = bounded_json_string(bytes, index, 128) else {
            return "invalid".to_owned();
        };
        index = skip_json_ws(bytes, key_end);
        if bytes.get(index) != Some(&b':') {
            return "invalid".to_owned();
        }
        index = skip_json_ws(bytes, index + 1);
        if key == "request_id" {
            let Some((_, request_id)) =
                bounded_json_string(bytes, index, MAX_REQUEST_ID_LITERAL_BYTES)
            else {
                return "invalid".to_owned();
            };
            return request_id;
        }
        let Some(value_end) = skip_json_value(bytes, index) else {
            return "invalid".to_owned();
        };
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            _ => return "invalid".to_owned(),
        }
    }
}

fn skip_json_ws(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        index += 1;
    }
    index
}

/// Returns the exclusive end and decoded string for a small JSON literal.
fn bounded_json_string(
    bytes: &[u8],
    start: usize,
    max_literal_bytes: usize,
) -> Option<(usize, String)> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut index = start + 1;
    let mut escaped = false;
    while let Some(byte) = bytes.get(index) {
        if index.saturating_sub(start) > max_literal_bytes {
            return None;
        }
        match (*byte, escaped) {
            (_, true) => escaped = false,
            (b'\\', false) => escaped = true,
            (b'"', false) => {
                let literal = std::str::from_utf8(&bytes[start..=index]).ok()?;
                return serde_json::from_str(literal)
                    .ok()
                    .map(|value| (index + 1, value));
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Skips one syntactically balanced JSON value without allocating it.
fn skip_json_value(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    let mut container_stack = 0_u64;
    let mut depth = 0_u32;
    let mut string = false;
    let mut escaped = false;
    while let Some(byte) = bytes.get(index) {
        if string {
            match (*byte, escaped) {
                (_, true) => escaped = false,
                (b'\\', false) => escaped = true,
                (b'"', false) => string = false,
                _ => {}
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => string = true,
            b'{' | b'[' => {
                if depth == 64 {
                    return None;
                }
                let is_array = *byte == b'[';
                container_stack |= u64::from(is_array) << depth;
                depth += 1;
            }
            b'}' | b']'
                if depth > 0 && (*byte == b']') == ((container_stack >> (depth - 1)) & 1 == 1) =>
            {
                depth -= 1;
                container_stack &= !(1_u64 << depth);
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            b',' | b'}' if depth == 0 => return Some(index),
            byte if byte.is_ascii_whitespace() && depth == 0 => return Some(index),
            _ => {}
        }
        index += 1;
    }
    if !string && depth == 0 && index > start {
        Some(index)
    } else {
        None
    }
}

fn stale_revision(actual: u64, expected: u64) -> WorkerResponse {
    WorkerResponse::Error {
        error: WorkerError::new(
            WorkerErrorCode::StaleRevision,
            format!("request revision {actual} does not match current revision {expected}"),
        ),
    }
}

fn session_ref(session: Option<&WorkbenchSession>) -> Result<&WorkbenchSession, WorkerError> {
    session.ok_or_else(|| WorkerError::new(WorkerErrorCode::Session, "no workbook is open"))
}

fn session_mut(
    session: &mut Option<WorkbenchSession>,
) -> Result<&mut WorkbenchSession, WorkerError> {
    session
        .as_mut()
        .ok_or_else(|| WorkerError::new(WorkerErrorCode::Session, "no workbook is open"))
}

fn serialize_unbounded_error(response: &ResponseEnvelope) -> String {
    serde_json::to_string(response).unwrap_or_else(|error| {
        format!(
            "{{\"protocol\":\"{PROTOCOL_VERSION}\",\"request_id\":\"invalid\",\"revision\":0,\"response\":{{\"kind\":\"error\",\"error\":{{\"code\":\"protocol\",\"message\":{:?},\"diagnostics\":[],\"diagnostics_omitted\":0}}}}}}",
            format!("protocol serialization failed: {error}")
        )
    })
}

fn serialize_response(response: &ResponseEnvelope, max_bytes: usize) -> Result<String, String> {
    let mut writer = BoundedJsonWriter::new(max_bytes);
    serde_json::to_writer(&mut writer, response)
        .map_err(|_| format!("response exceeds the {max_bytes} byte worker limit"))?;
    String::from_utf8(writer.into_bytes()).map_err(|_| "response was not UTF-8".to_owned())
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl BoundedJsonWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(max_bytes.min(16 * 1024)),
            max_bytes,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "response limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PreparedSource {
    workbook: Workbook,
    view: WorkbookView,
    extension_report: ExtensionReport,
    calculation_complete: bool,
    rendering_complete: bool,
    editable: bool,
    persistent_diagnostics: Vec<Diagnostic>,
}

fn prepare_source(source: &[u8], limits: &SessionLimits) -> Result<PreparedSource, WorkerError> {
    validate_diagnostic_budget(source, limits)?;
    let options = extension_parse_options();
    let document = parse_with_options(source, &options);
    if has_unrecoverable_parse_errors(&document) {
        let (diagnostics, diagnostics_omitted) =
            bounded_diagnostics(document.diagnostics, limits.max_diagnostics);
        return Err(WorkerError::with_diagnostics(
            WorkerErrorCode::InvalidSource,
            "source is not a valid Marksheet document",
            diagnostics,
            diagnostics_omitted,
        ));
    }
    let workbook = document.workbook.clone().ok_or_else(|| {
        let (diagnostics, diagnostics_omitted) =
            bounded_diagnostics(document.diagnostics.clone(), limits.max_diagnostics);
        WorkerError::with_diagnostics(
            WorkerErrorCode::InvalidSource,
            "source did not produce a recoverable Marksheet workbook",
            diagnostics,
            diagnostics_omitted,
        )
    })?;
    validate_browser_safe_workbook(&workbook)?;
    let view = WorkbookView::from_document(&document, view_limits(limits)).map_err(|error| {
        let (diagnostics, diagnostics_omitted) =
            bounded_diagnostics(document.diagnostics.clone(), limits.max_diagnostics);
        WorkerError::with_diagnostics(
            WorkerErrorCode::InvalidSource,
            format!("source could not be prepared for sparse presentation: {error}"),
            diagnostics,
            diagnostics_omitted,
        )
    })?;
    let view_summary = view.summary();
    let editable = is_editable(&view_summary.diagnostics);
    let registry = extension_registry();
    let extension_report = registry.validate(&workbook, &extension_limits(limits));
    let persistent_diagnostics = merge_extension_diagnostics(
        view_summary.diagnostics,
        extension_report
            .diagnostics
            .iter()
            .map(|item| item.diagnostic.clone()),
    );
    Ok(PreparedSource {
        workbook,
        view,
        calculation_complete: view_summary.completeness.calculation_complete
            && extension_report.calculation_complete,
        rendering_complete: view_summary.completeness.rendering_complete
            && extension_report.rendering_complete,
        editable,
        extension_report,
        persistent_diagnostics,
    })
}

fn extension_registry() -> ExtensionRegistry<'static> {
    ExtensionRegistry::with_assertions()
}

fn extension_parse_options() -> ParseOptions {
    let registry = extension_registry();
    ParseOptions {
        supported_extensions: registry
            .capabilities()
            .iter()
            .map(extension_id_string)
            .collect(),
    }
}

fn extension_limits(session: &SessionLimits) -> ExtensionLimits {
    let mut limits = ExtensionLimits::default();
    limits.max_payload_bytes = limits.max_payload_bytes.min(session.max_source_bytes);
    limits.calculation = view_limits(session).calculation;
    limits
}

fn has_unrecoverable_parse_errors(document: &ParsedDocument) -> bool {
    document.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.code.as_str() != AVAILABILITY_REQUIRED_DIAGNOSTIC
    })
}

fn merge_extension_diagnostics(
    mut diagnostics: Vec<Diagnostic>,
    extension_diagnostics: impl IntoIterator<Item = Diagnostic>,
) -> Vec<Diagnostic> {
    for diagnostic in extension_diagnostics {
        let duplicate = diagnostics.iter().any(|existing| {
            existing == &diagnostic || equivalent_availability_diagnostic(existing, &diagnostic)
        });
        if !duplicate {
            diagnostics.push(diagnostic);
        }
    }
    diagnostics
}

fn equivalent_availability_diagnostic(left: &Diagnostic, right: &Diagnostic) -> bool {
    let code = left.code.as_str();
    matches!(
        code,
        AVAILABILITY_REQUIRED_DIAGNOSTIC | AVAILABILITY_WARNING_DIAGNOSTIC
    ) && right.code.as_str() == code
        && left.severity == right.severity
        && spans_overlap(left.primary.span, right.primary.span)
}

const fn spans_overlap(left: ByteSpan, right: ByteSpan) -> bool {
    left.start < right.end && right.start < left.end
}

fn view_limits(session: &SessionLimits) -> ViewLimits {
    let mut calculation = CalcLimits::default();
    calculation.work.max_output_cells = session.max_calculation_cells;
    calculation.work.max_graph_nodes = bounded_work_limit(session.max_calculation_cells);
    calculation.work.max_dirty_cells = bounded_work_limit(session.max_calculation_cells);
    calculation.work.max_evaluated_cells = bounded_work_limit(session.max_calculation_cells);
    ViewLimits {
        max_viewport_cells: session.max_viewport_cells,
        max_presented_cells: session.max_presented_cells,
        max_style_regions: session.max_presented_cells,
        max_style_layers_per_cell: 1_000,
        max_style_applications: 1_024,
        calculation,
    }
}

fn bounded_work_limit(cells: u64) -> usize {
    usize::try_from(cells.saturating_mul(10)).unwrap_or(usize::MAX)
}

fn validate_source_size(source: &[u8], limits: &SessionLimits) -> Result<(), WorkerError> {
    if source.len() > limits.max_source_bytes {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!(
                "source has {} bytes, exceeding the {} byte worker limit",
                source.len(),
                limits.max_source_bytes
            ),
        ));
    }
    Ok(())
}

/// Reject diagnostic-amplifying input before syntax and formula preparation
/// allocate one structured diagnostic per malformed record. These limits are
/// intentionally independent from the response cap: callers may request zero
/// diagnostics while opening a valid document with many directives.
fn validate_diagnostic_budget(source: &[u8], _limits: &SessionLimits) -> Result<(), WorkerError> {
    let candidates = source
        .split(|byte| *byte == b'\n')
        .filter(|line| line.first() == Some(&b'@'))
        .count();
    if candidates > MAX_DIAGNOSTIC_CANDIDATES {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!(
                "source has {candidates} directive diagnostic candidates, exceeding the {MAX_DIAGNOSTIC_CANDIDATES} worker limit",
            ),
        ));
    }
    let mut records = usize::from(!source.is_empty());
    let mut formulas = 0_usize;
    let mut delimiters = 0_usize;
    for byte in source {
        match byte {
            b'\n' => records = records.saturating_add(1),
            b'=' => formulas = formulas.saturating_add(1),
            b',' => delimiters = delimiters.saturating_add(1),
            _ => {}
        }
    }
    // A trailing newline terminates the final record rather than beginning a
    // new one.
    if source.ends_with(b"\n") {
        records = records.saturating_sub(1);
    }
    if records > MAX_SOURCE_RECORDS {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!(
                "source has {records} records, exceeding the {MAX_SOURCE_RECORDS} worker limit"
            ),
        ));
    }
    // Formula syntax starts with `=`. The preflight deliberately counts every
    // occurrence, including quoted text and comments: it must not duplicate
    // the parser's outer grammar or let an unmatched quote hide a costly CSV
    // body. This is a conservative source-structure admission limit.
    if formulas > MAX_FORMULA_CANDIDATES {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!(
                "source has {formulas} formula candidates, exceeding the {MAX_FORMULA_CANDIDATES} worker limit"
            ),
        ));
    }
    // Parsing materializes one cell per CSV field even when every field is
    // blank. Bound every comma before lowering so a short one-line CSV record
    // cannot force an unbounded authored-cell allocation. Counting comments
    // and quoted text is intentionally conservative: outer syntax state must
    // not be able to bypass this worker-side resource limit.
    if delimiters > MAX_CSV_FIELD_DELIMITERS {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!(
                "source has {delimiters} CSV field delimiters, exceeding the {MAX_CSV_FIELD_DELIMITERS} worker limit"
            ),
        ));
    }
    Ok(())
}

fn bounded_diagnostics(mut diagnostics: Vec<Diagnostic>, max: usize) -> (Vec<Diagnostic>, usize) {
    let omitted = diagnostics.len().saturating_sub(max);
    diagnostics.truncate(max);
    (diagnostics, omitted)
}

fn validate_area(range: Range, limit: u64, operation: &str) -> Result<(), WorkerError> {
    validate_browser_safe_range(range, operation)?;
    let cells = range
        .width()
        .and_then(|width| {
            range.height().and_then(|height| {
                width
                    .checked_mul(height)
                    .ok_or(marksheet_model::CoordinateError::Overflow)
            })
        })
        .map_err(|error| {
            WorkerError::new(
                WorkerErrorCode::Limit,
                format!("{operation} range is invalid: {error}"),
            )
        })?;
    if cells > limit {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!("{operation} range contains {cells} cells, exceeding the {limit} cell limit"),
        ));
    }
    Ok(())
}

fn validate_browser_safe_coordinate(
    coordinate: Coordinate,
    context: &str,
) -> Result<(), WorkerError> {
    if coordinate.column > MAX_JS_SAFE_INTEGER || coordinate.row > MAX_JS_SAFE_INTEGER {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!(
                "{context} coordinate exceeds JavaScript's maximum safe integer ({MAX_JS_SAFE_INTEGER})"
            ),
        ));
    }
    Ok(())
}

fn validate_browser_safe_range(range: Range, context: &str) -> Result<(), WorkerError> {
    validate_browser_safe_coordinate(range.start, context)?;
    validate_browser_safe_coordinate(range.end, context)
}

/// The JSON protocol intentionally uses JavaScript numbers for coordinates.
/// Admit only the exact IEEE-754 integer domain, before a workbook can be
/// projected into a browser-visible response.
fn validate_browser_safe_workbook(workbook: &Workbook) -> Result<(), WorkerError> {
    for name in &workbook.names {
        match &name.target {
            NameTarget::Cell(cell) => validate_browser_safe_coordinate(cell.coordinate, "name")?,
            NameTarget::Range(range) => validate_browser_safe_range(range.range, "name")?,
            NameTarget::TableColumn { .. } => {}
        }
    }
    for sheet in &workbook.sheets {
        for item in &sheet.items {
            match item {
                SheetItem::Block(block) => validate_browser_safe_range(
                    block
                        .footprint()
                        .map_err(|error| {
                            WorkerError::new(WorkerErrorCode::Limit, error.to_string())
                        })?
                        .range()
                        .map_err(|error| {
                            WorkerError::new(WorkerErrorCode::Limit, error.to_string())
                        })?,
                    "block",
                )?,
                SheetItem::Table(table) => validate_browser_safe_range(
                    table
                        .block
                        .footprint()
                        .map_err(|error| {
                            WorkerError::new(WorkerErrorCode::Limit, error.to_string())
                        })?
                        .range()
                        .map_err(|error| {
                            WorkerError::new(WorkerErrorCode::Limit, error.to_string())
                        })?,
                    "table",
                )?,
                SheetItem::Fill(fill) => {
                    if let FillTarget::Range(range) = fill.target {
                        validate_browser_safe_range(range, "fill")?;
                    }
                }
                SheetItem::Apply(apply) => {
                    if let ApplyTarget::Range(range) = apply.target {
                        validate_browser_safe_range(range, "style application")?;
                    }
                }
                SheetItem::ColumnGeometry(geometry) => {
                    if geometry.columns.start > MAX_JS_SAFE_INTEGER
                        || geometry.columns.end > MAX_JS_SAFE_INTEGER
                    {
                        return Err(WorkerError::new(
                            WorkerErrorCode::Limit,
                            "column geometry exceeds JavaScript's maximum safe integer",
                        ));
                    }
                }
                SheetItem::RowGeometry(geometry) => {
                    if geometry.rows.start > MAX_JS_SAFE_INTEGER
                        || geometry.rows.end > MAX_JS_SAFE_INTEGER
                    {
                        return Err(WorkerError::new(
                            WorkerErrorCode::Limit,
                            "row geometry exceeds JavaScript's maximum safe integer",
                        ));
                    }
                }
                SheetItem::Extension(_) => {}
            }
        }
    }
    Ok(())
}

fn validate_browser_safe_transaction(transaction: &EditTransaction) -> Result<(), WorkerError> {
    for operation in &transaction.operations {
        match operation {
            marksheet_edit::transaction::EditOperation::SetCell { coordinate, .. } => {
                validate_browser_safe_coordinate(*coordinate, "edit")?;
            }
            marksheet_edit::transaction::EditOperation::MoveBlock {
                source,
                destination,
                ..
            } => {
                validate_browser_safe_range(*source, "edit")?;
                validate_browser_safe_coordinate(*destination, "edit")?;
            }
            marksheet_edit::transaction::EditOperation::SetColumnWidth { columns, .. } => {
                if columns.start > MAX_JS_SAFE_INTEGER || columns.end > MAX_JS_SAFE_INTEGER {
                    return Err(WorkerError::new(
                        WorkerErrorCode::Limit,
                        "edit column range exceeds JavaScript's maximum safe integer",
                    ));
                }
            }
            marksheet_edit::transaction::EditOperation::SetRowHeight { rows, .. } => {
                if rows.start > MAX_JS_SAFE_INTEGER || rows.end > MAX_JS_SAFE_INTEGER {
                    return Err(WorkerError::new(
                        WorkerErrorCode::Limit,
                        "edit row range exceeds JavaScript's maximum safe integer",
                    ));
                }
            }
            marksheet_edit::transaction::EditOperation::ApplyStyle { target, .. } => {
                if let ApplyTarget::Range(range) = target {
                    validate_browser_safe_range(*range, "edit")?;
                }
            }
            marksheet_edit::transaction::EditOperation::SetNameTarget { target, .. } => {
                match target {
                    NameTarget::Cell(cell) => {
                        validate_browser_safe_coordinate(cell.coordinate, "edit")?;
                    }
                    NameTarget::Range(range) => validate_browser_safe_range(range.range, "edit")?,
                    NameTarget::TableColumn { .. } => {}
                }
            }
            marksheet_edit::transaction::EditOperation::AppendTableRow { .. }
            | marksheet_edit::transaction::EditOperation::RenameSheetLabel { .. }
            | marksheet_edit::transaction::EditOperation::RenameSheetId { .. }
            | marksheet_edit::transaction::EditOperation::RenameNameId { .. }
            | marksheet_edit::transaction::EditOperation::DefineStyle { .. } => {}
        }
    }
    Ok(())
}

/// Performs binding-local edit admission before the core transaction parses
/// or plans anything. JSON serialization is used only as a deterministic
/// recursive byte meter: it visits every nested string, vector, value, style,
/// name target, and source expectation without retaining a duplicate payload.
fn validate_worker_edit_transaction(
    transaction: &WorkerEditTransaction,
    limits: &SessionLimits,
) -> Result<(), WorkerError> {
    if transaction.operations.len() > MAX_EDIT_OPERATIONS {
        return Err(WorkerError::new(
            WorkerErrorCode::Limit,
            format!(
                "edit has {} operations, exceeding the {MAX_EDIT_OPERATIONS} operation worker limit",
                transaction.operations.len()
            ),
        ));
    }
    if let Some(source) = transaction
        .expectations
        .as_ref()
        .and_then(|expectations| expectations.source.as_ref())
    {
        if source.bytes.len() > limits.max_source_bytes {
            return Err(WorkerError::new(
                WorkerErrorCode::Limit,
                format!(
                    "edit source expectation has {} bytes, exceeding the {} byte worker limit",
                    source.bytes.len(),
                    limits.max_source_bytes
                ),
            ));
        }
    }
    let payload_bytes = bounded_json_payload_bytes(transaction, MAX_EDIT_PAYLOAD_BYTES)
        .ok_or_else(|| {
            WorkerError::new(
                WorkerErrorCode::Limit,
                format!("edit payload exceeds the {MAX_EDIT_PAYLOAD_BYTES} byte worker limit"),
            )
        })?;
    debug_assert!(payload_bytes <= MAX_EDIT_PAYLOAD_BYTES);
    Ok(())
}

fn bounded_json_payload_bytes<T: Serialize>(value: &T, max_bytes: usize) -> Option<usize> {
    let mut writer = CountingWriter::new(max_bytes);
    serde_json::to_writer(&mut writer, value).ok()?;
    Some(writer.len)
}

struct CountingWriter {
    len: usize,
    max_bytes: usize,
}

impl CountingWriter {
    const fn new(max_bytes: usize) -> Self {
        Self { len: 0, max_bytes }
    }
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.len.checked_add(bytes.len()) else {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "payload length overflow",
            ));
        };
        if next > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "payload limit exceeded",
            ));
        }
        self.len = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn parse_sheet_id(id: &str) -> Result<SheetId, WorkerError> {
    SheetId::parse(id).map_err(|error| {
        WorkerError::new(
            WorkerErrorCode::Session,
            format!("invalid sheet id {id:?}: {error}"),
        )
    })
}

fn snapshot(session: &WorkbenchSession) -> WorkbookSnapshot {
    let workbook = &session.workbook;
    let extension_report = &session.extension_report;
    let supported_capabilities = extension_registry()
        .capabilities()
        .iter()
        .map(extension_id_string)
        .collect::<Vec<_>>();
    WorkbookSnapshot {
        revision: session.revision,
        diagnostics: session.diagnostics.clone(),
        diagnostics_omitted: session.diagnostics_omitted,
        // This is derived from the full source/formula diagnostic set before
        // the response cap. Trusted extension validation errors remain
        // editable so a semantic edit can repair them.
        editable: session.editable,
        locale: workbook.settings.locale.clone(),
        timezone: workbook.settings.timezone.clone(),
        formula_profile: workbook.settings.formula_profile.clone(),
        sheets: workbook
            .sheets
            .iter()
            .map(|sheet| SheetSummary {
                id: sheet.id.as_str().to_owned(),
                label: sheet.label.clone(),
                authored_cell_count: authored_cell_count(sheet),
                table_count: sheet
                    .items
                    .iter()
                    .filter(|item| matches!(item, SheetItem::Table(_)))
                    .count(),
            })
            .collect(),
        style_count: workbook.styles.len(),
        names: workbook
            .names
            .iter()
            .map(|name| NameSummary {
                id: name.id.as_str().to_owned(),
                target: name.target.clone(),
                resolved: resolve_name_target(workbook, &name.target),
                source_span: name.origin.map(|origin| origin.span),
            })
            .collect(),
        name_count: workbook.names.len(),
        extension_declarations: workbook
            .extensions
            .iter()
            .map(|declaration| {
                let availability = extension_report
                    .capabilities
                    .iter()
                    .find(|check| check.capability == declaration.capability)
                    .map_or_else(
                        || {
                            if supported_capabilities
                                .contains(&extension_id_string(&declaration.capability))
                            {
                                ExtensionAvailabilitySummary::Available
                            } else if declaration.required {
                                ExtensionAvailabilitySummary::UnavailableRequired
                            } else {
                                ExtensionAvailabilitySummary::UnavailableOptional
                            }
                        },
                        |check| availability_summary(check.availability),
                    );
                ExtensionDeclarationSummary {
                    capability: extension_id_string(&declaration.capability),
                    required: declaration.required,
                    availability,
                    source_span: declaration.origin.map(|origin| origin.span),
                }
            })
            .collect(),
        extension_instances: extension_report
            .instances
            .iter()
            .map(|instance| ExtensionInstanceSummary {
                capability: extension_id_string(&instance.capability),
                name: instance.instance_name.clone(),
                scope: extension_scope_summary(&instance.scope),
                declared: workbook
                    .extensions
                    .iter()
                    .any(|declaration| declaration.capability == instance.capability),
                supported: supported_capabilities
                    .contains(&extension_id_string(&instance.capability)),
                outcome: instance_outcome_summary(instance.outcome),
                source_span: extension_instance_span(workbook, instance),
            })
            .collect(),
        extension_support: ExtensionSupportSummary {
            supported_capabilities,
            capabilities_complete: extension_report.capabilities_complete,
            calculation_complete: session.calculation_complete,
            rendering_complete: session.rendering_complete,
            validation_complete: extension_report.validation_complete,
            valid: extension_report.valid,
        },
    }
}

fn extension_id_string(capability: &ExtensionId) -> String {
    format!("{}@{}", capability.id, capability.major)
}

const fn availability_summary(
    availability: CapabilityAvailability,
) -> ExtensionAvailabilitySummary {
    match availability {
        CapabilityAvailability::Available => ExtensionAvailabilitySummary::Available,
        CapabilityAvailability::UnavailableOptional => {
            ExtensionAvailabilitySummary::UnavailableOptional
        }
        CapabilityAvailability::UnavailableRequired => {
            ExtensionAvailabilitySummary::UnavailableRequired
        }
    }
}

fn extension_scope_summary(scope: &ExtensionScope) -> ExtensionScopeSummary {
    match scope {
        ExtensionScope::Workbook => ExtensionScopeSummary::Workbook,
        ExtensionScope::Sheet(sheet) => ExtensionScopeSummary::Sheet {
            sheet: sheet.as_str().to_owned(),
        },
    }
}

const fn instance_outcome_summary(outcome: InstanceOutcome) -> ExtensionInstanceOutcomeSummary {
    match outcome {
        InstanceOutcome::Processed => ExtensionInstanceOutcomeSummary::Processed,
        InstanceOutcome::SkippedUnavailable => ExtensionInstanceOutcomeSummary::SkippedUnavailable,
        InstanceOutcome::SkippedUndeclared => ExtensionInstanceOutcomeSummary::SkippedUndeclared,
        InstanceOutcome::RejectedDuplicate => ExtensionInstanceOutcomeSummary::RejectedDuplicate,
        InstanceOutcome::RejectedByLimit => ExtensionInstanceOutcomeSummary::RejectedByLimit,
    }
}

fn extension_instance_span(
    workbook: &Workbook,
    report: &marksheet_extensions::InstanceReport,
) -> Option<ByteSpan> {
    match &report.scope {
        ExtensionScope::Workbook => workbook
            .extension_instances
            .iter()
            .find(|extension| {
                extension.capability == report.capability && extension.name == report.instance_name
            })
            .and_then(|extension| extension.origin.map(|origin| origin.span)),
        ExtensionScope::Sheet(sheet_id) => workbook
            .sheets
            .iter()
            .find(|sheet| sheet.id == *sheet_id)
            .and_then(|sheet| {
                sheet.items.iter().find_map(|item| match item {
                    SheetItem::Extension(extension)
                        if extension.capability == report.capability
                            && extension.name == report.instance_name =>
                    {
                        extension.origin.map(|origin| origin.span)
                    }
                    _ => None,
                })
            }),
    }
}

fn resolve_name_target(workbook: &Workbook, target: &NameTarget) -> Option<ResolvedNameTarget> {
    match target {
        NameTarget::Cell(cell) => Some(ResolvedNameTarget {
            sheet: cell.sheet.as_str().to_owned(),
            range: Range::single(cell.coordinate),
        }),
        NameTarget::Range(range) => Some(ResolvedNameTarget {
            sheet: range.sheet.as_str().to_owned(),
            range: range.range,
        }),
        NameTarget::TableColumn { table, header } => workbook.sheets.iter().find_map(|sheet| {
            let SheetItem::Table(table_item) = sheet.items.iter().find(
                |item| matches!(item, SheetItem::Table(candidate) if candidate.id == *table),
            )?
            else {
                return None;
            };
            let column_offset =
                table_item.block.cells.first()?.iter().position(
                    |cell| matches!(&cell.value, Value::Text(value) if value == header),
                )?;
            let data_rows = u64::try_from(table_item.block.cells.len().checked_sub(1)?).ok()?;
            if data_rows == 0 {
                return None;
            }
            let column_offset = u64::try_from(column_offset).ok()?;
            let start = table_item.block.anchor.offset(column_offset, 1).ok()?;
            let end = Coordinate {
                column: start.column,
                row: start.row.checked_add(data_rows.checked_sub(1)?)?,
            };
            Some(ResolvedNameTarget {
                sheet: sheet.id.as_str().to_owned(),
                range: Range::new(start, end),
            })
        }),
    }
}

fn is_editable(diagnostics: &[Diagnostic]) -> bool {
    !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
}

/// Preserves source diagnostics across calculations and appends runtime
/// diagnostics in deterministic first-seen order. A single source issue may
/// be surfaced by the view and calculator, so exact duplicates are removed.
fn merge_diagnostics(persistent: &[Diagnostic], calculation: &[Diagnostic]) -> Vec<Diagnostic> {
    let mut merged = persistent.to_vec();
    for diagnostic in calculation {
        if !merged.contains(diagnostic) {
            merged.push(diagnostic.clone());
        }
    }
    merged
}

fn authored_cell_count(sheet: &Sheet) -> u64 {
    sheet.items.iter().fold(0_u64, |count, item| {
        let cells = match item {
            SheetItem::Block(block) => cell_matrix_len(&block.cells),
            SheetItem::Table(table) => cell_matrix_len(&table.block.cells),
            _ => 0,
        };
        count.saturating_add(cells)
    })
}

fn cell_matrix_len(cells: &[Vec<marksheet_model::Cell>]) -> u64 {
    cells.iter().fold(0_u64, |count, row| {
        count.saturating_add(u64::try_from(row.len()).unwrap_or(u64::MAX))
    })
}

impl fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkerError {}

/// Wasm-compatible façade. Native hosts should generally use `WorkerRuntime`
/// directly; JavaScript calls this façade with JSON strings.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub struct WasmWorkbench {
    runtime: WorkerRuntime,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
impl WasmWorkbench {
    #[cfg_attr(
        target_arch = "wasm32",
        wasm_bindgen::prelude::wasm_bindgen(constructor)
    )]
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime: WorkerRuntime::new(SessionLimits::default()),
        }
    }

    /// Dispatches one JSON request and returns one JSON response.
    #[must_use]
    pub fn dispatch_json(&mut self, request_json: &str) -> String {
        self.runtime.dispatch_json(request_json)
    }
}

impl Default for WasmWorkbench {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_edit::transaction::EditOperation;
    use marksheet_model::{Coordinate, DiagnosticCode, LabeledSpan, NameTarget, Severity, Value};

    const SOURCE: &[u8] =
        b"#!marksheet 0.1\n@sheet budget \"Budget\"\n@block A1 csv\nItem,Amount\nRent,100\n@end\n";

    fn envelope(revision: u64, request: WorkerRequest) -> RequestEnvelope {
        RequestEnvelope {
            protocol: PROTOCOL_VERSION.to_owned(),
            request_id: "request-1".to_owned(),
            revision,
            request,
        }
    }

    #[test]
    fn open_visible_calculate_and_edit_are_batched_and_revisioned() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let opened = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ));
        assert_eq!(opened.revision, 1);
        assert!(matches!(opened.response, WorkerResponse::Opened { .. }));

        let visible = runtime.dispatch(envelope(
            1,
            WorkerRequest::VisibleRegion {
                sheet: "budget".to_owned(),
                range: Range::parse("A1:B2").unwrap(),
            },
        ));
        let WorkerResponse::VisibleRegion { region, .. } = visible.response else {
            panic!("expected visible region");
        };
        assert_eq!(region.cells.len(), 4);
        assert_eq!(region.cells[3].coordinate.to_string(), "B2");

        let edited = runtime.dispatch(envelope(
            1,
            WorkerRequest::Edit {
                transaction: EditTransaction::single(EditOperation::SetCell {
                    sheet: SheetId::parse("budget").unwrap(),
                    coordinate: Coordinate::parse("B2").unwrap(),
                    value: Value::Number(125.0),
                })
                .into(),
            },
        ));
        assert_eq!(edited.revision, 2);
        assert!(matches!(
            edited.response,
            WorkerResponse::Edited { changed: true, .. }
        ));

        let stale = runtime.dispatch(envelope(1, WorkerRequest::WorkbookSnapshot));
        assert!(matches!(
            stale.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::StaleRevision,
                    ..
                }
            }
        ));
    }

    #[test]
    fn json_edit_accepts_omitted_default_expectations() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let opened = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ));
        assert_eq!(opened.revision, 1);

        let request = serde_json::json!({
            "protocol": PROTOCOL_VERSION,
            "request_id": "edit-with-default-expectations",
            "revision": 1,
            "request": {
                "kind": "edit",
                "transaction": {
                    "operations": [{
                        "kind": "set_cell",
                        "sheet": "budget",
                        "coordinate": { "column": 2, "row": 2 },
                        "value": { "kind": "number", "value": 125.0 }
                    }]
                }
            }
        });
        let response: ResponseEnvelope =
            serde_json::from_str(&runtime.dispatch_json(&serde_json::to_string(&request).unwrap()))
                .unwrap();

        assert_eq!(response.revision, 2);
        assert!(matches!(
            response.response,
            WorkerResponse::Edited { changed: true, .. }
        ));
    }

    #[test]
    fn source_limits_and_sparse_limits_are_rejected_without_a_session_mutation() {
        let limits = SessionLimits {
            max_source_bytes: SOURCE.len() - 1,
            ..SessionLimits::default()
        };
        let mut runtime = WorkerRuntime::new(limits);
        let response = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ));
        assert_eq!(response.revision, 0);
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
    }

    #[test]
    fn diagnostic_amplification_is_rejected_before_parser_output_is_serialized() {
        let mut source = b"#!marksheet 0.1\n".to_vec();
        for _ in 0..=MAX_DIAGNOSTIC_CANDIDATES {
            source.extend_from_slice(b"@wat\n");
        }
        source.extend_from_slice(b"@sheet budget \"Budget\"\n");

        let error = WorkbenchSession::open(source, SessionLimits::default()).unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::Limit);
        assert!(error.diagnostics.is_empty());
        assert_eq!(error.diagnostics_omitted, 0);
    }

    #[test]
    fn malformed_formula_records_are_rejected_before_view_preparation() {
        let mut source = b"#!marksheet 0.1\n@sheet budget \"Budget\"\n@block A1 csv\n".to_vec();
        for _ in 0..=MAX_SOURCE_RECORDS {
            source.extend_from_slice(b"=(\n");
        }
        source.extend_from_slice(b"@end\n");

        let error = WorkbenchSession::open(source, SessionLimits::default()).unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::Limit);
        assert!(error.message.contains("records"));
    }

    #[test]
    fn wide_csv_row_is_rejected_before_authored_cells_are_allocated() {
        // An unmatched quote in an outer comment must not hide the CSV body
        // from the worker's structural admission check.
        let mut source =
            b"#!marksheet 0.1\n# \"\n@sheet budget \"Budget\"\n@block A1 csv\n".to_vec();
        source.extend(std::iter::repeat_n(b',', MAX_CSV_FIELD_DELIMITERS + 1));
        source.extend_from_slice(b"\n@end\n");

        let error = WorkbenchSession::open(source, SessionLimits::default()).unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::Limit);
        assert!(error.message.contains("CSV field delimiters"));
    }

    #[test]
    fn diagnostic_budget_allows_small_quoted_commas_and_multiline_fields() {
        let source = b"#!marksheet 0.1\n@sheet budget \"Budget\"\n@block A1 csv\n\"one,two\nthree\",4\n@end\n";
        assert!(validate_diagnostic_budget(source, &SessionLimits::default()).is_ok());
    }

    #[test]
    fn snapshot_reports_diagnostics_omitted_by_the_browser_response_cap() {
        let source = include_bytes!("../../../tests/conformance/valid/all_core.ms");
        let session = WorkbenchSession::open(
            source.to_vec(),
            SessionLimits {
                max_diagnostics: 0,
                ..SessionLimits::default()
            },
        )
        .unwrap();
        let snapshot = session.snapshot();
        assert!(snapshot.diagnostics.is_empty());
        assert!(snapshot.diagnostics_omitted > 0);
    }

    #[test]
    fn worker_responses_cap_view_and_calculation_diagnostics_independently() {
        let source = include_bytes!("../../../tests/conformance/valid/all_core.ms");
        let limits = SessionLimits {
            max_diagnostics: 0,
            ..SessionLimits::default()
        };
        let mut runtime = WorkerRuntime::new(limits);
        let opened = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: source.to_vec(),
            },
        ));
        assert_eq!(opened.revision, 1);

        let visible = runtime.dispatch(envelope(
            1,
            WorkerRequest::VisibleRegion {
                sheet: "summary".to_owned(),
                range: Range::parse("B2").unwrap(),
            },
        ));
        let WorkerResponse::VisibleRegion {
            region,
            diagnostics_omitted,
        } = visible.response
        else {
            panic!("expected visible response");
        };
        assert!(region.diagnostics.is_empty());
        assert_eq!(diagnostics_omitted, 1);

        let calculation = runtime.dispatch(envelope(
            1,
            WorkerRequest::Calculate {
                sheet: "summary".to_owned(),
                range: Range::parse("B2").unwrap(),
            },
        ));
        let WorkerResponse::Calculation {
            calculation,
            diagnostics_omitted,
        } = calculation.response
        else {
            panic!("expected calculation response");
        };
        assert!(calculation.diagnostics.is_empty());
        assert_eq!(diagnostics_omitted, 1);
    }

    #[test]
    fn unsafe_browser_coordinates_are_rejected_before_json_can_round_them() {
        let unsafe_row = MAX_JS_SAFE_INTEGER + 1;
        let source =
            format!("#!marksheet 0.1\n@sheet budget \"Budget\"\n@row {unsafe_row} height=18\n");
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let response_json = runtime.dispatch_json(
            &serde_json::to_string(&envelope(
                0,
                WorkerRequest::Open {
                    source: source.into_bytes(),
                },
            ))
            .unwrap(),
        );
        let response: ResponseEnvelope = serde_json::from_str(&response_json).unwrap();
        assert_eq!(response.revision, 0);
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
        assert!(!response_json.contains(&unsafe_row.to_string()));
    }

    #[test]
    fn json_protocol_round_trip_has_a_stable_envelope() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let request = serde_json::to_string(&envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ))
        .unwrap();
        let response_json = runtime.dispatch_json(&request);
        let response: ResponseEnvelope = serde_json::from_str(&response_json).unwrap();
        assert_eq!(response.protocol, PROTOCOL_VERSION);
        assert_eq!(response.request_id, "request-1");
        assert_eq!(response.revision, 1);
    }

    #[test]
    fn json_dispatch_returns_a_limit_error_when_a_response_exceeds_its_budget() {
        let mut runtime = WorkerRuntime::new(SessionLimits {
            max_response_bytes: 1,
            ..SessionLimits::default()
        });
        let response: ResponseEnvelope = serde_json::from_str(
            &runtime.dispatch_json(
                &serde_json::to_string(&envelope(
                    0,
                    WorkerRequest::Open {
                        source: SOURCE.to_vec(),
                    },
                ))
                .unwrap(),
            ),
        )
        .unwrap();
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
    }

    #[test]
    fn default_response_budget_covers_maximum_lossless_source_bytes() {
        let response = ResponseEnvelope {
            protocol: PROTOCOL_VERSION.to_owned(),
            request_id: "max-source".to_owned(),
            revision: 1,
            response: WorkerResponse::SourceBytes {
                source: vec![255; SessionLimits::default().max_source_bytes],
            },
        };
        assert!(serialize_response(&response, SessionLimits::default().max_response_bytes).is_ok());
    }

    #[test]
    fn malformed_json_request_preserves_request_identity_for_client_rejection() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let response: ResponseEnvelope = serde_json::from_str(&runtime.dispatch_json(
            r#"{"protocol":"marksheet-worker@1","request_id":"malformed-17","revision":0,"request":{"kind":"not_real"}}"#,
        ))
        .unwrap();
        assert_eq!(response.request_id, "malformed-17");
        assert_eq!(response.revision, 0);
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Protocol,
                    ..
                }
            }
        ));
    }

    #[test]
    fn oversized_raw_json_is_rejected_before_deserialization_with_a_correlated_id() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let opened = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ));
        assert_eq!(opened.revision, 1);

        let mut request = format!(
            r#"{{"protocol":"{PROTOCOL_VERSION}","request_id":"oversized-edit","revision":1,"padding":""#
        );
        request.extend(std::iter::repeat_n('x', MAX_REQUEST_JSON_BYTES));
        request.push_str("\"}");
        let response: ResponseEnvelope =
            serde_json::from_str(&runtime.dispatch_json(&request)).unwrap();
        assert_eq!(response.request_id, "oversized-edit");
        assert_eq!(response.revision, 1);
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
        assert_eq!(runtime.session.as_ref().unwrap().source_bytes(), SOURCE);
    }

    #[test]
    fn oversized_raw_json_id_scan_is_prefix_and_depth_bounded() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let mut request = String::from(r#"{"padding":["#);
        request.extend(std::iter::repeat_n('[', 10_000));
        request.push_str(r#"0,"request_id":"hidden-after-prefix""#);
        request.extend(std::iter::repeat_n(']', 10_000));
        request.extend(std::iter::repeat_n('x', MAX_REQUEST_JSON_BYTES));
        request.push('}');
        let response: ResponseEnvelope =
            serde_json::from_str(&runtime.dispatch_json(&request)).unwrap();
        assert_eq!(response.request_id, "invalid");
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
    }

    #[test]
    fn edit_admission_limits_are_atomic_before_core_planning() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let _ = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ));

        let too_many = WorkerEditTransaction {
            operations: vec![
                EditOperation::RenameSheetLabel {
                    sheet: SheetId::parse("budget").unwrap(),
                    label: "Budget".to_owned(),
                };
                MAX_EDIT_OPERATIONS + 1
            ],
            expectations: None,
        };
        let response = runtime.dispatch(envelope(
            1,
            WorkerRequest::Edit {
                transaction: too_many,
            },
        ));
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
        assert_eq!(runtime.session.as_ref().unwrap().revision(), 1);

        let oversized_payload = WorkerEditTransaction {
            operations: vec![EditOperation::SetCell {
                sheet: SheetId::parse("budget").unwrap(),
                coordinate: Coordinate::parse("A1").unwrap(),
                value: Value::Text("x".repeat(MAX_EDIT_PAYLOAD_BYTES)),
            }],
            expectations: None,
        };
        let response = runtime.dispatch(envelope(
            1,
            WorkerRequest::Edit {
                transaction: oversized_payload,
            },
        ));
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
        assert_eq!(runtime.session.as_ref().unwrap().revision(), 1);

        let oversized_expectation = WorkerEditTransaction {
            operations: Vec::new(),
            expectations: Some(WorkerEditExpectations {
                source: Some(WorkerSourceExpectation {
                    bytes: vec![0; SessionLimits::default().max_source_bytes + 1],
                }),
            }),
        };
        let response = runtime.dispatch(envelope(
            1,
            WorkerRequest::Edit {
                transaction: oversized_expectation,
            },
        ));
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Limit,
                    ..
                }
            }
        ));
        assert_eq!(runtime.session.as_ref().unwrap().revision(), 1);
        assert_eq!(runtime.session.as_ref().unwrap().source_bytes(), SOURCE);
    }

    #[test]
    fn browser_source_expectations_use_exact_bytes_without_u64_rounding() {
        let fingerprint = SourceExpectation::capture(SOURCE).fingerprint;
        assert!(fingerprint.fnv1a64 > MAX_JS_SAFE_INTEGER);
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let _ = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ));
        let transaction = WorkerEditTransaction {
            operations: vec![EditOperation::SetCell {
                sheet: SheetId::parse("budget").unwrap(),
                coordinate: Coordinate::parse("B2").unwrap(),
                value: Value::Number(125.0),
            }],
            expectations: Some(WorkerEditExpectations {
                source: Some(WorkerSourceExpectation {
                    bytes: SOURCE.to_vec(),
                }),
            }),
        };
        let response = runtime.dispatch(envelope(1, WorkerRequest::Edit { transaction }));
        assert!(matches!(
            response.response,
            WorkerResponse::Edited { changed: true, .. }
        ));
        assert_eq!(response.revision, 2);

        let stale = WorkerEditTransaction {
            operations: vec![EditOperation::SetCell {
                sheet: SheetId::parse("budget").unwrap(),
                coordinate: Coordinate::parse("B2").unwrap(),
                value: Value::Number(130.0),
            }],
            expectations: Some(WorkerEditExpectations {
                source: Some(WorkerSourceExpectation {
                    bytes: SOURCE.to_vec(),
                }),
            }),
        };
        let response = runtime.dispatch(envelope(2, WorkerRequest::Edit { transaction: stale }));
        assert!(matches!(
            response.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Edit,
                    ..
                }
            }
        ));
        assert_eq!(response.revision, 2);
    }

    #[test]
    fn snapshot_editability_uses_full_persistent_diagnostics_not_the_capped_prefix() {
        let mut session =
            WorkbenchSession::open(SOURCE.to_vec(), SessionLimits::default()).unwrap();
        let warning = Diagnostic {
            code: DiagnosticCode::new("MS9001").unwrap(),
            severity: Severity::Warning,
            message: "visible warning".to_owned(),
            primary: LabeledSpan {
                span: ByteSpan::default(),
                label: None,
            },
            related: Vec::new(),
            context: None,
            suggestion: None,
        };
        let error = Diagnostic {
            code: DiagnosticCode::new("MS9002").unwrap(),
            severity: Severity::Error,
            message: "late persistent error".to_owned(),
            primary: LabeledSpan {
                span: ByteSpan::default(),
                label: None,
            },
            related: Vec::new(),
            context: None,
            suggestion: None,
        };
        session.persistent_diagnostics = vec![warning.clone(), error];
        session.diagnostics = vec![warning];
        session.diagnostics_omitted = 1;
        session.editable = is_editable(&session.persistent_diagnostics);

        let snapshot = session.snapshot();
        assert_eq!(snapshot.diagnostics.len(), 1);
        assert_eq!(snapshot.diagnostics_omitted, 1);
        assert!(!snapshot.editable);
    }

    #[test]
    fn date_and_datetime_wire_values_are_iso_strings_with_an_offset() {
        let source = b"#!marksheet 0.1\n@sheet dates \"Dates\"\n@block A1 csv\n2026-08-16,2026-08-16T10:30:00-04:00\n@end\n";
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let _ = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: source.to_vec(),
            },
        ));
        let response: serde_json::Value = serde_json::from_str(
            &runtime.dispatch_json(
                &serde_json::to_string(&envelope(
                    1,
                    WorkerRequest::VisibleRegion {
                        sheet: "dates".to_owned(),
                        range: Range::parse("A1:B1").unwrap(),
                    },
                ))
                .unwrap(),
            ),
        )
        .unwrap();
        let cells = response["response"]["region"]["cells"].as_array().unwrap();
        assert_eq!(cells[0]["source"]["Authored"]["value"]["kind"], "date");
        assert_eq!(
            cells[0]["source"]["Authored"]["value"]["value"],
            "2026-08-16"
        );
        assert_eq!(cells[1]["source"]["Authored"]["value"]["kind"], "date_time");
        assert_eq!(
            cells[1]["source"]["Authored"]["value"]["value"],
            "2026-08-16T10:30:00-04:00"
        );
    }

    #[test]
    fn second_raw_open_does_not_reset_an_active_runtime_revision() {
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let first = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: SOURCE.to_vec(),
            },
        ));
        assert_eq!(first.revision, 1);
        let second = runtime.dispatch(envelope(
            1,
            WorkerRequest::Open {
                source: b"#!marksheet 0.1\n@sheet replacement \"Replacement\"\n".to_vec(),
            },
        ));
        assert_eq!(second.revision, 1);
        assert!(matches!(
            second.response,
            WorkerResponse::Error {
                error: WorkerError {
                    code: WorkerErrorCode::Protocol,
                    ..
                }
            }
        ));
        let source = runtime.dispatch(envelope(1, WorkerRequest::SourceBytes));
        assert!(
            matches!(source.response, WorkerResponse::SourceBytes { source } if source == SOURCE)
        );
    }

    #[test]
    fn edit_rejects_growth_over_the_session_source_limit_atomically() {
        let source = b"#!marksheet 0.1\n@sheet budget \"Budget\"\n@block A1 csv\na\n@end\n";
        let mut session = WorkbenchSession::open(
            source.to_vec(),
            SessionLimits {
                max_source_bytes: source.len() + 1,
                ..SessionLimits::default()
            },
        )
        .unwrap();
        let error = session
            .edit(&EditTransaction::single(EditOperation::SetCell {
                sheet: SheetId::parse("budget").unwrap(),
                coordinate: Coordinate::parse("A1").unwrap(),
                value: Value::Text("source growth exceeds the configured limit".to_owned()),
            }))
            .unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::Limit);
        assert_eq!(session.revision(), 1);
        assert_eq!(session.source_bytes(), source);
    }

    #[test]
    fn calculation_keeps_source_diagnostics_after_preparation() {
        let source = include_bytes!("../../../tests/conformance/valid/all_core.ms");
        let mut session =
            WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        assert!(
            session
                .snapshot()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS3102")
        );
        let calculation = session
            .calculate("summary", Range::parse("B2").unwrap())
            .unwrap();
        assert!(
            calculation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS3102")
        );
        assert!(
            session
                .snapshot()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS3102")
        );
    }

    #[test]
    fn fills_are_projected_as_virtual_without_dense_expansion() {
        let source = b"#!marksheet 0.1\n@sheet budget \"Budget\"\n@block A1 csv\nBase,Result\n1,\n@end\n@fill B2 =A2*2\n";
        let mut runtime = WorkerRuntime::new(SessionLimits::default());
        let _ = runtime.dispatch(envelope(
            0,
            WorkerRequest::Open {
                source: source.to_vec(),
            },
        ));
        let response = runtime.dispatch(envelope(
            1,
            WorkerRequest::VisibleRegion {
                sheet: "budget".to_owned(),
                range: Range::parse("A1:B2").unwrap(),
            },
        ));
        let WorkerResponse::VisibleRegion { region, .. } = response.response else {
            panic!("expected region");
        };
        assert_eq!(region.cells.len(), 4);
        assert!(matches!(
            region.cells[3].source,
            marksheet_view::CellSource::VirtualFill { .. }
        ));
    }

    #[test]
    fn snapshot_exposes_typed_workbook_names_for_name_box_resolution() {
        let source = b"#!marksheet 0.1\n@name selected = budget!A1\n@sheet budget \"Budget\"\n@block A1 csv\n42\n@end\n";
        let session = WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        let snapshot = session.snapshot();
        assert_eq!(snapshot.name_count, 1);
        let name = snapshot.names.first().expect("one declared name");
        assert_eq!(name.id, "selected");
        assert!(matches!(
            &name.target,
            NameTarget::Cell(cell)
                if cell.sheet.as_str() == "budget" && cell.coordinate == Coordinate::parse("A1").unwrap()
        ));
        assert_eq!(
            name.resolved.as_ref().map(|target| target.range),
            Some(Range::parse("A1").unwrap())
        );
        assert!(name.source_span.is_some());

        let table_source = b"#!marksheet 0.1\n@name amounts = ledger[Amount]\n@sheet budget \"Budget\"\n@table ledger A1 csv\nAmount\n20\n@end\n";
        let table_session =
            WorkbenchSession::open(table_source.to_vec(), SessionLimits::default()).unwrap();
        let table_name = table_session
            .snapshot()
            .names
            .into_iter()
            .next()
            .expect("one table-column name");
        assert!(matches!(table_name.target, NameTarget::TableColumn { .. }));
        assert_eq!(
            table_name.resolved,
            Some(ResolvedNameTarget {
                sheet: "budget".to_owned(),
                range: Range::parse("A2").unwrap(),
            })
        );
    }

    fn diagnostic_codes(snapshot: &WorkbookSnapshot) -> Vec<&str> {
        snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect()
    }

    #[test]
    fn trusted_assertions_execute_and_are_exposed_without_payloads() {
        let source = b"#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\nValue\n2\n@end\n@extension assertions@1 \"checks\"\nassert A2 = 2\n@end\n";
        let session = WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        let snapshot = session.snapshot();

        assert!(snapshot.extension_support.capabilities_complete);
        assert!(snapshot.extension_support.calculation_complete);
        assert!(snapshot.extension_support.rendering_complete);
        assert!(snapshot.extension_support.validation_complete);
        assert!(snapshot.extension_support.valid);
        assert_eq!(
            snapshot.extension_support.supported_capabilities,
            ["assertions@1"]
        );
        assert_eq!(snapshot.extension_declarations.len(), 1);
        assert_eq!(
            snapshot.extension_declarations[0].availability,
            ExtensionAvailabilitySummary::Available
        );
        assert_eq!(snapshot.extension_instances.len(), 1);
        assert_eq!(
            snapshot.extension_instances[0].outcome,
            ExtensionInstanceOutcomeSummary::Processed
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("assert A2 = 2"));
    }

    #[test]
    fn failed_and_malformed_assertions_return_structured_host_diagnostics() {
        for (payload, expected) in [("assert A2 = 3", "MS3201"), ("assert A2 == 2", "MS3202")] {
            let source = format!(
                "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\nValue\n2\n@end\n@extension assertions@1 \"checks\"\n{payload}\n@end\n"
            );
            let session =
                WorkbenchSession::open(source.into_bytes(), SessionLimits::default()).unwrap();
            let snapshot = session.snapshot();
            assert_eq!(diagnostic_codes(&snapshot), [expected]);
            assert!(snapshot.editable);
            assert!(snapshot.extension_support.calculation_complete);
            assert!(snapshot.extension_support.rendering_complete);
            assert!(!snapshot.extension_support.valid);
        }
    }

    #[test]
    fn optional_exact_major_mismatch_warns_without_incomplete_core_claims() {
        let source = b"#!marksheet 0.1\n@use assertions@2\n@sheet s \"S\"\n";
        let session = WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        let snapshot = session.snapshot();

        assert_eq!(diagnostic_codes(&snapshot), ["MS3102"]);
        assert!(snapshot.editable);
        assert!(snapshot.extension_support.capabilities_complete);
        assert!(snapshot.extension_support.calculation_complete);
        assert!(snapshot.extension_support.rendering_complete);
        assert_eq!(
            snapshot.extension_declarations[0].availability,
            ExtensionAvailabilitySummary::UnavailableOptional
        );
    }

    #[test]
    fn required_exact_major_mismatch_is_recoverable_but_never_calculated_as_complete() {
        let source =
            b"#!marksheet 0.1\n@require assertions@2\n@sheet s \"S\"\n@block A1 csv\n=1+1\n@end\n";
        let mut session =
            WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        let snapshot = session.snapshot();

        assert_eq!(diagnostic_codes(&snapshot), ["MS3101"]);
        assert!(!snapshot.editable);
        assert!(!snapshot.extension_support.capabilities_complete);
        assert!(!snapshot.extension_support.calculation_complete);
        assert!(!snapshot.extension_support.rendering_complete);
        assert_eq!(
            snapshot.extension_declarations[0].availability,
            ExtensionAvailabilitySummary::UnavailableRequired
        );

        let error = session
            .calculate("s", Range::parse("A1").unwrap())
            .unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::Calculation);
        let region = session
            .visible_region("s", Range::parse("A1").unwrap())
            .unwrap();
        assert!(!region.completeness.calculation_complete);
        assert!(!region.completeness.rendering_complete);
        assert!(region.cells[0].calculated.is_none());
    }

    #[test]
    fn undeclared_opaque_instance_is_preserved_as_warning_and_complete() {
        let source = b"#!marksheet 0.1\n@extension vendor_data@1 \"secret\"\nopaque-private-payload\n@end\n@sheet s \"S\"\n";
        let session = WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        let snapshot = session.snapshot();

        assert_eq!(diagnostic_codes(&snapshot), ["MS3103"]);
        assert!(snapshot.editable);
        assert!(snapshot.extension_support.capabilities_complete);
        assert!(snapshot.extension_support.calculation_complete);
        assert!(snapshot.extension_support.rendering_complete);
        assert_eq!(snapshot.extension_instances.len(), 1);
        assert!(!snapshot.extension_instances[0].declared);
        assert!(!snapshot.extension_instances[0].supported);
        assert_eq!(
            snapshot.extension_instances[0].outcome,
            ExtensionInstanceOutcomeSummary::SkippedUndeclared
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("opaque-private-payload"));
    }

    #[test]
    fn edit_reparses_with_installed_exact_extensions_and_commits_atomically() {
        let source = b"#!marksheet 0.1\n@require assertions@1\n@sheet s \"S\"\n@block A1 csv\nValue\n2\n@end\n@extension assertions@1 \"checks\"\nassert A2 = 2\n@end\n";
        let mut session =
            WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        let (changed, _, snapshot) = session
            .edit(&EditTransaction::single(EditOperation::RenameSheetLabel {
                sheet: SheetId::parse("s").unwrap(),
                label: "Renamed".to_owned(),
            }))
            .unwrap();

        assert!(changed);
        assert_eq!(session.revision(), 2);
        assert_eq!(snapshot.sheets[0].label, "Renamed");
        assert!(snapshot.extension_support.valid);
        assert!(diagnostic_codes(&snapshot).is_empty());
    }

    #[test]
    fn assertion_failure_remains_editable_and_can_be_repaired() {
        let source = b"#!marksheet 0.1\n@require assertions@1\n@sheet s \"S\"\n@block A1 csv\nValue\n2\n@end\n@extension assertions@1 \"checks\"\nassert A2 = 3\n@end\n";
        let mut session =
            WorkbenchSession::open(source.to_vec(), SessionLimits::default()).unwrap();
        assert!(session.snapshot().editable);
        assert_eq!(diagnostic_codes(&session.snapshot()), ["MS3201"]);
        let region = session
            .visible_region("s", Range::parse("A2").unwrap())
            .unwrap();
        assert!(region.completeness.calculation_complete);
        assert!(region.completeness.rendering_complete);
        assert!(
            region
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS3201")
        );
        let calculation = session.calculate("s", Range::parse("A2").unwrap()).unwrap();
        assert_eq!(calculation.cells.len(), 1);
        assert!(
            calculation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS3201")
        );

        let (_, _, snapshot) = session
            .edit(&EditTransaction::single(EditOperation::SetCell {
                sheet: SheetId::parse("s").unwrap(),
                coordinate: Coordinate::parse("A2").unwrap(),
                value: Value::Number(3.0),
            }))
            .unwrap();
        assert!(snapshot.editable);
        assert!(snapshot.extension_support.valid);
        assert!(diagnostic_codes(&snapshot).is_empty());
    }
}
