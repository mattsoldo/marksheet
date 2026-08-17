use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use marksheet_calc::{
    CalcEngine, CalcLimits, CalculationRequest, PreparedCalculation, ReferenceCalcEngine,
    eval::{CalcValue, EvaluationLimits},
    formula::ParseLimits,
};
use marksheet_model::{
    ByteSpan, Coordinate, Diagnostic, DiagnosticCode, DiagnosticContext, Extension, ExtensionId,
    LabeledSpan, Range, Severity, SheetId, SheetItem, Workbook,
};

/// Required extension declared by the workbook is unavailable.
pub const AVAILABILITY_REQUIRED_DIAGNOSTIC: &str = "MS3101";
/// Optional extension declared by the workbook is unavailable.
pub const AVAILABILITY_WARNING_DIAGNOSTIC: &str = "MS3102";
/// An extension instance has no matching exact declaration.
pub const UNDECLARED_INSTANCE_DIAGNOSTIC: &str = "MS3103";
/// An extension payload or validation result was rejected.
pub const VALIDATION_DIAGNOSTIC: &str = "MS3110";
/// Extension-host resource bounds prevented complete validation.
pub const RESOURCE_LIMIT_DIAGNOSTIC: &str = "MS3111";

/// Host-controlled resource limits for one workbook validation run.
///
/// Payload byte/line limits apply to each instance. Assertion, diagnostic, and
/// work limits apply to the complete run, so splitting an attack across many
/// extension blocks does not bypass them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionLimits {
    pub max_payload_bytes: usize,
    pub max_lines: usize,
    /// Maximum `@use`/`@require` declarations admitted for resolution.
    pub max_declarations: usize,
    /// Maximum opaque instances admitted across all workbook and sheet scopes.
    pub max_instances: usize,
    /// Maximum trusted plugin calls across the complete validation run.
    pub max_invocations: usize,
    pub max_targets: usize,
    /// Maximum cells addressed by one target. Draft 0.1 assertions require
    /// this to permit their single concrete cell.
    pub max_target_area: u64,
    pub formula_parse: ParseLimits,
    pub formula_evaluation: EvaluationLimits,
    /// Calculator preparation, graph, dependency, and output bounds.
    pub calculation: CalcLimits,
    /// Aggregate retained plugin-diagnostic budget. Mandatory host capability
    /// and resource diagnostics remain visible even when this is zero.
    pub max_diagnostics: usize,
    /// Maximum UTF-8 bytes retained for one plugin diagnostic's message and
    /// subcode combined. Oversized output becomes a bounded limit diagnostic.
    pub max_diagnostic_message_bytes: usize,
    /// Aggregate deterministic units reported by plugins. The built-in
    /// assertions extension counts payload bytes, physical lines, targets,
    /// shared workbook preparation, evaluation steps, range cells, and
    /// produced text bytes.
    pub max_work_units: usize,
}

impl Default for ExtensionLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: 256 * 1024,
            max_lines: 10_000,
            max_declarations: 10_000,
            max_instances: 10_000,
            max_invocations: 10_000,
            max_targets: 10_000,
            max_target_area: 1,
            formula_parse: ParseLimits::default(),
            formula_evaluation: EvaluationLimits::default(),
            calculation: CalcLimits::default(),
            max_diagnostics: 1_000,
            max_diagnostic_message_bytes: 16 * 1024,
            max_work_units: 10_000_000,
        }
    }
}

/// Workbook or sheet placement of an opaque extension instance.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExtensionScope {
    Workbook,
    Sheet(SheetId),
}

/// Borrowed scope passed to a plugin without granting mutable workbook access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionScopeRef<'a> {
    Workbook,
    Sheet(&'a SheetId),
}

impl<'a> ExtensionScopeRef<'a> {
    #[must_use]
    pub fn to_owned(self) -> ExtensionScope {
        match self {
            Self::Workbook => ExtensionScope::Workbook,
            Self::Sheet(sheet) => ExtensionScope::Sheet(sheet.clone()),
        }
    }

    #[must_use]
    pub const fn sheet(self) -> Option<&'a SheetId> {
        match self {
            Self::Workbook => None,
            Self::Sheet(sheet) => Some(sheet),
        }
    }
}

/// Exact opaque bytes and source metadata presented to a trusted plugin.
///
/// Plugins report spans relative to `payload`. The host validates and maps
/// those spans; plugins never construct unchecked absolute source locations.
#[derive(Clone, Copy, Debug)]
pub struct OpaqueExtensionInput<'a> {
    pub capability: &'a ExtensionId,
    pub instance_name: &'a str,
    pub scope: ExtensionScopeRef<'a>,
    pub extension_span: Option<ByteSpan>,
    pub payload_span: Option<ByteSpan>,
    pub payload: &'a [u8],
}

/// Read-only facilities available to an extension implementation.
///
/// There are deliberately no I/O, clock, RNG, installation, subprocess, or
/// network handles in this context.
#[derive(Clone, Copy, Debug)]
pub struct PluginContext<'a> {
    pub workbook: &'a Workbook,
    pub limits: &'a ExtensionLimits,
    calculation: &'a RefCell<CalculationCache>,
}

impl PluginContext<'_> {
    /// Returns one typed calculated value through a cache shared by every
    /// extension instance in this validation run. The workbook is prepared at
    /// most once, preventing instance count from multiplying compile work.
    /// The host meters this operation directly; plugins must not report it in
    /// [`PluginResult::work`].
    #[must_use]
    pub fn calculated_cell(&self, sheet: &SheetId, coordinate: Coordinate) -> CalculatedLookup {
        self.calculation
            .borrow_mut()
            .cell(self.workbook, self.limits, sheet, coordinate)
    }
}

/// One host-metered, bounded calculated-cell lookup through [`PluginContext`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalculatedLookup {
    pub value: Option<CalcValue>,
    pub resource_limited: bool,
}

/// Broad host-owned classification used to choose a stable diagnostic code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginDiagnosticKind {
    Rejected,
    ValidationFailure,
    ResourceLimit,
}

/// A plugin diagnostic before its payload-relative span is validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginDiagnostic {
    pub kind: PluginDiagnosticKind,
    pub code: DiagnosticCode,
    /// Stable plugin-specific detail such as `assertions.failed`.
    pub subcode: String,
    pub severity: Severity,
    pub message: String,
    pub local_span: ByteSpan,
    pub payload_line: Option<u64>,
    pub context: Option<DiagnosticContext>,
}

impl PluginDiagnostic {
    #[must_use]
    pub fn rejected(
        subcode: impl Into<String>,
        message: impl Into<String>,
        local_span: ByteSpan,
    ) -> Self {
        Self {
            kind: PluginDiagnosticKind::Rejected,
            code: host_code(VALIDATION_DIAGNOSTIC),
            subcode: subcode.into(),
            severity: Severity::Error,
            message: message.into(),
            local_span,
            payload_line: None,
            context: None,
        }
    }

    #[must_use]
    pub fn validation_failure(
        subcode: impl Into<String>,
        message: impl Into<String>,
        local_span: ByteSpan,
    ) -> Self {
        Self {
            kind: PluginDiagnosticKind::ValidationFailure,
            code: host_code(VALIDATION_DIAGNOSTIC),
            subcode: subcode.into(),
            severity: Severity::Error,
            message: message.into(),
            local_span,
            payload_line: None,
            context: None,
        }
    }

    #[must_use]
    pub fn limit(
        subcode: impl Into<String>,
        message: impl Into<String>,
        local_span: ByteSpan,
    ) -> Self {
        Self {
            kind: PluginDiagnosticKind::ResourceLimit,
            code: host_code(RESOURCE_LIMIT_DIAGNOSTIC),
            subcode: subcode.into(),
            severity: Severity::Error,
            message: message.into(),
            local_span,
            payload_line: None,
            context: None,
        }
    }

    #[must_use]
    pub const fn with_payload_line(mut self, line: u64) -> Self {
        self.payload_line = Some(line);
        self
    }

    #[must_use]
    pub fn with_context(mut self, context: DiagnosticContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Replaces the generic host code with a plugin-owned stable code.
    #[must_use]
    pub fn with_code(mut self, code: DiagnosticCode) -> Self {
        self.code = code;
        self
    }
}

/// Auditable deterministic work counters reported by a plugin.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExtensionWork {
    pub payload_bytes: usize,
    pub payload_lines: usize,
    pub targets: usize,
    /// Number of lazy workbook preparations, not an estimate of their cost.
    /// Preparation itself remains bounded by [`ExtensionLimits::calculation`].
    pub preparations: usize,
    pub evaluation_steps: usize,
    pub range_cells: usize,
    pub text_bytes: usize,
}

impl ExtensionWork {
    #[must_use]
    pub fn total_units(self) -> usize {
        self.payload_bytes
            .saturating_add(self.payload_lines)
            .saturating_add(self.targets)
            .saturating_add(self.preparations)
            .saturating_add(self.evaluation_steps)
            .saturating_add(self.range_cells)
            .saturating_add(self.text_bytes)
    }

    fn saturating_add(&mut self, other: Self) {
        self.payload_bytes = self.payload_bytes.saturating_add(other.payload_bytes);
        self.payload_lines = self.payload_lines.saturating_add(other.payload_lines);
        self.targets = self.targets.saturating_add(other.targets);
        self.preparations = self.preparations.saturating_add(other.preparations);
        self.evaluation_steps = self.evaluation_steps.saturating_add(other.evaluation_steps);
        self.range_cells = self.range_cells.saturating_add(other.range_cells);
        self.text_bytes = self.text_bytes.saturating_add(other.text_bytes);
    }
}

#[derive(Debug, Default)]
struct CalculationCache {
    state: CalculationState,
    pending_work: ExtensionWork,
    work_exhausted: bool,
    limited_cells: BTreeMap<(SheetId, Coordinate), CalculatedLookup>,
}

#[derive(Debug, Default)]
enum CalculationState {
    #[default]
    Unprepared,
    Ready(Box<PreparedCalculation>),
    Unavailable {
        resource_limited: bool,
    },
}

impl CalculationCache {
    fn cell(
        &mut self,
        workbook: &Workbook,
        limits: &ExtensionLimits,
        sheet: &SheetId,
        coordinate: Coordinate,
    ) -> CalculatedLookup {
        let cache_key = (sheet.clone(), coordinate);
        if let Some(cached) = self.limited_cells.get(&cache_key) {
            return cached.clone();
        }
        if self.work_exhausted {
            return CalculatedLookup {
                value: None,
                resource_limited: true,
            };
        }
        let mut prepared_now = false;
        if matches!(self.state, CalculationState::Unprepared) {
            prepared_now = true;
            let engine = ReferenceCalcEngine::new();
            let report = engine.prepare(workbook, extension_calc_limits(limits));
            self.state = match report.calculation {
                Some(calculation) => CalculationState::Ready(Box::new(calculation)),
                None => CalculationState::Unavailable {
                    resource_limited: report
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.code.as_str() == "MS2901"),
                },
            };
        }
        let (mut lookup, lookup_work) = match &mut self.state {
            CalculationState::Ready(calculation) => {
                let result = ReferenceCalcEngine::new().calculate(
                    calculation,
                    &CalculationRequest::new(sheet.clone(), Range::single(coordinate)),
                );
                (
                    CalculatedLookup {
                        value: result.cells.first().map(|cell| cell.value.clone()),
                        resource_limited: result
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.code.as_str() == "MS2901"),
                    },
                    ExtensionWork {
                        preparations: usize::from(prepared_now),
                        evaluation_steps: result.stats.evaluation_steps,
                        range_cells: result.stats.range_cells,
                        text_bytes: result.stats.text_bytes,
                        ..ExtensionWork::default()
                    },
                )
            }
            CalculationState::Unavailable { resource_limited } => (
                CalculatedLookup {
                    value: None,
                    resource_limited: *resource_limited,
                },
                ExtensionWork {
                    preparations: usize::from(prepared_now),
                    ..ExtensionWork::default()
                },
            ),
            CalculationState::Unprepared => unreachable!("calculation cache was initialized"),
        };
        self.pending_work.saturating_add(lookup_work);
        if self.pending_work.total_units() > limits.max_work_units {
            self.work_exhausted = true;
            lookup.value = None;
            lookup.resource_limited = true;
        }
        if lookup.resource_limited {
            self.limited_cells.insert(cache_key, lookup.clone());
        }
        lookup
    }

    fn take_work(&mut self) -> ExtensionWork {
        std::mem::take(&mut self.pending_work)
    }
}

fn extension_calc_limits(limits: &ExtensionLimits) -> CalcLimits {
    let mut calculation = limits.calculation.clone();
    calculation.compile.parse = limits.formula_parse.clone();
    calculation.evaluation = limits.formula_evaluation.clone();
    calculation.work.max_output_cells = calculation.work.max_output_cells.min(1);
    calculation.work.max_evaluated_cells = calculation
        .work
        .max_evaluated_cells
        .min(limits.max_work_units);
    calculation.evaluation.max_steps = calculation.evaluation.max_steps.min(limits.max_work_units);
    calculation.evaluation.max_range_cells = calculation
        .evaluation
        .max_range_cells
        .min(limits.max_work_units);
    calculation.evaluation.max_text_bytes = calculation
        .evaluation
        .max_text_bytes
        .min(limits.max_work_units);
    calculation
}

/// Host-bounded receiver for diagnostics produced by one plugin invocation.
///
/// The host owns storage and never retains more ordinary diagnostics than the
/// configured capacity; it may retain one bounded resource sentinel when the
/// capacity is zero. Plugins must stop producing diagnostics when
/// [`Self::emit`] returns [`DiagnosticEmission::Stop`]. Trusted in-process code
/// can still allocate on its own or ignore this control signal; hard isolation
/// is outside Draft 0.1.
#[derive(Debug)]
pub struct PluginDiagnosticSink {
    diagnostics: Vec<PluginDiagnostic>,
    capacity: usize,
    omitted: usize,
    overflow_resource: Option<PluginDiagnostic>,
    stopped: bool,
    max_message_bytes: usize,
    resource_limit_code: DiagnosticCode,
}

impl PluginDiagnosticSink {
    fn new(limits: &ExtensionLimits, resource_limit_code: DiagnosticCode) -> Self {
        Self {
            diagnostics: Vec::with_capacity(limits.max_diagnostics.clamp(1, 64)),
            capacity: limits.max_diagnostics,
            omitted: 0,
            overflow_resource: None,
            stopped: false,
            max_message_bytes: limits.max_diagnostic_message_bytes,
            resource_limit_code,
        }
    }

    /// Attempts to retain one diagnostic within the host-owned bound.
    #[must_use]
    pub fn emit(&mut self, mut diagnostic: PluginDiagnostic) -> DiagnosticEmission {
        if self.stopped {
            self.omitted = self.omitted.saturating_add(1);
            return DiagnosticEmission::Stop;
        }
        if self.diagnostics.len() >= self.capacity {
            self.stopped = true;
            self.omitted = self.omitted.saturating_add(1);
            let mut resource = PluginDiagnostic::limit(
                "host.diagnostics",
                "extension diagnostics exceed the configured aggregate limit",
                diagnostic.local_span,
            )
            .with_code(self.resource_limit_code.clone());
            resource.payload_line = diagnostic.payload_line;
            resource.context = diagnostic.context;
            if self.capacity == 0 {
                self.overflow_resource = Some(resource);
            } else {
                if self.diagnostics.pop().is_some() {
                    self.omitted = self.omitted.saturating_add(1);
                }
                self.diagnostics.push(resource);
            }
            return DiagnosticEmission::Stop;
        }
        let diagnostic_bytes = diagnostic
            .message
            .len()
            .saturating_add(diagnostic.subcode.len());
        if diagnostic_bytes > self.max_message_bytes {
            diagnostic.kind = PluginDiagnosticKind::ResourceLimit;
            diagnostic.code = self.resource_limit_code.clone();
            diagnostic.severity = Severity::Error;
            "host.diagnostic_bytes".clone_into(&mut diagnostic.subcode);
            diagnostic.message = format!(
                "extension diagnostic exceeds the configured {}-byte message limit",
                self.max_message_bytes
            );
        }
        let must_stop = diagnostic.kind == PluginDiagnosticKind::ResourceLimit;
        if must_stop {
            self.stopped = true;
            diagnostic.code = self.resource_limit_code.clone();
            diagnostic.severity = Severity::Error;
        }
        self.diagnostics.push(diagnostic);
        if must_stop {
            DiagnosticEmission::Stop
        } else {
            DiagnosticEmission::Continue
        }
    }

    /// Number of additional diagnostics the host can retain for this call.
    #[must_use]
    pub fn remaining(&self) -> usize {
        if self.stopped {
            0
        } else {
            self.capacity.saturating_sub(self.diagnostics.len())
        }
    }
}

/// Result of submitting one diagnostic to [`PluginDiagnosticSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticEmission {
    Continue,
    Stop,
}

/// Result returned by one trusted plugin invocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginResult {
    pub work: ExtensionWork,
}

/// Static in-process extension contract.
///
/// Implementors are selected by host code and linked into the process. This
/// is not a dynamic ABI and exposes no workbook-directed installation surface.
pub trait ExtensionPlugin: fmt::Debug + Send + Sync {
    /// Exact capability identity. A different major is a different plugin.
    fn id(&self) -> ExtensionId;

    /// Stable code used when the host rejects this plugin's payload or output
    /// before the implementation can safely finish. Generic plugins inherit
    /// `MS3111`; a specified extension may override it.
    fn resource_limit_code(&self) -> DiagnosticCode {
        host_code(RESOURCE_LIMIT_DIAGNOSTIC)
    }

    /// Parses and validates one opaque instance against a read-only workbook.
    fn validate(
        &self,
        input: OpaqueExtensionInput<'_>,
        context: PluginContext<'_>,
        diagnostics: &mut PluginDiagnosticSink,
    ) -> PluginResult;
}

#[derive(Debug)]
struct Registration<'plugin> {
    id: ExtensionId,
    plugin: &'plugin dyn ExtensionPlugin,
}

/// Duplicate exact registration is a host configuration error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateRegistration {
    pub capability: ExtensionId,
}

impl fmt::Display for DuplicateRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "extension {} is already registered",
            capability_key(&self.capability)
        )
    }
}

impl std::error::Error for DuplicateRegistration {}

/// Exact-ID registry for application-installed, statically linked plugins.
#[derive(Debug, Default)]
pub struct ExtensionRegistry<'plugin> {
    plugins: BTreeMap<String, Registration<'plugin>>,
}

impl<'plugin> ExtensionRegistry<'plugin> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a registry containing the statically linked demonstration
    /// `assertions@1` implementation.
    #[must_use]
    pub fn with_assertions() -> ExtensionRegistry<'static> {
        let mut registry = ExtensionRegistry::new();
        let id = crate::ASSERTIONS_V1.id();
        registry.plugins.insert(
            capability_key(&id),
            Registration {
                id,
                plugin: &crate::ASSERTIONS_V1,
            },
        );
        registry
    }

    /// Registers one trusted implementation under its exact identity.
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateRegistration`] if the exact `id@major` already
    /// exists. Different majors may coexist and are never selected as fallback.
    pub fn register(
        &mut self,
        plugin: &'plugin dyn ExtensionPlugin,
    ) -> Result<(), DuplicateRegistration> {
        let id = plugin.id();
        let key = capability_key(&id);
        if self.plugins.contains_key(&key) {
            return Err(DuplicateRegistration { capability: id });
        }
        self.plugins.insert(key, Registration { id, plugin });
        Ok(())
    }

    /// Exact registered identities in deterministic lexical order.
    #[must_use]
    pub fn capabilities(&self) -> Vec<ExtensionId> {
        self.plugins
            .values()
            .map(|registration| registration.id.clone())
            .collect()
    }

    /// Evaluates capability availability and validates all eligible instances.
    #[must_use]
    pub fn validate(&self, workbook: &Workbook, limits: &ExtensionLimits) -> ExtensionReport {
        Validator::new(self, workbook, limits).run(workbook)
    }
}

/// Availability of a declared exact extension identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityAvailability {
    Available,
    UnavailableOptional,
    UnavailableRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityCheck {
    pub capability: ExtensionId,
    pub required: bool,
    pub availability: CapabilityAvailability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstanceOutcome {
    Processed,
    SkippedUnavailable,
    SkippedUndeclared,
    RejectedDuplicate,
    RejectedByLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceReport {
    pub capability: ExtensionId,
    pub instance_name: String,
    pub scope: ExtensionScope,
    pub outcome: InstanceOutcome,
}

/// Stable machine-readable reason supplementing a model diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticDetail {
    Availability,
    DuplicateDeclaration,
    UndeclaredInstance,
    DuplicateInstance,
    Plugin {
        subcode: String,
    },
    InvalidPluginSpan {
        subcode: String,
        local_span: ByteSpan,
    },
    DeclarationLimit,
    InstanceLimit,
    InvocationLimit,
    WorkLimit,
    DiagnosticsTruncated,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionDiagnostic {
    pub diagnostic: Diagnostic,
    pub capability: ExtensionId,
    pub instance_name: Option<String>,
    pub scope: ExtensionScope,
    pub detail: DiagnosticDetail,
    pub payload_line: Option<u64>,
}

/// Whole-workbook extension validation result.
#[derive(Clone, Debug, Eq, PartialEq)]
// These flags answer distinct public questions from SPEC 17.1: capability,
// calculation, rendering, validation, and assertion validity completeness.
#[allow(clippy::struct_excessive_bools)]
pub struct ExtensionReport {
    /// Every required declaration is available. Optional capabilities and
    /// undeclared opaque instances do not make core outputs incomplete.
    pub capabilities_complete: bool,
    /// Required extension availability is sufficient for complete calculation.
    pub calculation_complete: bool,
    /// Required extension availability is sufficient for complete rendering.
    pub rendering_complete: bool,
    /// No resource bound prevented the host from finishing its checks.
    pub validation_complete: bool,
    /// No emitted (or truncated) error diagnostic exists.
    pub valid: bool,
    pub capabilities: Vec<CapabilityCheck>,
    pub instances: Vec<InstanceReport>,
    pub diagnostics: Vec<ExtensionDiagnostic>,
    /// Diagnostics omitted after deterministic ordering. Nonzero is explicit
    /// evidence that `valid` cannot be established.
    pub diagnostics_omitted: usize,
    /// Declarations not resolved after aggregate declaration admission was full.
    pub declarations_omitted: usize,
    /// Instances not traversed after aggregate instance admission was full.
    pub instances_omitted: usize,
    /// Trusted plugin calls actually performed.
    pub plugin_invocations: usize,
    /// Otherwise eligible calls rejected by aggregate invocation admission.
    pub plugin_invocations_rejected: usize,
    pub work: ExtensionWork,
}

#[allow(clippy::struct_excessive_bools)]
struct Validator<'a, 'plugin> {
    registry: &'a ExtensionRegistry<'plugin>,
    limits: &'a ExtensionLimits,
    declarations: BTreeMap<String, ExtensionId>,
    capabilities: Vec<CapabilityCheck>,
    instances: Vec<InstanceReport>,
    mandatory_diagnostics: Vec<ExtensionDiagnostic>,
    host_diagnostics: Vec<ExtensionDiagnostic>,
    plugin_diagnostics: Vec<ExtensionDiagnostic>,
    diagnostics_omitted: usize,
    plugin_diagnostic_slots_used: usize,
    diagnostic_truncation_reported: bool,
    saw_error: bool,
    work: ExtensionWork,
    calculation: RefCell<CalculationCache>,
    capabilities_complete: bool,
    validation_complete: bool,
    admitted_instances: usize,
    declarations_omitted: usize,
    total_instances: usize,
    instances_omitted: usize,
    plugin_invocations: usize,
    plugin_invocations_rejected: usize,
    invocation_limit_reported: bool,
    admission_exhausted: bool,
}

impl<'a, 'plugin> Validator<'a, 'plugin> {
    fn new(
        registry: &'a ExtensionRegistry<'plugin>,
        workbook: &'a Workbook,
        limits: &'a ExtensionLimits,
    ) -> Self {
        let total_instances = workbook.extension_instances.len().saturating_add(
            workbook
                .sheets
                .iter()
                .map(|sheet| {
                    sheet
                        .items
                        .iter()
                        .filter(|item| matches!(item, SheetItem::Extension(_)))
                        .count()
                })
                .fold(0_usize, usize::saturating_add),
        );
        Self {
            registry,
            limits,
            declarations: BTreeMap::new(),
            capabilities: Vec::new(),
            instances: Vec::new(),
            mandatory_diagnostics: Vec::new(),
            host_diagnostics: Vec::new(),
            plugin_diagnostics: Vec::new(),
            diagnostics_omitted: 0,
            plugin_diagnostic_slots_used: 0,
            diagnostic_truncation_reported: false,
            saw_error: false,
            work: ExtensionWork::default(),
            calculation: RefCell::new(CalculationCache::default()),
            capabilities_complete: true,
            validation_complete: true,
            admitted_instances: 0,
            declarations_omitted: 0,
            total_instances,
            instances_omitted: 0,
            plugin_invocations: 0,
            plugin_invocations_rejected: 0,
            invocation_limit_reported: false,
            admission_exhausted: false,
        }
    }

    fn run(mut self, workbook: &Workbook) -> ExtensionReport {
        self.check_declarations(workbook);
        if self.admission_exhausted {
            return self.finish();
        }
        self.check_scope_refs(
            workbook,
            ExtensionScopeRef::Workbook,
            workbook.extension_instances.iter(),
        );
        if self.admission_exhausted {
            return self.finish();
        }
        for sheet in &workbook.sheets {
            let extensions = sheet.items.iter().filter_map(|item| match item {
                SheetItem::Extension(extension) => Some(extension),
                _ => None,
            });
            self.check_scope_refs(workbook, ExtensionScopeRef::Sheet(&sheet.id), extensions);
            if self.admission_exhausted {
                break;
            }
        }
        self.finish()
    }

    fn check_declarations(&mut self, workbook: &Workbook) {
        for (index, declaration) in workbook.extensions.iter().enumerate() {
            if index >= self.limits.max_declarations {
                self.capabilities_complete = false;
                self.validation_complete = false;
                self.admission_exhausted = true;
                self.declarations_omitted = workbook.extensions.len().saturating_sub(index);
                self.instances_omitted = self.total_instances;
                self.push_host_diagnostic(host_diagnostic(
                    RESOURCE_LIMIT_DIAGNOSTIC,
                    Severity::Error,
                    format!(
                        "extension declarations exceed the configured aggregate limit of {}",
                        self.limits.max_declarations
                    ),
                    declaration.origin.map(|origin| origin.span),
                    declaration.capability.clone(),
                    None,
                    ExtensionScope::Workbook,
                    DiagnosticDetail::DeclarationLimit,
                ));
                break;
            }
            let key = capability_key(&declaration.capability);
            let base = declaration.capability.id.as_str().to_owned();
            if self
                .declarations
                .insert(base, declaration.capability.clone())
                .is_some()
            {
                self.capabilities_complete = false;
                self.push_host_diagnostic(host_diagnostic(
                    VALIDATION_DIAGNOSTIC,
                    Severity::Error,
                    "duplicate or conflicting extension declaration",
                    declaration.origin.map(|origin| origin.span),
                    declaration.capability.clone(),
                    None,
                    ExtensionScope::Workbook,
                    DiagnosticDetail::DuplicateDeclaration,
                ));
                continue;
            }
            let available = self.registry.plugins.contains_key(&key);
            let availability = match (available, declaration.required) {
                (true, _) => CapabilityAvailability::Available,
                (false, false) => CapabilityAvailability::UnavailableOptional,
                (false, true) => CapabilityAvailability::UnavailableRequired,
            };
            self.capabilities.push(CapabilityCheck {
                capability: declaration.capability.clone(),
                required: declaration.required,
                availability,
            });
            if !available {
                let (code, severity, message) = if declaration.required {
                    self.capabilities_complete = false;
                    (
                        AVAILABILITY_REQUIRED_DIAGNOSTIC,
                        Severity::Error,
                        "required extension is not available",
                    )
                } else {
                    (
                        AVAILABILITY_WARNING_DIAGNOSTIC,
                        Severity::Warning,
                        "optional extension is not available",
                    )
                };
                let diagnostic = host_diagnostic(
                    code,
                    severity,
                    message,
                    declaration.origin.map(|origin| origin.span),
                    declaration.capability.clone(),
                    None,
                    ExtensionScope::Workbook,
                    DiagnosticDetail::Availability,
                );
                if declaration.required {
                    self.push_mandatory_diagnostic(diagnostic);
                } else {
                    self.push_host_diagnostic(diagnostic);
                }
            }
        }
    }

    fn check_scope_refs<'extension>(
        &mut self,
        workbook: &Workbook,
        scope: ExtensionScopeRef<'_>,
        extensions: impl Iterator<Item = &'extension Extension>,
    ) {
        let mut seen = BTreeSet::new();
        for extension in extensions {
            if self.admitted_instances >= self.limits.max_instances {
                self.validation_complete = false;
                self.admission_exhausted = true;
                self.instances_omitted =
                    self.total_instances.saturating_sub(self.admitted_instances);
                self.push_host_diagnostic(host_diagnostic(
                    RESOURCE_LIMIT_DIAGNOSTIC,
                    Severity::Error,
                    format!(
                        "extension instances exceed the configured aggregate limit of {}",
                        self.limits.max_instances
                    ),
                    extension.origin.map(|origin| origin.span),
                    extension.capability.clone(),
                    Some(extension.name.clone()),
                    scope.to_owned(),
                    DiagnosticDetail::InstanceLimit,
                ));
                break;
            }
            self.admitted_instances = self.admitted_instances.saturating_add(1);
            let identity = (
                capability_key(&extension.capability),
                extension.name.clone(),
            );
            if !seen.insert(identity) {
                self.capabilities_complete = false;
                self.push_host_diagnostic(host_diagnostic(
                    VALIDATION_DIAGNOSTIC,
                    Severity::Error,
                    "duplicate extension instance in one scope",
                    extension.origin.map(|origin| origin.span),
                    extension.capability.clone(),
                    Some(extension.name.clone()),
                    scope.to_owned(),
                    DiagnosticDetail::DuplicateInstance,
                ));
                self.instances.push(instance_report(
                    extension,
                    scope,
                    InstanceOutcome::RejectedDuplicate,
                ));
                continue;
            }
            self.check_instance(workbook, scope, extension);
            if self.admission_exhausted {
                break;
            }
        }
    }

    // Keeping resolution, generic admission, invocation, and bounded result
    // ingestion together makes the trust-boundary order directly auditable.
    #[allow(clippy::too_many_lines)]
    fn check_instance(
        &mut self,
        workbook: &Workbook,
        scope: ExtensionScopeRef<'_>,
        extension: &Extension,
    ) {
        let declared = self
            .declarations
            .get(extension.capability.id.as_str())
            .is_some_and(|declared| declared == &extension.capability);
        if !declared {
            self.push_host_diagnostic(host_diagnostic(
                UNDECLARED_INSTANCE_DIAGNOSTIC,
                Severity::Warning,
                "extension instance has no matching exact declaration",
                extension.origin.map(|origin| origin.span),
                extension.capability.clone(),
                Some(extension.name.clone()),
                scope.to_owned(),
                DiagnosticDetail::UndeclaredInstance,
            ));
            self.instances.push(instance_report(
                extension,
                scope,
                InstanceOutcome::SkippedUndeclared,
            ));
            return;
        }

        let key = capability_key(&extension.capability);
        let Some(registration) = self.registry.plugins.get(&key) else {
            self.instances.push(instance_report(
                extension,
                scope,
                InstanceOutcome::SkippedUnavailable,
            ));
            return;
        };

        if extension.payload.len() > self.limits.max_payload_bytes {
            self.work.payload_bytes = self
                .work
                .payload_bytes
                .saturating_add(extension.payload.len());
            self.validation_complete = false;
            let limit_code = registration.plugin.resource_limit_code();
            self.push_host_diagnostic(host_diagnostic(
                limit_code.as_str(),
                Severity::Error,
                format!(
                    "extension payload has {} bytes; the configured limit is {}",
                    extension.payload.len(),
                    self.limits.max_payload_bytes
                ),
                extension.payload_origin.map(|origin| origin.span),
                extension.capability.clone(),
                Some(extension.name.clone()),
                scope.to_owned(),
                DiagnosticDetail::Plugin {
                    subcode: "host.payload_bytes".to_owned(),
                },
            ));
            self.instances.push(instance_report(
                extension,
                scope,
                InstanceOutcome::RejectedByLimit,
            ));
            return;
        }
        if let Some(excess) =
            first_excess_payload_line(extension.payload.as_bytes(), self.limits.max_lines)
        {
            self.work.payload_bytes = self
                .work
                .payload_bytes
                .saturating_add(extension.payload.len());
            self.work.payload_lines = self
                .work
                .payload_lines
                .saturating_add(excess.observed_lines);
            self.validation_complete = false;
            let limit_code = registration.plugin.resource_limit_code();
            self.push_host_diagnostic(host_diagnostic(
                limit_code.as_str(),
                Severity::Error,
                format!(
                    "extension payload exceeds the configured {}-line limit",
                    self.limits.max_lines
                ),
                map_payload_span(
                    extension.payload_origin.map(|origin| origin.span),
                    excess.span,
                ),
                extension.capability.clone(),
                Some(extension.name.clone()),
                scope.to_owned(),
                DiagnosticDetail::Plugin {
                    subcode: "host.payload_lines".to_owned(),
                },
            ));
            self.instances.push(instance_report(
                extension,
                scope,
                InstanceOutcome::RejectedByLimit,
            ));
            return;
        }

        let remaining_targets = self.limits.max_targets.saturating_sub(self.work.targets);
        let mut invocation_limits = self.limits.clone();
        invocation_limits.max_targets = remaining_targets;
        invocation_limits.max_diagnostics = self
            .limits
            .max_diagnostics
            .saturating_sub(self.plugin_diagnostic_slots_used);
        invocation_limits.max_work_units = self
            .limits
            .max_work_units
            .saturating_sub(self.work.total_units());
        let input = OpaqueExtensionInput {
            capability: &extension.capability,
            instance_name: &extension.name,
            scope,
            extension_span: extension.origin.map(|origin| origin.span),
            payload_span: extension.payload_origin.map(|origin| origin.span),
            payload: extension.payload.as_bytes(),
        };
        if self.plugin_invocations >= self.limits.max_invocations {
            self.validation_complete = false;
            self.plugin_invocations_rejected = self.plugin_invocations_rejected.saturating_add(1);
            if !self.invocation_limit_reported {
                self.invocation_limit_reported = true;
                self.push_host_diagnostic(host_diagnostic(
                    RESOURCE_LIMIT_DIAGNOSTIC,
                    Severity::Error,
                    format!(
                        "extension invocations exceed the configured aggregate limit of {}",
                        self.limits.max_invocations
                    ),
                    extension.origin.map(|origin| origin.span),
                    extension.capability.clone(),
                    Some(extension.name.clone()),
                    scope.to_owned(),
                    DiagnosticDetail::InvocationLimit,
                ));
            }
            self.instances.push(instance_report(
                extension,
                scope,
                InstanceOutcome::RejectedByLimit,
            ));
            return;
        }
        self.plugin_invocations = self.plugin_invocations.saturating_add(1);
        let mut diagnostic_sink = PluginDiagnosticSink::new(
            &invocation_limits,
            registration.plugin.resource_limit_code(),
        );
        let mut result = registration.plugin.validate(
            input,
            PluginContext {
                workbook,
                limits: &invocation_limits,
                calculation: &self.calculation,
            },
            &mut diagnostic_sink,
        );
        result
            .work
            .saturating_add(self.calculation.borrow_mut().take_work());
        self.work.saturating_add(result.work);
        let exceeds_work = self.work.total_units() > self.limits.max_work_units
            || self.work.targets > self.limits.max_targets;
        let limited = diagnostic_sink
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == PluginDiagnosticKind::ResourceLimit)
            || diagnostic_sink.overflow_resource.is_some();
        if exceeds_work && !limited {
            self.validation_complete = false;
            let limit_code = registration.plugin.resource_limit_code();
            self.push_host_diagnostic(host_diagnostic(
                limit_code.as_str(),
                Severity::Error,
                "extension validation exceeded the configured aggregate work limit",
                extension.payload_origin.map(|origin| origin.span),
                extension.capability.clone(),
                Some(extension.name.clone()),
                scope.to_owned(),
                DiagnosticDetail::WorkLimit,
            ));
            self.instances.push(instance_report(
                extension,
                scope,
                InstanceOutcome::RejectedByLimit,
            ));
            return;
        }
        let omitted_before = self.diagnostics_omitted;
        self.diagnostics_omitted = self
            .diagnostics_omitted
            .saturating_add(diagnostic_sink.omitted);
        self.plugin_diagnostic_slots_used = self
            .plugin_diagnostic_slots_used
            .saturating_add(diagnostic_sink.diagnostics.len());
        let mut limited = limited;
        for diagnostic in diagnostic_sink.diagnostics {
            let is_resource_limit = diagnostic.kind == PluginDiagnosticKind::ResourceLimit;
            let mapped = map_plugin_diagnostic(input, diagnostic);
            if is_resource_limit {
                self.push_host_diagnostic(mapped);
            } else {
                self.push_plugin_diagnostic(mapped);
            }
        }
        if let Some(resource) = diagnostic_sink.overflow_resource {
            self.push_host_diagnostic(map_plugin_diagnostic(input, resource));
        }
        if diagnostic_sink.omitted > 0 && limited {
            self.diagnostic_truncation_reported = true;
        }
        limited |= self.diagnostics_omitted > omitted_before;
        self.validation_complete &= !limited;
        self.instances.push(instance_report(
            extension,
            scope,
            if limited {
                InstanceOutcome::RejectedByLimit
            } else {
                InstanceOutcome::Processed
            },
        ));
    }

    fn push_mandatory_diagnostic(&mut self, diagnostic: ExtensionDiagnostic) {
        self.saw_error |= diagnostic.diagnostic.severity == Severity::Error;
        self.mandatory_diagnostics.push(diagnostic);
    }

    fn push_host_diagnostic(&mut self, diagnostic: ExtensionDiagnostic) {
        self.saw_error |= diagnostic.diagnostic.severity == Severity::Error;
        self.host_diagnostics.push(diagnostic);
    }

    fn push_plugin_diagnostic(&mut self, diagnostic: ExtensionDiagnostic) {
        self.saw_error |= diagnostic.diagnostic.severity == Severity::Error;
        let capacity = self.limits.max_diagnostics.saturating_add(1).max(1);
        self.plugin_diagnostics.push(diagnostic);
        self.plugin_diagnostics
            .sort_by(|left, right| diagnostic_sort_key(left).cmp(&diagnostic_sort_key(right)));
        if self.plugin_diagnostics.len() > capacity {
            self.plugin_diagnostics.pop();
            self.diagnostics_omitted = self.diagnostics_omitted.saturating_add(1);
        }
    }

    fn finish(mut self) -> ExtensionReport {
        self.mandatory_diagnostics
            .sort_by(|left, right| diagnostic_sort_key(left).cmp(&diagnostic_sort_key(right)));
        self.host_diagnostics
            .sort_by(|left, right| diagnostic_sort_key(left).cmp(&diagnostic_sort_key(right)));
        self.plugin_diagnostics
            .sort_by(|left, right| diagnostic_sort_key(left).cmp(&diagnostic_sort_key(right)));
        let mut omitted = self.diagnostics_omitted;
        let plugin_limit = self.limits.max_diagnostics;
        let retained_plugins = self.plugin_diagnostics.len().min(plugin_limit);
        omitted = omitted.saturating_add(
            self.plugin_diagnostics
                .len()
                .saturating_sub(retained_plugins),
        );
        let truncation_anchor = self
            .plugin_diagnostics
            .get(retained_plugins)
            .or_else(|| self.plugin_diagnostics.last())
            .or_else(|| self.host_diagnostics.last())
            .or_else(|| self.mandatory_diagnostics.last())
            .cloned();
        self.plugin_diagnostics.truncate(retained_plugins);

        let mut diagnostics = self.mandatory_diagnostics;
        diagnostics.extend(self.host_diagnostics);
        diagnostics.append(&mut self.plugin_diagnostics);
        if omitted > 0 {
            self.validation_complete = false;
            if !self.diagnostic_truncation_reported {
                if let Some(anchor) = truncation_anchor {
                    diagnostics.push(host_diagnostic(
                        RESOURCE_LIMIT_DIAGNOSTIC,
                        Severity::Error,
                        format!("{omitted} extension diagnostics omitted by configured limit"),
                        Some(anchor.diagnostic.primary.span),
                        anchor.capability,
                        None,
                        anchor.scope,
                        DiagnosticDetail::DiagnosticsTruncated,
                    ));
                }
            }
        }
        diagnostics
            .sort_by(|left, right| diagnostic_sort_key(left).cmp(&diagnostic_sort_key(right)));
        ExtensionReport {
            capabilities_complete: self.capabilities_complete,
            calculation_complete: self.capabilities_complete,
            rendering_complete: self.capabilities_complete,
            validation_complete: self.validation_complete,
            valid: !self.saw_error && omitted == 0,
            capabilities: self.capabilities,
            instances: self.instances,
            diagnostics,
            diagnostics_omitted: omitted,
            declarations_omitted: self.declarations_omitted,
            instances_omitted: self.instances_omitted,
            plugin_invocations: self.plugin_invocations,
            plugin_invocations_rejected: self.plugin_invocations_rejected,
            work: self.work,
        }
    }
}

fn instance_report(
    extension: &Extension,
    scope: ExtensionScopeRef<'_>,
    outcome: InstanceOutcome,
) -> InstanceReport {
    InstanceReport {
        capability: extension.capability.clone(),
        instance_name: extension.name.clone(),
        scope: scope.to_owned(),
        outcome,
    }
}

fn capability_key(capability: &ExtensionId) -> String {
    format!("{}@{}", capability.id, capability.major)
}

struct PayloadLineExcess {
    observed_lines: usize,
    span: ByteSpan,
}

fn first_excess_payload_line(payload: &[u8], max_lines: usize) -> Option<PayloadLineExcess> {
    let mut lines = 0_usize;
    let mut line_start = 0_usize;
    for (index, byte) in payload.iter().copied().enumerate() {
        if byte == b'\n' {
            lines = lines.saturating_add(1);
            if lines > max_lines {
                let end = if index > line_start && payload[index - 1] == b'\r' {
                    index - 1
                } else {
                    index
                };
                return Some(PayloadLineExcess {
                    observed_lines: lines,
                    span: local_span(line_start, end),
                });
            }
            line_start = index + 1;
        }
    }
    if !payload.is_empty() && !payload.ends_with(b"\n") {
        lines = lines.saturating_add(1);
        if lines > max_lines {
            return Some(PayloadLineExcess {
                observed_lines: lines,
                span: local_span(line_start, payload.len()),
            });
        }
    }
    None
}

fn local_span(start: usize, end: usize) -> ByteSpan {
    ByteSpan::try_new(
        u64::try_from(start).unwrap_or(u64::MAX),
        u64::try_from(end).unwrap_or(u64::MAX),
    )
    .unwrap_or_default()
}

fn map_payload_span(payload: Option<ByteSpan>, local: ByteSpan) -> Option<ByteSpan> {
    let payload = payload?;
    ByteSpan::try_new(
        payload.start.checked_add(local.start)?,
        payload.start.checked_add(local.end)?,
    )
    .ok()
}

fn host_code(value: &str) -> DiagnosticCode {
    DiagnosticCode::new(value).unwrap_or_else(|_| {
        unreachable!("extension diagnostic constants are compile-time controlled")
    })
}

fn map_plugin_diagnostic(
    input: OpaqueExtensionInput<'_>,
    plugin: PluginDiagnostic,
) -> ExtensionDiagnostic {
    let scope = input.scope.to_owned();
    let fallback = input.payload_span.or(input.extension_span);
    let mapped = validate_and_map_span(input, plugin.local_span);
    let (span, detail, message, code) = match mapped {
        Ok(span) => (
            span.or(fallback),
            DiagnosticDetail::Plugin {
                subcode: plugin.subcode.clone(),
            },
            plugin.message,
            plugin.code.as_str(),
        ),
        Err(()) => (
            fallback,
            DiagnosticDetail::InvalidPluginSpan {
                subcode: plugin.subcode,
                local_span: plugin.local_span,
            },
            "extension plugin returned an invalid payload-relative span".to_owned(),
            VALIDATION_DIAGNOSTIC,
        ),
    };
    host_diagnostic_with_context(
        code,
        plugin.severity,
        message,
        span,
        input.capability.clone(),
        Some(input.instance_name.to_owned()),
        scope,
        detail,
        plugin.payload_line,
        plugin.context,
    )
}

fn validate_and_map_span(
    input: OpaqueExtensionInput<'_>,
    local: ByteSpan,
) -> Result<Option<ByteSpan>, ()> {
    let payload_len = u64::try_from(input.payload.len()).map_err(|_| ())?;
    if local.start > local.end || local.end > payload_len {
        return Err(());
    }
    let start = usize::try_from(local.start).map_err(|_| ())?;
    let end = usize::try_from(local.end).map_err(|_| ())?;
    if !utf8_boundary(input.payload, start) || !utf8_boundary(input.payload, end) {
        return Err(());
    }
    let Some(payload) = input.payload_span else {
        return Ok(None);
    };
    if payload.len() != payload_len {
        return Ok(None);
    }
    let Some(start) = payload.start.checked_add(local.start) else {
        return Ok(None);
    };
    let Some(end) = payload.start.checked_add(local.end) else {
        return Ok(None);
    };
    let Ok(mapped) = ByteSpan::try_new(start, end) else {
        return Ok(None);
    };
    Ok(payload.contains_span(mapped).then_some(mapped))
}

#[allow(clippy::too_many_arguments)]
fn host_diagnostic(
    code: &str,
    severity: Severity,
    message: impl Into<String>,
    span: Option<ByteSpan>,
    capability: ExtensionId,
    instance_name: Option<String>,
    scope: ExtensionScope,
    detail: DiagnosticDetail,
) -> ExtensionDiagnostic {
    host_diagnostic_with_context(
        code,
        severity,
        message,
        span,
        capability,
        instance_name,
        scope,
        detail,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn host_diagnostic_with_context(
    code: &str,
    severity: Severity,
    message: impl Into<String>,
    span: Option<ByteSpan>,
    capability: ExtensionId,
    instance_name: Option<String>,
    scope: ExtensionScope,
    detail: DiagnosticDetail,
    payload_line: Option<u64>,
    context: Option<DiagnosticContext>,
) -> ExtensionDiagnostic {
    let span = span.unwrap_or_default();
    ExtensionDiagnostic {
        diagnostic: Diagnostic {
            code: host_code(code),
            severity,
            message: message.into(),
            primary: LabeledSpan {
                span,
                label: instance_name.clone(),
            },
            related: Vec::new(),
            context,
            suggestion: None,
        },
        capability,
        instance_name,
        scope,
        detail,
        payload_line,
    }
}

fn diagnostic_sort_key(diagnostic: &ExtensionDiagnostic) -> (u64, u64, &str, String, &str, String) {
    (
        diagnostic.diagnostic.primary.span.start,
        diagnostic.diagnostic.primary.span.end,
        diagnostic.diagnostic.code.as_str(),
        capability_key(&diagnostic.capability),
        diagnostic.instance_name.as_deref().unwrap_or(""),
        match &diagnostic.detail {
            DiagnosticDetail::Plugin { subcode }
            | DiagnosticDetail::InvalidPluginSpan { subcode, .. } => subcode.clone(),
            other => format!("{other:?}"),
        },
    )
}

fn utf8_boundary(bytes: &[u8], index: usize) -> bool {
    index == 0
        || index == bytes.len()
        || bytes
            .get(index)
            .is_some_and(|byte| byte & 0b1100_0000 != 0b1000_0000)
}
