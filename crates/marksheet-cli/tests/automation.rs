use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[test]
fn inspect_and_get_expose_stable_structured_workbook_data() {
    let workbook = workspace_file("examples/budget.ms");
    let inspect = marksheet()
        .arg("inspect")
        .arg(&workbook)
        .output()
        .expect("CLI executes");
    assert!(
        inspect.status.success(),
        "stderr: {}",
        text(&inspect.stderr)
    );
    let inspect: serde_json::Value =
        serde_json::from_slice(&inspect.stdout).expect("valid inspect JSON");
    assert_eq!(inspect["version"], "marksheet-inspect@1");
    assert_eq!(inspect["status"], "ok");
    assert_eq!(inspect["workbook"]["sheets"][0]["id"], "inputs");
    assert_eq!(inspect["workbook"]["sheets"][1]["id"], "summary");
    assert_eq!(inspect["workbook"]["sheets"][0]["tables"][0]["id"], "costs");
    assert_eq!(inspect["workbook"]["names"][0]["id"], "tax_rate");

    let get = marksheet()
        .args(["get"])
        .arg(&workbook)
        .arg("tax_rate")
        .output()
        .expect("CLI executes");
    assert!(get.status.success(), "stderr: {}", text(&get.stderr));
    let get: serde_json::Value = serde_json::from_slice(&get.stdout).expect("valid get JSON");
    assert_eq!(get["version"], "marksheet-get@1");
    assert_eq!(get["target"]["sheet"], "inputs");
    assert_eq!(get["target"]["range"], "G2");
    assert_eq!(get["cells"][0]["source"], "authored");
    assert_eq!(get["cells"][0]["authored"]["value"], 0.2);
    assert_eq!(get["cells"][0]["calculated"]["value"], 0.2);

    let source_only = marksheet()
        .arg("get")
        .arg(&workbook)
        .args(["tax_rate", "--calculated", "false"])
        .output()
        .expect("CLI executes");
    assert!(source_only.status.success());
    let source_only: serde_json::Value =
        serde_json::from_slice(&source_only.stdout).expect("valid source-only JSON");
    assert_eq!(source_only["calculated"], false);
    assert_eq!(
        source_only["cells"][0]["calculated"],
        serde_json::Value::Null
    );
}

#[test]
fn get_distinguishes_authored_virtual_and_absent_cells() {
    let workbook = workspace_file("examples/budget.ms");
    let output = marksheet()
        .arg("get")
        .arg(&workbook)
        .arg("inputs!D2:E2")
        .output()
        .expect("CLI executes");
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let output: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid get JSON");
    assert_eq!(output["cells"][0]["source"], "authored");
    assert_eq!(
        output["cells"][0]["virtual_formula"],
        "=[@Cost]*[@Quantity]"
    );
    assert_eq!(output["cells"][0]["calculated"]["value"], 1500.0);
    assert_eq!(output["cells"][1]["source"], "absent");
    assert_eq!(output["cells"][1]["calculated"]["kind"], "blank");
}

#[test]
fn set_and_append_return_exact_patches_and_preserve_calculation() {
    let source = fs::read(workspace_file("examples/budget.ms")).expect("read fixture");
    let workbook = TempFile::write("automation-edit.ms", &source);

    let set = marksheet()
        .arg("set")
        .arg(workbook.path())
        .args(["tax_rate", "0.25"])
        .output()
        .expect("CLI executes");
    assert!(set.status.success(), "stderr: {}", text(&set.stderr));
    let set: serde_json::Value = serde_json::from_slice(&set.stdout).expect("valid edit JSON");
    assert_eq!(set["version"], "marksheet-edit@1");
    assert_eq!(set["changed"], true);
    assert_eq!(set["patches"].as_array().map(Vec::len), Some(1));
    assert_eq!(set["patches"][0]["replacement"], "0.25");

    let append = marksheet()
        .arg("append-table-row")
        .arg(workbook.path())
        .arg("costs")
        .args([
            "--value",
            "Transport",
            "--value",
            "50",
            "--value",
            "2",
            "--value",
            "",
        ])
        .output()
        .expect("CLI executes");
    assert!(append.status.success(), "stderr: {}", text(&append.stderr));
    let append: serde_json::Value =
        serde_json::from_slice(&append.stdout).expect("valid append JSON");
    assert_eq!(append["operation"], "append_table_row");
    assert_eq!(append["changed"], true);
    assert_eq!(append["patches"].as_array().map(Vec::len), Some(1));

    let get = marksheet()
        .arg("get")
        .arg(workbook.path())
        .arg("inputs!A5:D5")
        .output()
        .expect("CLI executes");
    assert!(get.status.success(), "stderr: {}", text(&get.stderr));
    let get: serde_json::Value = serde_json::from_slice(&get.stdout).expect("valid get JSON");
    assert_eq!(get["cells"][0]["authored"]["value"], "Transport");
    assert_eq!(get["cells"][3]["calculated"]["value"], 100.0);
}

#[test]
fn automation_refuses_ambiguous_and_invalid_edits_without_mutation() {
    let source = fs::read(workspace_file("examples/budget.ms")).expect("read fixture");
    let workbook = TempFile::write("automation-refusal.ms", &source);

    let range = marksheet()
        .arg("set")
        .arg(workbook.path())
        .args(["inputs!A1:B1", "changed"])
        .output()
        .expect("CLI executes");
    assert_eq!(range.status.code(), Some(1));
    let range: serde_json::Value =
        serde_json::from_slice(&range.stdout).expect("valid refusal JSON");
    assert_eq!(range["changed"], false);
    assert_eq!(range["error"]["kind"], "ambiguous_target");
    assert_eq!(fs::read(workbook.path()).expect("read source"), source);

    let absent = marksheet()
        .arg("set")
        .arg(workbook.path())
        .args(["inputs!Z99", "1"])
        .output()
        .expect("CLI executes");
    assert_eq!(absent.status.code(), Some(1));
    let absent: serde_json::Value =
        serde_json::from_slice(&absent.stdout).expect("valid refusal JSON");
    assert_eq!(absent["error"]["kind"], "absent_cell");
    assert_eq!(fs::read(workbook.path()).expect("read source"), source);
}

#[test]
fn automation_failures_keep_versioned_envelopes_and_requested_modes() {
    let source = fs::read(workspace_file("examples/budget.ms")).expect("read fixture");
    let workbook = TempFile::write("automation-errors.ms", &source);

    let invalid_table = marksheet()
        .arg("append-table-row")
        .arg(workbook.path())
        .arg("Not-A-Table")
        .arg("--value")
        .arg("x")
        .output()
        .expect("CLI executes");
    assert_eq!(invalid_table.status.code(), Some(1));
    let invalid_table: serde_json::Value =
        serde_json::from_slice(&invalid_table.stdout).expect("valid edit refusal JSON");
    assert_eq!(invalid_table["version"], "marksheet-edit@1");
    assert_eq!(invalid_table["error"]["kind"], "invalid_identifier");

    let invalid_target = marksheet()
        .arg("get")
        .arg(workbook.path())
        .args(["missing_target", "--calculated", "false"])
        .output()
        .expect("CLI executes");
    assert_eq!(invalid_target.status.code(), Some(1));
    let invalid_target: serde_json::Value =
        serde_json::from_slice(&invalid_target.stdout).expect("valid get refusal JSON");
    assert_eq!(invalid_target["calculated"], false);
    assert_eq!(invalid_target["error"]["kind"], "invalid_target");

    let resource_limit = marksheet()
        .arg("get")
        .arg(workbook.path())
        .arg("inputs!A1:XFD1048576")
        .output()
        .expect("CLI executes");
    assert_eq!(resource_limit.status.code(), Some(1));
    let resource_limit: serde_json::Value =
        serde_json::from_slice(&resource_limit.stdout).expect("valid get refusal JSON");
    assert_eq!(resource_limit["error"]["kind"], "resource_limit");
}

#[test]
fn inspect_preserves_recovered_structure_and_authored_table_order() {
    let source = b"#!marksheet 0.1\n@sheet s \"S\"\n@table z A1 csv\nH\n1\n@end\n@table a C1 csv\nH\n2\n@end\n@row 0 height=20\n";
    let workbook = TempFile::write("automation-recovery.ms", source);
    let output = marksheet()
        .arg("inspect")
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert_eq!(output.status.code(), Some(1));
    let output: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid inspect JSON");
    assert_eq!(output["status"], "invalid");
    assert_eq!(output["workbook"]["sheets"][0]["authored_cell_count"], 4);
    assert_eq!(output["workbook"]["sheets"][0]["tables"][0]["id"], "z");
    assert_eq!(output["workbook"]["sheets"][0]["tables"][1]["id"], "a");
    assert!(
        output["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
}

#[test]
fn automation_refuses_oversized_source_before_parsing() {
    let workbook = TempFile::sparse("automation-oversized.ms", 32 * 1024 * 1024 + 1);
    let output = marksheet()
        .arg("inspect")
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert_eq!(output.status.code(), Some(1));
    let output: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid inspect refusal JSON");
    assert_eq!(output["version"], "marksheet-inspect@1");
    assert_eq!(output["error"]["kind"], "resource_limit");
    assert_eq!(output["source"]["byte_length"], 32 * 1024 * 1024 + 1);
    assert_eq!(output["source"]["fnv1a64"], serde_json::Value::Null);
}

#[test]
fn json_format_reports_its_own_exact_guarded_patch() {
    let source = fs::read(workspace_file("tests/roundtrip/canonical_mixed_input.ms"))
        .expect("read noncanonical fixture");
    let workbook = TempFile::write("automation-format.ms", &source);

    let check = marksheet()
        .args(["fmt", "--check", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert_eq!(check.status.code(), Some(1));
    let check: serde_json::Value =
        serde_json::from_slice(&check.stdout).expect("valid format JSON");
    assert_eq!(check["version"], "marksheet-format@1");
    assert_eq!(check["status"], "needs_format");
    assert_eq!(check["changed"], false);
    assert_eq!(check["would_change"], true);
    assert!(check.get("proposal_patches").is_none());
    assert!(check.get("diagnostics_source").is_none());
    assert_eq!(check["patches"].as_array().map(Vec::len), Some(0));
    assert_eq!(fs::read(workbook.path()).expect("read source"), source);

    let format = marksheet()
        .args(["fmt", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert!(format.status.success(), "stderr: {}", text(&format.stderr));
    let format: serde_json::Value =
        serde_json::from_slice(&format.stdout).expect("valid format JSON");
    assert_eq!(format["status"], "ok");
    assert_eq!(format["changed"], true);
    assert!(format.get("proposal_patches").is_none());
    assert!(format.get("diagnostics_source").is_none());
    assert_eq!(format["patches"][0]["start"], 0);
    assert_eq!(format["patches"][0]["end"], source.len());
    assert_eq!(
        format["patches"][0]["replacement"]
            .as_str()
            .map(str::as_bytes),
        Some(
            fs::read(workbook.path())
                .expect("read formatted source")
                .as_slice()
        )
    );
}

#[test]
fn json_format_leaves_a1_shaped_identifiers_valid() {
    let source = b"#!marksheet 0.1\n@style h2 bold=true\n@sheet q1 \"Q1\"\n@table data1 A1 csv\nItem,Total\nRent,5\n@end\n@apply data1[Total] h2\n";
    let workbook = TempFile::write("automation-format-identifiers.ms", source);

    let format = marksheet()
        .args(["fmt", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    let format: serde_json::Value =
        serde_json::from_slice(&format.stdout).expect("valid format JSON");
    assert_eq!(format["status"], "ok");
    assert_eq!(format["valid"], true);

    let formatted = fs::read(workbook.path()).expect("read formatted source");
    let formatted = String::from_utf8(formatted).expect("formatted source is UTF-8");
    assert!(formatted.contains("@sheet q1 \"Q1\"\n"), "{formatted}");
    assert!(formatted.contains("@table data1 A1 csv\n"), "{formatted}");
    assert!(
        formatted.contains("@apply data1[Total] h2\n"),
        "{formatted}"
    );

    // A formatting envelope that reports `ok` must leave a workbook `check`
    // still accepts; the identifiers above are the case that silently became
    // invalid upper-case spellings.
    let check = marksheet()
        .args(["check", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert!(check.status.success(), "stdout: {}", text(&check.stdout));
}

#[test]
fn committed_edit_reports_post_edit_extension_failures() {
    let source =
        fs::read(workspace_file("tests/extensions/assertions_success.ms")).expect("read fixture");
    let workbook = TempFile::write("automation-assertion.ms", &source);

    let output = marksheet()
        .arg("set")
        .arg(workbook.path())
        .args(["inputs!A2", "1"])
        .output()
        .expect("CLI executes");
    assert_eq!(output.status.code(), Some(1));
    let output: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid edit JSON");
    assert_eq!(output["status"], "committed_invalid");
    assert_eq!(output["changed"], true);
    assert_eq!(output["valid"], false);
    assert_eq!(output["diagnostics"][0]["code"], "MS3201");
    assert_ne!(
        fs::read(workbook.path()).expect("read edited source"),
        source
    );

    let repeated = marksheet()
        .arg("set")
        .arg(workbook.path())
        .args(["inputs!A2", "1"])
        .output()
        .expect("CLI executes");
    assert_eq!(repeated.status.code(), Some(1));
    let repeated: serde_json::Value =
        serde_json::from_slice(&repeated.stdout).expect("valid no-op edit JSON");
    assert_eq!(repeated["status"], "invalid");
    assert_eq!(repeated["changed"], false);
    assert_eq!(repeated["valid"], false);
}

#[test]
fn json_format_still_formats_a_workbook_with_a_failing_assertion() {
    // A failed trusted assertion is an authoring outcome, not a formatter
    // defect: it holds identically before and after the rewrite, so the
    // result guard must not refuse to format the workbook.
    let source = b"#!marksheet 0.1\n@use assertions@1\n@sheet inputs \"Inputs\"\n@block A1 csv\nValue,Text\n5,hello\n@end\n@extension assertions@1 \"checks\"\nassert A2 > 9\n@end\n";
    let workbook = TempFile::write("automation-format-assertion.ms", source);

    let format = marksheet()
        .args(["fmt", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    let format: serde_json::Value =
        serde_json::from_slice(&format.stdout).expect("valid format JSON");
    assert_eq!(format["status"], "ok");
    assert_eq!(format["changed"], true);
    assert_eq!(format["error"], serde_json::Value::Null);
    // The assertion still fails, and the envelope must keep saying so.
    assert_eq!(format["valid"], false);
    assert_ne!(
        fs::read(workbook.path()).expect("read formatted source"),
        source
    );

    // The same document already canonical must reach the same verdict; a
    // validity claim may not depend on whether formatting changed bytes.
    let again = marksheet()
        .args(["fmt", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    let again: serde_json::Value =
        serde_json::from_slice(&again.stdout).expect("valid format JSON");
    assert_eq!(again["status"], "ok");
    assert_eq!(again["changed"], false);
    assert_eq!(again["valid"], false);
}

#[test]
fn json_format_locates_diagnostics_in_the_source_it_reports() {
    // The undeclared-instance warning sits after the blank line canonical
    // formatting inserts, so a stale line index reports a position that
    // belongs to neither the original nor the formatted workbook.
    let source = b"#!marksheet 0.1\n@book locale=\"en-US\" timezone=\"UTC\" formula-profile=\"portable-a1@1\"\n@sheet s \"S\"\n@block A1 csv\nValue\n5\n@end\n\n@extension assertions@1 \"checks\"\nassert A2 >= 0\n@end\n";
    let workbook = TempFile::write("automation-format-spans.ms", source);

    let format = marksheet()
        .args(["fmt", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    let format: serde_json::Value =
        serde_json::from_slice(&format.stdout).expect("valid format JSON");
    assert_eq!(format["changed"], true);

    let check = marksheet()
        .args(["check", "--format", "json"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    let check: serde_json::Value = serde_json::from_slice(&check.stdout).expect("valid check JSON");

    assert_eq!(format["diagnostics"], check["diagnostics"]);
}

fn marksheet() -> Command {
    Command::new(env!("CARGO_BIN_EXE_marksheet"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn workspace_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn unique_path(name: &str) -> PathBuf {
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "marksheet-cli-{name}-{}-{sequence}",
        std::process::id()
    ))
}

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn write(name: &str, contents: &[u8]) -> Self {
        let path = unique_path(name);
        fs::write(&path, contents).expect("write temporary fixture");
        Self { path }
    }

    fn sparse(name: &str, length: u64) -> Self {
        let path = unique_path(name);
        let file = fs::File::create(&path).expect("create sparse fixture");
        file.set_len(length).expect("size sparse fixture");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
