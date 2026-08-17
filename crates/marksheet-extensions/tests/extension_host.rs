use marksheet_extensions::{
    ASSERTION_FAILED_DIAGNOSTIC, ASSERTION_LIMIT_DIAGNOSTIC, ASSERTION_MALFORMED_DIAGNOSTIC,
    AVAILABILITY_REQUIRED_DIAGNOSTIC, AVAILABILITY_WARNING_DIAGNOSTIC, DiagnosticDetail,
    DiagnosticEmission, ExtensionLimits, ExtensionPlugin, ExtensionRegistry, ExtensionScope,
    InstanceOutcome, OpaqueExtensionInput, PluginContext, PluginDiagnostic, PluginDiagnosticSink,
    PluginResult, RESOURCE_LIMIT_DIAGNOSTIC, UNDECLARED_INSTANCE_DIAGNOSTIC,
};
use marksheet_model::{
    ByteSpan, Coordinate, DiagnosticCode, DiagnosticContext, Extension, ExtensionDeclaration,
    ExtensionId, LineIndex, Severity, Sheet, SheetId, SheetItem, Workbook,
};

fn parse_workbook(source: &str) -> Workbook {
    marksheet_syntax::parse(source.as_bytes())
        .workbook
        .expect("fixture has a semantic workbook")
}

fn assertions_registry() -> ExtensionRegistry<'static> {
    ExtensionRegistry::with_assertions()
}

fn diagnostic_codes(report: &marksheet_extensions::ExtensionReport) -> Vec<&str> {
    report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.diagnostic.code.as_str())
        .collect()
}

fn diagnostic_lines(source: &str, report: &marksheet_extensions::ExtensionReport) -> Vec<u64> {
    let index = LineIndex::new(source);
    report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            index
                .line_column(diagnostic.diagnostic.primary.span.start)
                .expect("mapped span starts in source")
                .line
        })
        .collect()
}

#[test]
fn exact_registration_rejects_duplicates_but_allows_another_major() {
    #[derive(Debug)]
    struct Noop(&'static str);
    impl ExtensionPlugin for Noop {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse(self.0).unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            PluginResult::default()
        }
    }

    static V1: Noop = Noop("demo@1");
    static V2: Noop = Noop("demo@2");
    let mut registry = ExtensionRegistry::new();
    registry.register(&V1).unwrap();
    registry.register(&V2).unwrap();
    let duplicate = registry.register(&V1).unwrap_err();
    assert_eq!(duplicate.capability, ExtensionId::parse("demo@1").unwrap());
    assert_eq!(
        registry.capabilities(),
        [
            ExtensionId::parse("demo@1").unwrap(),
            ExtensionId::parse("demo@2").unwrap()
        ]
    );
}

#[test]
fn optional_and_required_availability_have_distinct_completeness() {
    let optional = parse_workbook("#!marksheet 0.1\n@use absent@1\n@sheet s \"S\"\n");
    let report = ExtensionRegistry::new().validate(&optional, &ExtensionLimits::default());
    assert_eq!(diagnostic_codes(&report), [AVAILABILITY_WARNING_DIAGNOSTIC]);
    assert!(report.capabilities_complete);
    assert!(report.calculation_complete);
    assert!(report.rendering_complete);
    assert!(report.valid);

    let required = parse_workbook("#!marksheet 0.1\n@require absent@1\n@sheet s \"S\"\n");
    let report = ExtensionRegistry::new().validate(&required, &ExtensionLimits::default());
    assert_eq!(
        diagnostic_codes(&report),
        [AVAILABILITY_REQUIRED_DIAGNOSTIC]
    );
    assert!(!report.capabilities_complete);
    assert!(!report.calculation_complete);
    assert!(!report.rendering_complete);
    assert!(!report.valid);
}

#[test]
fn undeclared_instance_warns_without_making_core_outputs_incomplete() {
    let source = "#!marksheet 0.1\n@sheet s \"S\"\n@extension vendor@1 \"opaque\"\nbytes\n@end\n";
    let workbook = parse_workbook(source);
    let report = ExtensionRegistry::new().validate(&workbook, &ExtensionLimits::default());
    assert_eq!(diagnostic_codes(&report), [UNDECLARED_INSTANCE_DIAGNOSTIC]);
    assert_eq!(report.diagnostics[0].diagnostic.severity, Severity::Warning);
    assert!(report.capabilities_complete);
    assert!(report.calculation_complete);
    assert!(report.rendering_complete);
    assert!(report.valid);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::SkippedUndeclared
    );
}

#[test]
fn assertions_use_calculated_typed_values_in_both_scopes() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@extension assertions@1 \"root\"\nassert inputs!B2 = 10\n@end\n\n@sheet inputs \"Inputs\"\n@block A1 csv\nValue,Double,Flag,Text,Error\n5,=A2*2,true,hello,=1/0\n@end\n@extension assertions@1 \"sheet\"\n# comments and empty lines are allowed\n\nassert A2 < 6\nassert B2 >= 10\nassert C2 = true\nassert D2 = \"hello\"\nassert E2 = #DIV/0!\n@end\n";
    let workbook = parse_workbook(source);
    let report = assertions_registry().validate(&workbook, &ExtensionLimits::default());
    assert!(report.valid, "{:#?}", report.diagnostics);
    assert!(report.validation_complete);
    assert_eq!(report.instances.len(), 2);
    assert!(
        report
            .instances
            .iter()
            .all(|instance| instance.outcome == InstanceOutcome::Processed)
    );
    assert_eq!(report.work.targets, 6);
    assert!(report.work.evaluation_steps > 0);
}

#[test]
fn date_datetime_and_json_text_literals_keep_their_types() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\nDate,DateTime,Text\n2026-08-16,2026-08-16T14:30:00-04:00,hello world\n@end\n@extension assertions@1 \"typed\"\nassert A2 >= 2026-08-16\nassert B2 = 2026-08-16T14:30:00-04:00\nassert C2 = \"hello world\"\n@end\n";
    let report =
        assertions_registry().validate(&parse_workbook(source), &ExtensionLimits::default());
    assert!(report.valid, "{:#?}", report.diagnostics);
}

#[test]
fn datetime_equality_includes_the_stored_offset_but_ordering_uses_the_instant() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\n2026-08-16T12:00:00+00:00\n@end\n@extension assertions@1 \"offset\"\nassert A1 = 2026-08-16T13:00:00+01:00\nassert A1 != 2026-08-16T13:00:00+01:00\nassert A1 < 2026-08-16T13:00:00+01:00\n@end\n";
    let report =
        assertions_registry().validate(&parse_workbook(source), &ExtensionLimits::default());
    assert_eq!(
        diagnostic_codes(&report),
        [ASSERTION_FAILED_DIAGNOSTIC, ASSERTION_FAILED_DIAGNOSTIC]
    );
}

#[test]
fn failures_are_typed_source_mapped_and_deterministically_ordered() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet inputs \"Inputs\"\n@block A1 csv\nNumber,Text,Blank\n5,hello,\n@end\n@extension assertions@1 \"checks\"\nassert A2 > 9\nassert B2 = \"goodbye\"\nassert A2 = \"5\"\nassert C2 != blank\n@end\n";
    let workbook = parse_workbook(source);
    let report = assertions_registry().validate(&workbook, &ExtensionLimits::default());
    assert_eq!(
        diagnostic_codes(&report),
        [
            ASSERTION_FAILED_DIAGNOSTIC,
            ASSERTION_FAILED_DIAGNOSTIC,
            ASSERTION_FAILED_DIAGNOSTIC,
            ASSERTION_FAILED_DIAGNOSTIC
        ]
    );
    assert_eq!(diagnostic_lines(source, &report), [9, 10, 11, 12]);
    assert!(report.capabilities_complete);
    assert!(report.validation_complete);
    assert!(!report.valid);
    for diagnostic in &report.diagnostics {
        assert!(matches!(
            diagnostic.detail,
            DiagnosticDetail::Plugin { ref subcode } if subcode == "assertions.failed"
        ));
        assert_eq!(
            diagnostic.diagnostic.context,
            Some(DiagnosticContext {
                sheet: Some(SheetId::parse("inputs").unwrap()),
                cell: diagnostic.diagnostic.context.as_ref().unwrap().cell,
            })
        );
    }
}

#[test]
fn malformed_lines_enforce_exact_tokens_scope_and_literals() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@extension assertions@1 \"root\"\nassert A1 = 1\n@end\n@sheet s \"S\"\n@block A1 csv\n1\n@end\n@extension assertions@1 \"sheet\"\nassert s!A1 = 1\nassert A1 == 1\nassert  A1 = 1\nassert A1 = unquoted\nassert a1 = 1\n@end\n";
    let workbook = parse_workbook(source);
    let report = assertions_registry().validate(&workbook, &ExtensionLimits::default());
    assert_eq!(
        diagnostic_codes(&report),
        [ASSERTION_MALFORMED_DIAGNOSTIC; 6]
    );
    assert_eq!(diagnostic_lines(source, &report), [4, 11, 12, 13, 14, 15]);
    assert!(report.validation_complete);
    assert!(!report.valid);
}

#[test]
fn json_text_rejects_surrounding_non_separator_whitespace() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\nx\n@end\n@extension assertions@1 \"json\"\nassert A1 = \t\"x\"\nassert A1 = \"x\" \n@end\n";
    let report =
        assertions_registry().validate(&parse_workbook(source), &ExtensionLimits::default());
    assert_eq!(
        diagnostic_codes(&report),
        [
            ASSERTION_MALFORMED_DIAGNOSTIC,
            ASSERTION_MALFORMED_DIAGNOSTIC
        ]
    );
}

#[test]
fn configured_limits_stop_an_instance_with_ms3203() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\n1\n@end\n@extension assertions@1 \"checks\"\nassert A1 = 1\nassert A1 = 1\nassert A1 = 1\n@end\n";
    let workbook = parse_workbook(source);
    let limits = ExtensionLimits {
        max_targets: 2,
        ..ExtensionLimits::default()
    };
    let report = assertions_registry().validate(&workbook, &limits);
    assert_eq!(diagnostic_codes(&report), [ASSERTION_LIMIT_DIAGNOSTIC]);
    assert_eq!(diagnostic_lines(source, &report), [10]);
    assert!(!report.validation_complete);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
}

#[test]
fn every_assertions_resource_dimension_is_configurable() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\n=1+1\n@end\n@extension assertions@1 \"checks\"\nassert A1 = 2\nassert A1 = 3\n@end\n";
    let workbook = parse_workbook(source);
    let cases = [
        ExtensionLimits {
            max_payload_bytes: 1,
            ..ExtensionLimits::default()
        },
        ExtensionLimits {
            max_lines: 1,
            ..ExtensionLimits::default()
        },
        ExtensionLimits {
            max_target_area: 0,
            ..ExtensionLimits::default()
        },
        ExtensionLimits {
            max_work_units: 1,
            ..ExtensionLimits::default()
        },
        ExtensionLimits {
            formula_evaluation: marksheet_calc::eval::EvaluationLimits {
                max_steps: 0,
                ..marksheet_calc::eval::EvaluationLimits::default()
            },
            ..ExtensionLimits::default()
        },
    ];
    for limits in cases {
        let report = assertions_registry().validate(&workbook, &limits);
        assert_eq!(
            diagnostic_codes(&report),
            [ASSERTION_LIMIT_DIAGNOSTIC],
            "{:#?}",
            report.diagnostics
        );
        assert!(!report.validation_complete);
    }
}

#[test]
fn duplicate_instance_is_rejected_only_within_the_same_scope() {
    let capability = ExtensionId::parse("assertions@1").unwrap();
    let extension = || Extension {
        capability: capability.clone(),
        name: "checks".to_owned(),
        payload: String::new(),
        origin: None,
        payload_origin: None,
    };
    let mut workbook = Workbook::default();
    workbook.extensions.push(ExtensionDeclaration {
        capability: capability.clone(),
        required: false,
        origin: None,
    });
    workbook.extension_instances.push(extension());
    workbook.extension_instances.push(extension());
    workbook.sheets.push(Sheet {
        id: SheetId::parse("s").unwrap(),
        label: "S".to_owned(),
        items: vec![SheetItem::Extension(extension())],
        origin: None,
    });

    let report = assertions_registry().validate(&workbook, &ExtensionLimits::default());
    assert_eq!(report.instances.len(), 3);
    assert_eq!(report.instances[0].scope, ExtensionScope::Workbook);
    assert_eq!(
        report.instances[1].outcome,
        InstanceOutcome::RejectedDuplicate
    );
    assert_eq!(
        report.instances[2].scope,
        ExtensionScope::Sheet(SheetId::parse("s").unwrap())
    );
}

#[test]
fn host_rejects_plugin_local_spans_outside_the_opaque_payload() {
    #[derive(Debug)]
    struct BadSpan;
    impl ExtensionPlugin for BadSpan {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("bad_span@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            let _ = diagnostics.emit(PluginDiagnostic::rejected(
                "bad_span.outside",
                "bad",
                ByteSpan::try_new(0, 99).unwrap(),
            ));
            PluginResult::default()
        }
    }

    static BAD: BadSpan = BadSpan;
    let mut registry = ExtensionRegistry::new();
    registry.register(&BAD).unwrap();
    let source = "#!marksheet 0.1\n@use bad_span@1\n@sheet s \"S\"\n@extension bad_span@1 \"bad\"\nx\n@end\n";
    let workbook = parse_workbook(source);
    let expected_payload_span = workbook.sheets[0]
        .items
        .iter()
        .find_map(|item| match item {
            SheetItem::Extension(extension) => extension.payload_origin.map(|origin| origin.span),
            _ => None,
        })
        .unwrap();
    let report = registry.validate(&workbook, &ExtensionLimits::default());
    assert!(matches!(
        report.diagnostics[0].detail,
        DiagnosticDetail::InvalidPluginSpan { .. }
    ));
    assert_eq!(
        report.diagnostics[0].diagnostic.primary.span,
        expected_payload_span
    );
}

#[test]
fn diagnostic_truncation_is_explicit_and_deterministic() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\n1\n@end\n@extension assertions@1 \"checks\"\nassert A1 = 2\nassert A1 = 3\n@end\n";
    let workbook = parse_workbook(source);
    let limits = ExtensionLimits {
        max_diagnostics: 1,
        ..ExtensionLimits::default()
    };
    let report = assertions_registry().validate(&workbook, &limits);
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(diagnostic_codes(&report), [ASSERTION_LIMIT_DIAGNOSTIC]);
    assert!(!report.validation_complete);
    assert!(!report.valid);
}

#[test]
fn zero_diagnostic_cap_cannot_hide_a_required_capability_error() {
    let workbook = parse_workbook("#!marksheet 0.1\n@require absent@1\n@sheet s \"S\"\n");
    let limits = ExtensionLimits {
        max_diagnostics: 0,
        ..ExtensionLimits::default()
    };
    let report = ExtensionRegistry::new().validate(&workbook, &limits);
    assert_eq!(
        diagnostic_codes(&report),
        [AVAILABILITY_REQUIRED_DIAGNOSTIC]
    );
    assert!(!report.calculation_complete);
}

#[test]
fn zero_diagnostic_cap_turns_an_assertion_failure_into_ms3203() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\n1\n@end\n@extension assertions@1 \"checks\"\nassert A1 = 2\n@end\n";
    let limits = ExtensionLimits {
        max_diagnostics: 0,
        ..ExtensionLimits::default()
    };
    let report = assertions_registry().validate(&parse_workbook(source), &limits);
    assert_eq!(diagnostic_codes(&report), [ASSERTION_LIMIT_DIAGNOSTIC]);
    assert_eq!(report.diagnostics_omitted, 1);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
    assert!(!report.validation_complete);
}

#[test]
fn plugin_diagnostic_flood_cannot_evict_host_availability_diagnostics() {
    #[derive(Debug)]
    struct Noisy;
    impl ExtensionPlugin for Noisy {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("noisy@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            for offset in 0_u64..3 {
                let _ = diagnostics.emit(PluginDiagnostic::rejected(
                    format!("noisy.{offset}"),
                    "noise",
                    ByteSpan::try_new(0, 0).unwrap(),
                ));
            }
            PluginResult::default()
        }
    }

    static NOISY: Noisy = Noisy;
    let mut registry = ExtensionRegistry::new();
    registry.register(&NOISY).unwrap();
    let source = "#!marksheet 0.1\n@require absent_a@1\n@require absent_b@1\n@use absent_c@1\n@use noisy@1\n@sheet s \"S\"\n@extension noisy@1 \"noise\"\nx\n@end\n";
    let limits = ExtensionLimits {
        max_diagnostics: 0,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&parse_workbook(source), &limits);
    let codes = diagnostic_codes(&report);
    assert_eq!(
        codes
            .iter()
            .filter(|code| **code == AVAILABILITY_REQUIRED_DIAGNOSTIC)
            .count(),
        2
    );
    assert_eq!(
        codes
            .iter()
            .filter(|code| **code == AVAILABILITY_WARNING_DIAGNOSTIC)
            .count(),
        1
    );
    assert_eq!(
        codes
            .iter()
            .filter(|code| **code == RESOURCE_LIMIT_DIAGNOSTIC)
            .count(),
        1
    );
    assert_eq!(report.diagnostics_omitted, 3);
    assert!(!report.capabilities_complete);
    assert!(!report.validation_complete);
}

#[test]
fn aggregate_instance_admission_stops_traversal_before_extra_calls() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CountingInstances;
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    impl ExtensionPlugin for CountingInstances {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("count_instances@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            CALLS.fetch_add(1, Ordering::Relaxed);
            PluginResult::default()
        }
    }

    static COUNTING: CountingInstances = CountingInstances;
    CALLS.store(0, Ordering::Relaxed);
    let mut registry = ExtensionRegistry::new();
    registry.register(&COUNTING).unwrap();
    let source = "#!marksheet 0.1\n@use count_instances@1\n@extension count_instances@1 \"root_one\"\nx\n@end\n@extension count_instances@1 \"root_two\"\nx\n@end\n@sheet s \"S\"\n@extension count_instances@1 \"sheet_three\"\nx\n@end\n";
    let limits = ExtensionLimits {
        max_instances: 2,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&parse_workbook(source), &limits);
    assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    assert_eq!(report.instances.len(), 2);
    assert_eq!(report.instances_omitted, 1);
    assert_eq!(report.plugin_invocations, 2);
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert!(!report.validation_complete);
}

#[test]
fn aggregate_declaration_admission_bounds_capabilities_and_stops_traversal() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DeclaredPlugin;
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    impl ExtensionPlugin for DeclaredPlugin {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("declared@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            CALLS.fetch_add(1, Ordering::Relaxed);
            PluginResult::default()
        }
    }

    static DECLARED: DeclaredPlugin = DeclaredPlugin;
    CALLS.store(0, Ordering::Relaxed);
    let mut registry = ExtensionRegistry::new();
    registry.register(&DECLARED).unwrap();
    let source = "#!marksheet 0.1\n@use declared@1\n@require absent_a@1\n@use absent_b@1\n@sheet s \"S\"\n@extension declared@1 \"never_called\"\nx\n@end\n";
    let limits = ExtensionLimits {
        max_declarations: 1,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&parse_workbook(source), &limits);
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    assert_eq!(report.capabilities.len(), 1);
    assert_eq!(report.declarations_omitted, 2);
    assert_eq!(report.instances_omitted, 1);
    assert!(report.instances.is_empty());
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert!(!report.capabilities_complete);
    assert!(!report.validation_complete);
    assert!(!report.calculation_complete);
    assert!(!report.rendering_complete);
}

#[test]
fn aggregate_invocation_admission_rejects_eligible_calls_without_invoking() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CountingInvocations;
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    impl ExtensionPlugin for CountingInvocations {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("count_invocations@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            CALLS.fetch_add(1, Ordering::Relaxed);
            PluginResult::default()
        }
    }

    static COUNTING: CountingInvocations = CountingInvocations;
    CALLS.store(0, Ordering::Relaxed);
    let mut registry = ExtensionRegistry::new();
    registry.register(&COUNTING).unwrap();
    let source = "#!marksheet 0.1\n@use count_invocations@1\n@extension count_invocations@1 \"one\"\nx\n@end\n@extension count_invocations@1 \"two\"\nx\n@end\n@sheet s \"S\"\n@extension count_invocations@1 \"three\"\nx\n@end\n";
    let limits = ExtensionLimits {
        max_invocations: 1,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&parse_workbook(source), &limits);
    assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    assert_eq!(report.plugin_invocations, 1);
    assert_eq!(report.plugin_invocations_rejected, 2);
    assert_eq!(report.instances.len(), 3);
    assert_eq!(report.instances[0].outcome, InstanceOutcome::Processed);
    assert_eq!(
        report.instances[1].outcome,
        InstanceOutcome::RejectedByLimit
    );
    assert_eq!(
        report.instances[2].outcome,
        InstanceOutcome::RejectedByLimit
    );
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert!(!report.validation_complete);
}

#[test]
fn host_preflights_payload_before_invoking_a_noncompliant_plugin() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Counting;
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    impl ExtensionPlugin for Counting {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("counting@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            CALLS.fetch_add(1, Ordering::Relaxed);
            PluginResult::default()
        }
    }

    static COUNTING: Counting = Counting;
    CALLS.store(0, Ordering::Relaxed);
    let mut registry = ExtensionRegistry::new();
    registry.register(&COUNTING).unwrap();
    let workbook = parse_workbook(
        "#!marksheet 0.1\n@use counting@1\n@sheet s \"S\"\n@extension counting@1 \"x\"\ntoo large\n@end\n",
    );
    let limits = ExtensionLimits {
        max_payload_bytes: 1,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&workbook, &limits);
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
}

#[test]
fn host_preflights_physical_lines_before_invoking_a_noncompliant_plugin() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CountingLines;
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    impl ExtensionPlugin for CountingLines {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("counting_lines@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            CALLS.fetch_add(1, Ordering::Relaxed);
            PluginResult::default()
        }
    }

    static COUNTING_LINES: CountingLines = CountingLines;
    CALLS.store(0, Ordering::Relaxed);
    let mut registry = ExtensionRegistry::new();
    registry.register(&COUNTING_LINES).unwrap();
    let source = "#!marksheet 0.1\n@use counting_lines@1\n@sheet s \"S\"\n@extension counting_lines@1 \"x\"\nfirst\nsecond\n@end\n";
    let workbook = parse_workbook(source);
    let limits = ExtensionLimits {
        max_lines: 1,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&workbook, &limits);
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert_eq!(diagnostic_lines(source, &report), [6]);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
}

#[test]
fn host_bounds_noncompliant_plugin_diagnostics_during_ingestion() {
    #[derive(Debug)]
    struct Flood;
    impl ExtensionPlugin for Flood {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("flood@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            // Intentionally ignore Stop to prove host retention stays bounded
            // even for incorrectly implemented trusted code.
            for offset in (0_u64..10).rev() {
                let _ = diagnostics.emit(PluginDiagnostic::rejected(
                    format!("flood.{offset}"),
                    "bounded",
                    ByteSpan::try_new(offset, offset + 1).unwrap(),
                ));
            }
            PluginResult::default()
        }
    }

    static FLOOD: Flood = Flood;
    let mut registry = ExtensionRegistry::new();
    registry.register(&FLOOD).unwrap();
    let workbook = parse_workbook(
        "#!marksheet 0.1\n@use flood@1\n@sheet s \"S\"\n@extension flood@1 \"x\"\n0123456789\n@end\n",
    );
    let limits = ExtensionLimits {
        max_diagnostics: 2,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&workbook, &limits);
    assert_eq!(report.diagnostics.len(), 2);
    assert_eq!(report.diagnostics_omitted, 9);
    assert!(diagnostic_codes(&report).contains(&RESOURCE_LIMIT_DIAGNOSTIC));
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
    assert!(
        report.diagnostics[0].diagnostic.primary.span.start
            <= report.diagnostics[1].diagnostic.primary.span.start
    );
}

#[test]
fn multiple_assertion_instances_share_one_core_preparation() {
    let source = "#!marksheet 0.1\n@use assertions@1\n@sheet s \"S\"\n@block A1 csv\n=1+1\n@end\n@extension assertions@1 \"first\"\nassert A1 = 2\n@end\n@extension assertions@1 \"second\"\nassert A1 = 2\n@end\n";
    let report =
        assertions_registry().validate(&parse_workbook(source), &ExtensionLimits::default());
    assert!(report.valid, "{:#?}", report.diagnostics);
    assert_eq!(report.work.preparations, 1);
}

#[test]
fn host_meters_calculation_work_even_when_a_plugin_does_not_report_it() {
    #[derive(Debug)]
    struct UnmeteredLookup;
    impl ExtensionPlugin for UnmeteredLookup {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("unmetered_lookup@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            let sheet = SheetId::parse("s").unwrap();
            let coordinate = Coordinate::parse("A1").unwrap();
            let lookup = context.calculated_cell(&sheet, coordinate);
            assert_eq!(
                lookup.value,
                Some(marksheet_calc::eval::CalcValue::Number(2.0))
            );
            // Calculator work is host-owned and absent from PluginResult.
            PluginResult::default()
        }
    }

    static UNMETERED: UnmeteredLookup = UnmeteredLookup;
    let mut registry = ExtensionRegistry::new();
    registry.register(&UNMETERED).unwrap();
    let workbook = parse_workbook(
        "#!marksheet 0.1\n@use unmetered_lookup@1\n@sheet s \"S\"\n@block A1 csv\n=1+1\n@end\n@extension unmetered_lookup@1 \"x\"\nignored\n@end\n",
    );
    let report = registry.validate(&workbook, &ExtensionLimits::default());
    assert!(report.valid, "{:#?}", report.diagnostics);
    assert_eq!(report.work.preparations, 1);
    assert!(report.work.evaluation_steps > 0);
}

#[test]
fn calculated_cell_stops_when_lookups_cross_the_aggregate_budget() {
    #[derive(Debug)]
    struct TwoLookups;
    impl ExtensionPlugin for TwoLookups {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("two_lookups@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            context: PluginContext<'_>,
            _diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            let sheet = SheetId::parse("s").unwrap();
            let first = context.calculated_cell(&sheet, Coordinate::parse("A1").unwrap());
            assert!(!first.resource_limited);
            let second = context.calculated_cell(&sheet, Coordinate::parse("B1").unwrap());
            assert!(second.resource_limited);
            assert!(second.value.is_none());
            PluginResult::default()
        }
    }

    static TWO_LOOKUPS: TwoLookups = TwoLookups;
    let mut registry = ExtensionRegistry::new();
    registry.register(&TWO_LOOKUPS).unwrap();
    let workbook = parse_workbook(
        "#!marksheet 0.1\n@use two_lookups@1\n@sheet s \"S\"\n@block A1 csv\n=1+1,=1+2\n@end\n@extension two_lookups@1 \"x\"\nignored\n@end\n",
    );
    let limits = ExtensionLimits {
        // Each formula is below the cap; preparation plus both formulas is not.
        max_work_units: 5,
        ..ExtensionLimits::default()
    };
    let report = registry.validate(&workbook, &limits);
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
}

#[test]
fn host_normalizes_malformed_plugin_limit_diagnostics() {
    #[derive(Debug)]
    struct BadLimit;
    impl ExtensionPlugin for BadLimit {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("bad_limit@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            let mut diagnostic =
                PluginDiagnostic::limit("bad_limit", "limited", ByteSpan::default());
            diagnostic.code = DiagnosticCode::new("MS3110").unwrap();
            diagnostic.severity = Severity::Warning;
            assert_eq!(diagnostics.emit(diagnostic), DiagnosticEmission::Stop);
            PluginResult::default()
        }
    }

    static BAD_LIMIT: BadLimit = BadLimit;
    let mut registry = ExtensionRegistry::new();
    registry.register(&BAD_LIMIT).unwrap();
    let workbook = parse_workbook(
        "#!marksheet 0.1\n@use bad_limit@1\n@sheet s \"S\"\n@extension bad_limit@1 \"x\"\n\n@end\n",
    );
    let report = registry.validate(&workbook, &ExtensionLimits::default());
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert_eq!(report.diagnostics[0].diagnostic.severity, Severity::Error);
    assert!(!report.valid);
    assert!(!report.validation_complete);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
}

#[test]
fn diagnostic_sink_latches_after_resource_stop() {
    #[derive(Debug)]
    struct RepeatedLimit;
    impl ExtensionPlugin for RepeatedLimit {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("repeated_limit@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            for _ in 0..10 {
                assert_eq!(
                    diagnostics.emit(PluginDiagnostic::limit(
                        "repeated_limit",
                        "limited",
                        ByteSpan::default(),
                    )),
                    DiagnosticEmission::Stop
                );
                assert_eq!(diagnostics.remaining(), 0);
            }
            PluginResult::default()
        }
    }

    static REPEATED_LIMIT: RepeatedLimit = RepeatedLimit;
    let mut registry = ExtensionRegistry::new();
    registry.register(&REPEATED_LIMIT).unwrap();
    let workbook = parse_workbook(
        "#!marksheet 0.1\n@use repeated_limit@1\n@sheet s \"S\"\n@extension repeated_limit@1 \"x\"\n\n@end\n",
    );
    let report = registry.validate(&workbook, &ExtensionLimits::default());
    assert_eq!(diagnostic_codes(&report), [RESOURCE_LIMIT_DIAGNOSTIC]);
    assert_eq!(report.diagnostics_omitted, 9);
    assert_eq!(
        report.instances[0].outcome,
        InstanceOutcome::RejectedByLimit
    );
}

#[test]
fn plugin_diagnostics_are_sorted_by_mapped_source_span_then_code() {
    #[derive(Debug)]
    struct Reverse;
    impl ExtensionPlugin for Reverse {
        fn id(&self) -> ExtensionId {
            ExtensionId::parse("reverse@1").unwrap()
        }

        fn validate(
            &self,
            _input: OpaqueExtensionInput<'_>,
            _context: PluginContext<'_>,
            diagnostics: &mut PluginDiagnosticSink,
        ) -> PluginResult {
            assert_eq!(
                diagnostics.emit(PluginDiagnostic::rejected(
                    "reverse.second",
                    "second",
                    ByteSpan::try_new(2, 3).unwrap(),
                )),
                DiagnosticEmission::Continue
            );
            assert_eq!(
                diagnostics.emit(PluginDiagnostic::rejected(
                    "reverse.first",
                    "first",
                    ByteSpan::try_new(0, 1).unwrap(),
                )),
                DiagnosticEmission::Continue
            );
            PluginResult::default()
        }
    }

    static REVERSE: Reverse = Reverse;
    let mut registry = ExtensionRegistry::new();
    registry.register(&REVERSE).unwrap();
    let source =
        "#!marksheet 0.1\n@use reverse@1\n@sheet s \"S\"\n@extension reverse@1 \"r\"\nabc\n@end\n";
    let report = registry.validate(&parse_workbook(source), &ExtensionLimits::default());
    assert!(
        report.diagnostics[0].diagnostic.primary.span.start
            < report.diagnostics[1].diagnostic.primary.span.start
    );
    assert!(matches!(
        report.diagnostics[0].detail,
        DiagnosticDetail::Plugin { ref subcode } if subcode == "reverse.first"
    ));
}
