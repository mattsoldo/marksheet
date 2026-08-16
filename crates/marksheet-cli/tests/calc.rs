use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[test]
fn calc_renders_the_documented_example_in_all_supported_formats() {
    let workbook = workspace_file("examples/budget.ms");

    let json = marksheet()
        .args(["calc", "--sheet", "summary", "--range", "B2:B4"])
        .arg(&workbook)
        .output()
        .expect("CLI executes");
    assert!(json.status.success(), "stderr: {}", text(&json.stderr));
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(json["version"], "marksheet-calc@1");
    assert_eq!(json["selection"]["sheet"], "summary");
    assert_eq!(json["selection"]["range"], "B2:B4");
    assert_eq!(json["cells"][0]["coordinate"], "B2");
    assert_eq!(json["cells"][0]["value"]["kind"], "number");
    assert_eq!(json["cells"][0]["value"]["value"], 2060.0);
    assert_eq!(json["cells"][1]["value"]["value"], 686.666_666_666_666_6);
    assert_eq!(json["cells"][2]["value"]["value"], 1648.0);
    assert!(json["stats"].get("dirty_cells").is_none());
    assert!(json["stats"].get("evaluated_cells").is_none());
    assert!(json["stats"].get("dirty_cell_count").is_some());
    assert!(json["stats"].get("evaluated_cell_count").is_some());

    let csv = marksheet()
        .args([
            "calc", "--sheet", "summary", "--range", "B2:B4", "--format", "csv",
        ])
        .arg(&workbook)
        .output()
        .expect("CLI executes");
    assert!(csv.status.success(), "stderr: {}", text(&csv.stderr));
    assert_eq!(text(&csv.stdout), "2060\n686.6666666666666\n1648\n");

    let text_output = marksheet()
        .args([
            "calc", "--sheet", "summary", "--range", "A1:B2", "--format", "text",
        ])
        .arg(&workbook)
        .output()
        .expect("CLI executes");
    assert!(
        text_output.status.success(),
        "stderr: {}",
        text(&text_output.stderr)
    );
    assert_eq!(
        text(&text_output.stdout),
        "summary!A1:B2\nMetric\t|\tValue\nTotal costs\t|\t2060\n"
    );
}

#[test]
fn calc_preserves_blank_and_empty_text_as_distinct_json_values() {
    let workbook = TempFile::write(
        "blank-and-empty-text.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n,'\n@end\n",
    );

    let json = marksheet()
        .args(["calc", "--sheet", "s", "--range", "A1:B1"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert!(json.status.success(), "stderr: {}", text(&json.stderr));
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(json["cells"][0]["value"]["kind"], "blank");
    assert_eq!(json["cells"][1]["value"]["kind"], "text");
    assert_eq!(json["cells"][1]["value"]["value"], "");

    let csv = marksheet()
        .args([
            "calc", "--sheet", "s", "--range", "A1:B1", "--format", "csv",
        ])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert!(csv.status.success(), "stderr: {}", text(&csv.stderr));
    assert_eq!(text(&csv.stdout), ",\n");
}

#[test]
fn calc_csv_quotes_selected_text_values() {
    let workbook = TempFile::write(
        "csv-output.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n\"a,b\",\"say \"\"hi\"\"\"\n@end\n",
    );

    let output = marksheet()
        .args([
            "calc", "--sheet", "s", "--range", "A1:B1", "--format", "csv",
        ])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");

    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "\"a,b\",\"say \"\"hi\"\"\"\n");
}

#[test]
fn calc_csv_neutralizes_text_that_csv_consumers_may_interpret_as_formulas() {
    let workbook = TempFile::write(
        "csv-formula-injection.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n\"=\"\"=sum(1,1)\"\"\",\"=\"\"+sum(1,1)\"\"\",\"=\"\"-sum(1,1)\"\"\",\"=\"\"@sum(1,1)\"\"\",\"=\"\" \t=sum(1,1)\"\"\"\n@end\n",
    );

    let output = marksheet()
        .args([
            "calc", "--sheet", "s", "--range", "A1:E1", "--format", "csv",
        ])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");

    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "\"'=sum(1,1)\",\"'+sum(1,1)\",\"'-sum(1,1)\",\"'@sum(1,1)\",\"' \t=sum(1,1)\"\n"
    );
}

#[test]
fn calc_text_escapes_controls_without_splitting_selected_rows() {
    let workbook = TempFile::write(
        "text-controls.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n\"has\ttab\",\"line one\nline two\",\"carriage\rreturn\"\n@end\n",
    );

    let output = marksheet()
        .args([
            "calc", "--sheet", "s", "--range", "A1:C1", "--format", "text",
        ])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");

    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "s!A1:C1\n\"has\\ttab\"\t|\t\"line one\\nline two\"\t|\t\"carriage\\rreturn\"\n"
    );
}

#[test]
fn calc_reports_cycles_but_returns_the_selected_error_value() {
    let workbook = TempFile::write(
        "cycle.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n=A1\n@end\n",
    );

    let output = marksheet()
        .args(["calc", "--sheet", "s", "--range", "A1"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");

    assert_eq!(output.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(json["cells"][0]["value"]["kind"], "error");
    assert_eq!(json["cells"][0]["value"]["value"], "#CIRC!");
    assert!(text(&output.stderr).contains("error[MS2303]"));
}

#[test]
fn calc_returns_executable_unresolved_reference_errors_with_diagnostics() {
    let workbook = TempFile::write(
        "unresolved-name.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n=unknown_name\n@end\n",
    );
    let output = marksheet()
        .args(["calc", "--sheet", "s", "--range", "A1"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert_eq!(output.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(json["cells"][0]["value"]["kind"], "error");
    assert_eq!(json["cells"][0]["value"]["value"], "#NAME?");
    assert_eq!(json["diagnostics"][0]["code"], "MS2103");
    assert!(text(&output.stderr).contains("error[MS2103]"));
}

#[test]
fn calc_rejects_syntax_errors_and_missing_selectors_without_stdout() {
    let workbook = TempFile::write(
        "invalid-source.ms",
        b"#!marksheet broken\n@sheet s \"Sheet\"\n",
    );
    let invalid = marksheet()
        .args(["calc", "--sheet", "s", "--range", "A1"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stdout.is_empty());
    assert!(text(&invalid.stderr).contains("error[MS1001]"));

    let missing_range = marksheet()
        .args(["calc", "--sheet", "s"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert_eq!(missing_range.status.code(), Some(2));
    assert!(missing_range.stdout.is_empty());

    let missing_sheet = marksheet()
        .args(["calc", "--range", "A1"])
        .arg(workbook.path())
        .output()
        .expect("CLI executes");
    assert_eq!(missing_sheet.status.code(), Some(2));
    assert!(missing_sheet.stdout.is_empty());
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

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
