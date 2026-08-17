//! Trusted, statically linked extension hosting for Marksheet workbooks.
//!
//! The implementation is intentionally in-process and capability-based. A
//! workbook can select an already registered exact `id@major`; it cannot
//! install code or acquire filesystem, network, clock, process, or randomness
//! handles through this API.

#![forbid(unsafe_code)]

mod assertions;
mod registry;

pub use assertions::{
    ASSERTION_FAILED_DIAGNOSTIC, ASSERTION_LIMIT_DIAGNOSTIC, ASSERTION_MALFORMED_DIAGNOSTIC,
    ASSERTIONS_V1, AssertionsV1,
};
pub use registry::{
    AVAILABILITY_REQUIRED_DIAGNOSTIC, AVAILABILITY_WARNING_DIAGNOSTIC, CalculatedLookup,
    CapabilityAvailability, CapabilityCheck, DiagnosticDetail, DiagnosticEmission,
    DuplicateRegistration, ExtensionDiagnostic, ExtensionLimits, ExtensionPlugin,
    ExtensionRegistry, ExtensionReport, ExtensionScope, ExtensionScopeRef, ExtensionWork,
    InstanceOutcome, InstanceReport, OpaqueExtensionInput, PluginContext, PluginDiagnostic,
    PluginDiagnosticKind, PluginDiagnosticSink, PluginResult, RESOURCE_LIMIT_DIAGNOSTIC,
    UNDECLARED_INSTANCE_DIAGNOSTIC, VALIDATION_DIAGNOSTIC,
};
