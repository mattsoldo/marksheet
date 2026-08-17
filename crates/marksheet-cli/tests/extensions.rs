use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[test]
fn check_runs_the_statically_linked_assertions_extension() {
    let success = marksheet()
        .arg("check")
        .arg(fixture("assertions_success.ms"))
        .output()
        .expect("CLI executes");
    assert!(
        success.status.success(),
        "stderr: {}",
        text(&success.stderr)
    );

    let failure = marksheet()
        .args(["check", "--format", "json"])
        .arg(fixture("assertions_failure.ms"))
        .output()
        .expect("CLI executes");
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stderr.is_empty());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&failure.stdout).expect("valid diagnostics JSON");
    let codes = diagnostic_codes(&diagnostics);
    assert_eq!(codes, ["MS3201", "MS3201"]);
    assert_eq!(diagnostics[0]["primary"]["line"], 10);
    assert_eq!(diagnostics[1]["primary"]["line"], 11);
}

#[test]
fn check_reports_plugin_payload_errors_at_deterministic_source_locations() {
    let output = marksheet()
        .args(["check", "--format", "json"])
        .arg(fixture("assertions_malformed.ms"))
        .output()
        .expect("CLI executes");
    assert_eq!(output.status.code(), Some(1));
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid diagnostics JSON");
    assert_eq!(
        diagnostic_codes(&diagnostics),
        ["MS3202", "MS3202", "MS3202"]
    );
    assert_eq!(diagnostics[0]["primary"]["line"], 10);
    assert_eq!(diagnostics[1]["primary"]["line"], 11);
    assert_eq!(diagnostics[2]["primary"]["line"], 12);
}

#[test]
fn availability_and_undeclared_instance_diagnostics_are_not_duplicated() {
    let required = json_check("required_unavailable.ms");
    assert_eq!(required.status.code(), Some(1));
    let required: serde_json::Value =
        serde_json::from_slice(&required.stdout).expect("valid diagnostics JSON");
    assert_eq!(diagnostic_codes(&required), ["MS3101"]);

    let optional = json_check("optional_major_mismatch.ms");
    assert!(optional.status.success());
    let optional: serde_json::Value =
        serde_json::from_slice(&optional.stdout).expect("valid diagnostics JSON");
    assert_eq!(diagnostic_codes(&optional), ["MS3102"]);

    let undeclared = json_check("undeclared_opaque.ms");
    assert!(undeclared.status.success());
    let undeclared: serde_json::Value =
        serde_json::from_slice(&undeclared.stdout).expect("valid diagnostics JSON");
    assert_eq!(diagnostic_codes(&undeclared), ["MS3103"]);
}

#[test]
fn unavailable_required_extensions_block_calculation_and_diff() {
    let required = fixture("required_unavailable.ms");
    let calculation = marksheet()
        .args(["calc", "--sheet", "inputs", "--range", "A1:A2"])
        .arg(&required)
        .output()
        .expect("CLI executes");
    assert_eq!(calculation.status.code(), Some(1));
    assert!(calculation.stdout.is_empty());
    assert!(text(&calculation.stderr).contains("error[MS3101]"));

    let comparison = marksheet()
        .arg("diff")
        .arg(fixture("assertions_success.ms"))
        .arg(required)
        .output()
        .expect("CLI executes");
    assert_eq!(comparison.status.code(), Some(1));
    assert!(comparison.stdout.is_empty());
    assert!(text(&comparison.stderr).contains("error[MS3101]"));
}

#[test]
fn assertion_failures_do_not_make_core_calculation_incomplete() {
    let calculation = marksheet()
        .args(["calc", "--sheet", "inputs", "--range", "A2:B2"])
        .arg(fixture("assertions_failure.ms"))
        .output()
        .expect("CLI executes");

    assert_eq!(calculation.status.code(), Some(1));
    let result: serde_json::Value =
        serde_json::from_slice(&calculation.stdout).expect("calculation still emits JSON");
    assert_eq!(result["selection"]["sheet"], "inputs");
    assert_eq!(result["cells"][0]["value"]["value"], 5.0);
    assert_eq!(result["cells"][1]["value"]["value"], "hello");
    assert_eq!(
        text(&calculation.stderr).matches("error[MS3201]").count(),
        2
    );
}

#[test]
fn malformed_assertion_payload_does_not_block_safe_canonicalization() {
    let formatting = marksheet()
        .args(["fmt", "--check"])
        .arg(fixture("assertions_malformed.ms"))
        .output()
        .expect("CLI executes");

    assert!(
        formatting.status.success(),
        "stderr: {}",
        text(&formatting.stderr)
    );
    assert!(formatting.stdout.is_empty());
}

fn json_check(name: &str) -> std::process::Output {
    marksheet()
        .args(["check", "--format", "json"])
        .arg(fixture(name))
        .output()
        .expect("CLI executes")
}

fn diagnostic_codes(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .expect("diagnostic array")
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().expect("diagnostic code"))
        .collect()
}

fn marksheet() -> Command {
    Command::new(env!("CARGO_BIN_EXE_marksheet"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/extensions")
        .join(name)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
