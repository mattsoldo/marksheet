use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[test]
fn diff_ignores_source_formatting_and_equivalent_formula_spelling() {
    let old = TempFile::write(
        "diff-old.ms",
        b"#!marksheet 0.1\n@use archive@1\n@sheet s \"Sheet\"\n@block A1 csv\n= sum ( 1.0 )\n@end\n",
    );
    let new = TempFile::write(
        "diff-new.ms",
        b"#!marksheet 0.1\n\n# a source-only comment\n@use archive@1\n@sheet s \"Sheet\"\n@block A1 csv\n=SUM(1)\n@end\n",
    );

    let human = diff(&old, &new).output().expect("CLI executes");
    assert!(human.status.success(), "stderr: {}", text(&human.stderr));
    assert!(human.stdout.is_empty());
    assert!(human.stderr.is_empty());

    let json = diff(&old, &new)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    assert!(json.status.success(), "stderr: {}", text(&json.stderr));
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(envelope["version"], "marksheet-diff@1");
    assert_eq!(envelope["profile"], "portable-a1@1");
    assert_eq!(envelope["equivalent"], true);
    assert_eq!(envelope["change_count"], 0);
    assert_eq!(envelope["changes"], serde_json::json!([]));
}

#[test]
fn diff_ignores_block_split_before_a_sheet_extension() {
    // Same authored cells (A1=1, B1=2) followed by the same sheet-scoped
    // extension; only the number of `@block` directives differs. `@block`
    // boundaries are not semantic, so this must diff as equivalent.
    let old = TempFile::write(
        "diff-block-split-old.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n1,2\n@end\n@extension archive@1 \"meta\"\n  owner=finance\n@end\n",
    );
    let new = TempFile::write(
        "diff-block-split-new.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n1\n@end\n@block B1 csv\n2\n@end\n@extension archive@1 \"meta\"\n  owner=finance\n@end\n",
    );

    let human = diff(&old, &new).output().expect("CLI executes");
    assert!(human.status.success(), "stderr: {}", text(&human.stderr));
    assert!(human.stdout.is_empty());
    assert!(human.stderr.is_empty());

    let json = diff(&old, &new)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    assert!(json.status.success(), "stderr: {}", text(&json.stderr));
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(envelope["equivalent"], true);
    assert_eq!(envelope["change_count"], 0);
    assert_eq!(envelope["changes"], serde_json::json!([]));
}

#[test]
fn diff_reports_cell_formula_and_label_changes_in_stable_order() {
    let old = TempFile::write(
        "diff-old.ms",
        b"#!marksheet 0.1\n@sheet s \"Old label\"\n@block A1 csv\n1,=1\n@end\n",
    );
    let new = TempFile::write(
        "diff-new.ms",
        b"#!marksheet 0.1\n@sheet s \"New label\"\n@block A1 csv\n2,=2\n@end\n",
    );

    let human = diff(&old, &new).output().expect("CLI executes");
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stderr.is_empty());
    assert_eq!(
        text(&human.stdout),
        "changed sheet label s: \"Old label\" -> \"New label\"\nchanged cell s!A1: number 1 -> number 2\nchanged cell s!B1: formula =1 -> formula =2\n"
    );

    let json = diff(&old, &new)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    assert_eq!(json.status.code(), Some(1));
    assert!(json.stderr.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(envelope["version"], "marksheet-diff@1");
    assert_eq!(envelope["equivalent"], false);
    assert_eq!(envelope["change_count"], 2);
    assert_eq!(envelope["changes"][0]["kind"], "sheet_label_changed");
    assert_eq!(envelope["changes"][1]["kind"], "cells_changed");
    assert_eq!(envelope["changes"][1]["cells"][0]["coordinate"], "A1");
    assert_eq!(envelope["changes"][1]["cells"][1]["after"]["value"], "=2");
}

#[test]
fn diff_identifies_table_renames_at_their_stable_sheet_item_placement() {
    let old = TempFile::write(
        "diff-table-old.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@table old_costs A1 csv\nItem,Cost\nRent,100\n@end\n",
    );
    let new = TempFile::write(
        "diff-table-new.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@table new_costs A1 csv\nItem,Cost\nRent,100\n@end\n",
    );

    let human = diff(&old, &new).output().expect("CLI executes");
    assert_eq!(human.status.code(), Some(1));
    assert!(text(&human.stdout).contains(
        "changed sheet item s[0]: table old_costs at A1 (2 headers, 1 data rows) -> table new_costs at A1 (2 headers, 1 data rows)"
    ));

    let json = diff(&old, &new)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    let item = &envelope["changes"]
        .as_array()
        .expect("changes array")
        .iter()
        .find(|change| change["kind"] == "sheet_items_changed")
        .expect("sheet item change")["items"][0];
    assert_eq!(item["index"], 0);
    assert_eq!(item["before"]["kind"], "table");
    assert_eq!(item["before"]["id"], "old_costs");
    assert_eq!(item["after"]["id"], "new_costs");
    assert_eq!(item["after"]["anchor"], "A1");
}

#[test]
fn diff_sheet_items_include_targets_geometry_and_extension_identity() {
    let old = TempFile::write(
        "diff-items-old.ms",
        b"#!marksheet 0.1\n@use archive@1\n@style primary bold=true\n@style secondary italic=true\n@sheet s \"Sheet\"\n@table costs A1 csv\nItem,Cost\nRent,\n@end\n@fill costs[Cost] =1\n@apply costs[Cost] primary\n@column A width=10\n@row 1 height=20\n@extension archive@1 \"metadata\"\n  owner=finance\n@end\n",
    );
    let new = TempFile::write(
        "diff-items-new.ms",
        b"#!marksheet 0.1\n@use archive@1\n@style primary bold=true\n@style secondary italic=true\n@sheet s \"Sheet\"\n@table costs A1 csv\nItem,Cost\nRent,\n@end\n@fill costs[Cost] =2\n@apply costs[Cost] secondary\n@column A width=11\n@row 1 height=21\n@extension archive@1 \"metadata_v2\"\n  owner=finance\n@end\n",
    );

    let output = diff(&old, &new)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    let items = envelope["changes"]
        .as_array()
        .expect("changes array")
        .iter()
        .find(|change| change["kind"] == "sheet_items_changed")
        .expect("sheet item change")["items"]
        .as_array()
        .expect("item changes");

    let fill = item_with_kind(items, "fill");
    assert_eq!(fill["before"]["target"]["kind"], "table_column");
    assert_eq!(fill["before"]["target"]["table"], "costs");
    assert_eq!(fill["before"]["target"]["header"], "Cost");
    assert_eq!(fill["after"]["formula"], "=2");

    let style_effects = envelope["changes"]
        .as_array()
        .expect("changes array")
        .iter()
        .find(|change| change["kind"] == "style_effects_changed")
        .expect("style effect change");
    let apply = &style_effects["components"][0];
    // `table[Column]` targets table data only; the header cell B1 is excluded.
    assert_eq!(apply["before"]["effects"][0]["range"], "B2");
    assert_eq!(apply["before"]["effects"][0]["properties"]["bold"], true);
    assert_eq!(apply["after"]["effects"][0]["properties"]["italic"], true);

    let columns = item_with_kind(items, "column_geometry");
    assert_eq!(
        columns["before"]["columns"],
        serde_json::json!({ "start": 1, "end": 1 })
    );
    assert_eq!(columns["after"]["width"], 11.0);

    let rows = item_with_kind(items, "row_geometry");
    assert_eq!(
        rows["before"]["rows"],
        serde_json::json!({ "start": 1, "end": 1 })
    );
    assert_eq!(rows["after"]["height"], 21.0);

    let extension = item_with_kind(items, "extension");
    assert_eq!(extension["before"]["capability"], "archive@1");
    assert_eq!(extension["before"]["name"], "metadata");
    assert_eq!(extension["after"]["name"], "metadata_v2");
    assert!(extension["before"].get("payload").is_none());
}

#[test]
fn diff_refuses_invalid_inputs_without_emitting_a_partial_change_report() {
    let valid = TempFile::write(
        "diff-valid.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n1\n@end\n",
    );
    let invalid = TempFile::write("diff-invalid.ms", b"#!marksheet invalid\n");

    let human = diff(&valid, &invalid).output().expect("CLI executes");
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stdout.is_empty());
    assert!(text(&human.stderr).contains("error[MS1001]"));

    let json = diff(&valid, &invalid)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    assert_eq!(json.status.code(), Some(1));
    assert!(json.stderr.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(envelope["version"], "marksheet-diff@1");
    assert_eq!(envelope["status"], "invalid");
    assert!(envelope.get("changes").is_none());
    assert_eq!(envelope["diagnostics"][0]["code"], "MS1001");
}

#[test]
fn diff_json_is_deterministic_and_read_failures_use_exit_two() {
    let old = TempFile::write(
        "diff-old.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n1\n@end\n",
    );
    let new = TempFile::write(
        "diff-new.ms",
        b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n2\n@end\n",
    );

    let first = diff(&old, &new)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    let second = diff(&old, &new)
        .args(["--format", "json"])
        .output()
        .expect("CLI executes");
    assert_eq!(first.status.code(), Some(1));
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.stderr, second.stderr);

    let missing = unique_path("missing.ms");
    let failure = marksheet()
        .arg("diff")
        .arg(&old.path)
        .arg(&missing)
        .output()
        .expect("CLI executes");
    assert_eq!(failure.status.code(), Some(2));
    assert!(failure.stdout.is_empty());
    assert!(text(&failure.stderr).contains("could not read"));
}

fn item_with_kind<'a>(items: &'a [serde_json::Value], kind: &str) -> &'a serde_json::Value {
    items
        .iter()
        .find(|item| item["before"]["kind"] == kind)
        .unwrap_or_else(|| panic!("expected {kind} item"))
}

fn diff(old: &TempFile, new: &TempFile) -> Command {
    let mut command = marksheet();
    command.arg("diff").arg(&old.path).arg(&new.path);
    command
}

fn marksheet() -> Command {
    Command::new(env!("CARGO_BIN_EXE_marksheet"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
