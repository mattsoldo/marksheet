//! Executable coverage for the shared extension conformance corpus.
//!
//! The top-level manifest is the source of truth. This test intentionally
//! discovers cases at runtime so adding fixture metadata without executable
//! host coverage fails review immediately.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use marksheet_extensions::{
    ASSERTIONS_V1, ExtensionLimits, ExtensionPlugin, ExtensionRegistry, ExtensionScope,
    InstanceOutcome, OpaqueExtensionInput, PluginContext, PluginDiagnosticSink, PluginResult,
};
use marksheet_model::{ExtensionId, LineIndex, Severity};
use serde_json::{Map, Value};

const MANIFEST_PROTOCOL: &str = "marksheet-extension-conformance@1";
const FIXTURE_PROTOCOL: &str = "marksheet-extension-fixture@1";

#[derive(Debug)]
struct NoopPlugin {
    id: ExtensionId,
}

impl ExtensionPlugin for NoopPlugin {
    fn id(&self) -> ExtensionId {
        self.id.clone()
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

#[derive(Debug, Eq, PartialEq)]
struct ExpectedDiagnostic {
    code: String,
    severity: String,
    line: Option<u64>,
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedInstance {
    capability: String,
    name: String,
    scope: String,
    outcome: String,
}

#[test]
fn manifest_cases_match_the_extension_host() {
    let root = fixture_root();
    let manifest = read_json(&root.join("manifest.json"));
    assert_eq!(string(&manifest, "protocol"), MANIFEST_PROTOCOL);
    assert_eq!(unsigned(&manifest, "version"), 1);

    let cases = manifest["cases"]
        .as_array()
        .expect("manifest cases must be an array");
    assert!(!cases.is_empty(), "extension manifest must contain cases");
    assert_fixture_discovery_is_complete(&root, cases);

    for case in cases {
        run_case(&root, object(case), case_id(case));
    }
}

// Keeping one linear orchestration path makes each manifest field's assertion
// visible and avoids a test abstraction that merely reproduces host behavior.
#[allow(clippy::too_many_lines)]
fn run_case(root: &Path, case: &Map<String, Value>, case_id: &str) {
    let fixture_path = root.join(string_from_object(case, "fixture"));
    let fixture = read_json(&fixture_path);
    let kind = string_from_object(case, "kind");
    assert_eq!(
        string(&fixture, "protocol"),
        FIXTURE_PROTOCOL,
        "fixture {case_id}"
    );
    assert_fixture_kind(&fixture, &kind, case_id);

    let registry_ids = string_array(&fixture, "registry");
    let generic_plugins = registry_ids
        .iter()
        .filter(|id| id.as_str() != "assertions@1")
        .map(|id| NoopPlugin {
            id: ExtensionId::parse(id).expect("validated fixture capability"),
        })
        .collect::<Vec<_>>();
    let mut generic_plugins = generic_plugins.iter();
    let mut registry = ExtensionRegistry::new();
    let mut registry_error = None;

    for id in &registry_ids {
        let result = if id == "assertions@1" {
            registry.register(&ASSERTIONS_V1)
        } else {
            registry.register(
                generic_plugins
                    .next()
                    .expect("one no-op implementation per generic registry entry"),
            )
        };
        if let Err(error) = result {
            registry_error = Some(("duplicate_exact_id", capability_text(&error.capability)));
            break;
        }
    }

    let expect = object(&fixture["expect"]);
    if let Some(expected_error) = expect.get("registry_error") {
        assert_eq!(kind, "registry", "fixture {case_id}");
        assert_eq!(
            registry_error.as_ref().map(|(kind, _)| *kind),
            expected_error.as_str(),
            "fixture {case_id}"
        );
        let duplicate = registry_error.expect("fixture expects a registry error").1;
        assert!(
            registry_ids.iter().filter(|id| **id == duplicate).count() > 1,
            "fixture {case_id}: reported duplicate must occur in registry input"
        );
        assert_eq!(expect.len(), 1, "fixture {case_id}");
        return;
    }
    assert!(
        registry_error.is_none(),
        "fixture {case_id}: unexpected registry error {registry_error:?}"
    );
    assert_ne!(kind, "registry", "fixture {case_id}");

    let source = fixture_source(root, &fixture);
    let parse_options = marksheet_syntax::ParseOptions {
        supported_extensions: registry_ids.clone(),
    };
    let document = marksheet_syntax::parse_with_options(&source, &parse_options);
    assert_eq!(
        marksheet_syntax::lossless_bytes(&document),
        source,
        "fixture {case_id}: parser changed lossless bytes"
    );
    let workbook = document
        .workbook
        .as_ref()
        .unwrap_or_else(|| panic!("fixture {case_id} did not lower to a workbook"));
    let limits = fixture_limits(&fixture);
    assert_limits_were_applied(&fixture, &limits, case_id);
    let report = registry.validate(workbook, &limits);

    assert_eq!(
        report.capabilities_complete,
        boolean_from_object(expect, "capabilities_complete"),
        "fixture {case_id}"
    );
    assert_eq!(
        report.calculation_complete,
        boolean_from_object(expect, "calculation_complete"),
        "fixture {case_id}"
    );
    assert_eq!(
        report.rendering_complete,
        boolean_from_object(expect, "rendering_complete"),
        "fixture {case_id}"
    );
    assert_eq!(
        report.validation_complete,
        boolean_from_object(expect, "validation_complete"),
        "fixture {case_id}"
    );
    assert_eq!(
        report.valid,
        boolean_from_object(expect, "valid"),
        "fixture {case_id}"
    );

    let source_text = std::str::from_utf8(&source).expect("fixture source must be UTF-8");
    let line_index = LineIndex::new(source_text);
    let actual_diagnostics = report
        .diagnostics
        .iter()
        .map(|diagnostic| ExpectedDiagnostic {
            code: diagnostic.diagnostic.code.as_str().to_owned(),
            severity: severity_text(diagnostic.diagnostic.severity).to_owned(),
            line: Some(
                line_index
                    .line_column(diagnostic.diagnostic.primary.span.start)
                    .expect("diagnostic span must map into fixture source")
                    .line,
            ),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual_diagnostics,
        expected_diagnostics(expect),
        "fixture {case_id}: ordered diagnostics differ"
    );

    let actual_instances = report
        .instances
        .iter()
        .map(|instance| ExpectedInstance {
            capability: capability_text(&instance.capability),
            name: instance.instance_name.clone(),
            scope: scope_text(&instance.scope),
            outcome: outcome_text(instance.outcome).to_owned(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual_instances,
        expected_instances(expect),
        "fixture {case_id}: ordered instance outcomes differ"
    );

    let opaque_instances = actual_instances
        .iter()
        .map(|instance| format!("{}:{}", instance.capability, instance.name))
        .collect::<Vec<_>>();
    assert_eq!(
        opaque_instances,
        string_array_from_object(expect, "opaque_instances"),
        "fixture {case_id}: opaque instance order differs"
    );

    assert_lossless_and_canonical_bytes(&fixture, &document, case_id);
}

fn assert_fixture_kind(fixture: &Value, kind: &str, case_id: &str) {
    let base64_fields = [
        "original_source_base64",
        "lossless_output_base64",
        "canonical_output_base64",
    ];
    match kind {
        "lossless" => {
            assert!(
                string(fixture, "source").ends_with(".inline"),
                "fixture {case_id}: lossless source must be nominally inline"
            );
            for field in base64_fields {
                assert!(
                    fixture.get(field).is_some_and(Value::is_string),
                    "fixture {case_id}: lossless fixture requires {field}"
                );
            }
        }
        "registry" | "assertions" | "availability" => {
            for field in base64_fields {
                assert!(
                    fixture.get(field).is_none(),
                    "fixture {case_id}: {kind} fixture cannot define {field}"
                );
            }
        }
        _ => panic!("fixture {case_id}: unsupported manifest kind {kind}"),
    }
}

fn assert_fixture_discovery_is_complete(root: &Path, cases: &[Value]) {
    let listed = cases
        .iter()
        .map(|case| string(case, "fixture").to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(listed.len(), cases.len(), "fixture files must be unique");

    let discovered = fs::read_dir(root)
        .expect("read extension fixture directory")
        .map(|entry| entry.expect("read extension fixture entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            (!matches!(name, "manifest.json" | "schema.json")).then(|| name.to_owned())
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        listed, discovered,
        "every extension fixture JSON must be listed in manifest.json"
    );
}

fn fixture_source(root: &Path, fixture: &Value) -> Vec<u8> {
    fixture.get("original_source_base64").map_or_else(
        || fs::read(root.join(string(fixture, "source"))).expect("read fixture source"),
        |encoded| decode_base64(encoded.as_str().expect("base64 source must be a string")),
    )
}

fn fixture_limits(fixture: &Value) -> ExtensionLimits {
    let mut limits = ExtensionLimits::default();
    let Some(configured) = fixture.get("limits") else {
        return limits;
    };
    for (name, value) in object(configured) {
        let value = usize::try_from(value.as_u64().expect("limit must be unsigned"))
            .expect("fixture limit fits usize");
        match name.as_str() {
            "max_payload_bytes" => limits.max_payload_bytes = value,
            "max_lines" => limits.max_lines = value,
            "max_targets" => limits.max_targets = value,
            "max_diagnostics" => limits.max_diagnostics = value,
            _ => panic!("unsupported extension fixture limit {name}"),
        }
    }
    limits
}

fn assert_limits_were_applied(fixture: &Value, limits: &ExtensionLimits, case_id: &str) {
    let Some(configured) = fixture.get("limits") else {
        return;
    };
    for (name, expected) in object(configured) {
        let actual = match name.as_str() {
            "max_payload_bytes" => limits.max_payload_bytes,
            "max_lines" => limits.max_lines,
            "max_targets" => limits.max_targets,
            "max_diagnostics" => limits.max_diagnostics,
            _ => panic!("unsupported extension fixture limit {name}"),
        };
        assert_eq!(
            u64::try_from(actual).expect("configured limit fits u64"),
            expected.as_u64().expect("limit must be unsigned"),
            "fixture {case_id}: limit {name} was not applied"
        );
    }
}

fn assert_lossless_and_canonical_bytes(
    fixture: &Value,
    document: &marksheet_syntax::ParsedDocument,
    case_id: &str,
) {
    let Some(expected_lossless) = fixture.get("lossless_output_base64") else {
        assert!(
            fixture.get("canonical_output_base64").is_none(),
            "fixture {case_id}: canonical bytes require lossless bytes"
        );
        return;
    };
    let expected_lossless = decode_base64(
        expected_lossless
            .as_str()
            .expect("lossless output must be base64 text"),
    );
    assert_eq!(
        marksheet_syntax::lossless_bytes(document),
        expected_lossless,
        "fixture {case_id}: lossless output differs"
    );

    let expected_canonical = fixture["canonical_output_base64"]
        .as_str()
        .map(decode_base64)
        .expect("lossless fixture must specify canonical output");
    let canonical = marksheet_syntax::canonicalize(document).unwrap_or_else(|diagnostics| {
        panic!("fixture {case_id} is not canonicalizable: {diagnostics:?}")
    });
    assert_eq!(
        canonical, expected_canonical,
        "fixture {case_id}: canonical output differs"
    );
}

fn expected_diagnostics(expect: &Map<String, Value>) -> Vec<ExpectedDiagnostic> {
    expect["diagnostics"]
        .as_array()
        .expect("expected diagnostics must be an array")
        .iter()
        .map(|diagnostic| {
            let diagnostic = object(diagnostic);
            ExpectedDiagnostic {
                code: string_from_object(diagnostic, "code"),
                severity: string_from_object(diagnostic, "severity"),
                line: diagnostic.get("line").map(|line| {
                    line.as_u64()
                        .expect("expected diagnostic line must be unsigned")
                }),
            }
        })
        .collect()
}

fn expected_instances(expect: &Map<String, Value>) -> Vec<ExpectedInstance> {
    expect["instance_outcomes"]
        .as_array()
        .expect("expected instance outcomes must be an array")
        .iter()
        .map(|instance| {
            let instance = object(instance);
            ExpectedInstance {
                capability: string_from_object(instance, "capability"),
                name: string_from_object(instance, "name"),
                scope: string_from_object(instance, "scope"),
                outcome: string_from_object(instance, "outcome"),
            }
        })
        .collect()
}

fn scope_text(scope: &ExtensionScope) -> String {
    match scope {
        ExtensionScope::Workbook => "workbook".to_owned(),
        ExtensionScope::Sheet(sheet) => format!("sheet:{}", sheet.as_str()),
    }
}

const fn outcome_text(outcome: InstanceOutcome) -> &'static str {
    match outcome {
        InstanceOutcome::Processed => "processed",
        InstanceOutcome::SkippedUnavailable => "skipped_unavailable",
        InstanceOutcome::SkippedUndeclared => "skipped_undeclared",
        InstanceOutcome::RejectedDuplicate => "rejected_duplicate",
        InstanceOutcome::RejectedByLimit => "rejected_by_limit",
    }
}

const fn severity_text(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn capability_text(capability: &ExtensionId) -> String {
    format!("{}@{}", capability.id.as_str(), capability.major)
}

fn decode_base64(encoded: &str) -> Vec<u8> {
    assert_eq!(encoded.len() % 4, 0, "invalid base64 length");
    let mut output = Vec::with_capacity(encoded.len() / 4 * 3);
    let chunk_count = encoded.len() / 4;
    for (index, chunk) in encoded.as_bytes().chunks_exact(4).enumerate() {
        assert!(
            chunk[0] != b'=' && chunk[1] != b'=',
            "invalid base64 padding"
        );
        let is_last = index + 1 == chunk_count;
        let first = base64_value(chunk[0]).expect("invalid base64 character");
        let second = base64_value(chunk[1]).expect("invalid base64 character");
        let (third, fourth) = match (chunk[2], chunk[3]) {
            (b'=', b'=') => {
                assert!(is_last, "base64 padding is only valid in the final chunk");
                assert_eq!(second & 0x0f, 0, "non-canonical base64 padding bits");
                (None, None)
            }
            (b'=', _) => panic!("invalid base64 padding"),
            (third, b'=') => {
                assert!(is_last, "base64 padding is only valid in the final chunk");
                let third = base64_value(third).expect("invalid base64 character");
                assert_eq!(third & 0x03, 0, "non-canonical base64 padding bits");
                (Some(third), None)
            }
            (third, fourth) => (
                Some(base64_value(third).expect("invalid base64 character")),
                Some(base64_value(fourth).expect("invalid base64 character")),
            ),
        };
        output.push((first << 2) | (second >> 4));
        if let Some(third) = third {
            output.push((second << 4) | (third >> 2));
            if let Some(fourth) = fourth {
                output.push((third << 6) | fourth);
            }
        }
    }
    output
}

const fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/extensions")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    }))
    .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn case_id(case: &Value) -> &str {
    string(case, "id")
}

fn object(value: &Value) -> &Map<String, Value> {
    value.as_object().expect("fixture value must be an object")
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().expect("fixture field must be a string")
}

fn string_from_object(value: &Map<String, Value>, key: &str) -> String {
    value[key]
        .as_str()
        .expect("fixture field must be a string")
        .to_owned()
}

fn boolean_from_object(value: &Map<String, Value>, key: &str) -> bool {
    value[key]
        .as_bool()
        .expect("fixture field must be a boolean")
}

fn unsigned(value: &Value, key: &str) -> u64 {
    value[key].as_u64().expect("fixture field must be unsigned")
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .expect("fixture field must be an array")
        .iter()
        .map(|item| {
            item.as_str()
                .expect("fixture array item must be a string")
                .to_owned()
        })
        .collect()
}

fn string_array_from_object(value: &Map<String, Value>, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .expect("fixture field must be an array")
        .iter()
        .map(|item| {
            item.as_str()
                .expect("fixture array item must be a string")
                .to_owned()
        })
        .collect()
}
