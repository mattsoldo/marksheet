use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[test]
fn xlsx_export_is_deterministic_and_uses_a_sibling_default() {
    let temporary = TempDirectory::new("xlsx-determinism");
    let source = temporary.path().join("core.ms");
    fs::copy(fixture("sources/core.ms"), &source).expect("copy source fixture");

    let first = convert(&source, "xlsx", &[]);
    assert!(first.status.success(), "stderr: {}", text(&first.stderr));
    let report = report(&first);
    assert_eq!(report["schema"], "marksheet-conversion@1");
    assert_eq!(report["destination"]["format"], "xlsx");
    assert_ne!(report["fidelity"], "unsupported");
    let default_output = source.with_extension("xlsx");
    let first_bytes = fs::read(&default_output).expect("default XLSX artifact exists");
    assert!(first_bytes.starts_with(b"PK"));

    let second_output = temporary.path().join("second.xlsx");
    let second = convert(
        &source,
        "xlsx",
        &["--output", second_output.to_str().expect("UTF-8 path")],
    );
    assert!(second.status.success(), "stderr: {}", text(&second.stderr));
    assert_eq!(first.stdout, second.stdout, "reports are deterministic");
    assert_eq!(
        first_bytes,
        fs::read(second_output).expect("second XLSX artifact exists")
    );
}

#[test]
fn xlsx_round_trip_produces_valid_calculable_marksheet() {
    let temporary = TempDirectory::new("xlsx-roundtrip");
    let xlsx = temporary.path().join("core.xlsx");
    let export = convert(
        &fixture("sources/core.ms"),
        "xlsx",
        &["--output", xlsx.to_str().expect("UTF-8 path")],
    );
    assert!(export.status.success(), "stderr: {}", text(&export.stderr));

    let imported = temporary.path().join("imported.ms");
    let import = convert(
        &xlsx,
        "marksheet",
        &["--output", imported.to_str().expect("UTF-8 path")],
    );
    assert!(import.status.success(), "stderr: {}", text(&import.stderr));
    let import_report = report(&import);
    assert_eq!(import_report["source"]["format"], "xlsx");
    assert_eq!(import_report["destination"]["format"], "marksheet");

    let check = marksheet()
        .arg("check")
        .arg(&imported)
        .output()
        .expect("CLI executes");
    assert!(check.status.success(), "stderr: {}", text(&check.stderr));

    let calc = marksheet()
        .args(["calc", "--sheet", "summary", "--range", "A1:B3"])
        .arg(imported)
        .output()
        .expect("CLI executes");
    assert!(calc.status.success(), "stderr: {}", text(&calc.stderr));
    let calculation: serde_json::Value =
        serde_json::from_slice(&calc.stdout).expect("calculation JSON");
    assert_eq!(calculation["selection"]["sheet"], "summary");
}

#[test]
fn csv_export_requires_one_explicit_selection_and_reports_omissions() {
    let temporary = TempDirectory::new("csv-selection");
    let missing_output = temporary.path().join("missing.csv");
    let missing = convert(
        &fixture("sources/core.ms"),
        "csv",
        &["--output", missing_output.to_str().expect("UTF-8 path")],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(!missing_output.exists());
    let missing_report = report(&missing);
    assert_eq!(missing_report["fidelity"], "unsupported");
    assert_eq!(missing_report["diagnostics"][0]["code"], "MS4103");

    let selected_output = temporary.path().join("selected.csv");
    let selected = convert(
        &fixture("sources/core.ms"),
        "csv",
        &[
            "--output",
            selected_output.to_str().expect("UTF-8 path"),
            "--sheet",
            "summary",
            "--range",
            "A1:B3",
        ],
    );
    assert!(
        selected.status.success(),
        "stderr: {}",
        text(&selected.stderr)
    );
    let selected_report = report(&selected);
    assert_eq!(selected_report["schema"], "marksheet-conversion@1");
    assert_eq!(selected_report["fidelity"], "lossy");
    assert!(
        selected_report["outcomes"]
            .as_array()
            .expect("outcomes")
            .iter()
            .any(|outcome| outcome["feature"] == "selected_range")
    );
    let csv = fs::read_to_string(selected_output).expect("selected CSV artifact");
    assert!(csv.starts_with("Metric,Value\n"), "CSV: {csv}");
}

#[test]
fn csv_import_requires_and_honors_an_explicit_target() {
    let temporary = TempDirectory::new("csv-import");
    let missing_output = temporary.path().join("missing.ms");
    let missing = convert(
        &fixture("sources/import.csv"),
        "marksheet",
        &["--output", missing_output.to_str().expect("UTF-8 path")],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(!missing_output.exists());
    assert_eq!(report(&missing)["diagnostics"][0]["code"], "MS4104");

    let output = temporary.path().join("imported.ms");
    let imported = convert(
        &fixture("sources/import.csv"),
        "marksheet",
        &[
            "--output",
            output.to_str().expect("UTF-8 path"),
            "--sheet",
            "imported",
            "--label",
            "Imported",
            "--table",
            "sales",
            "--anchor",
            "A1",
        ],
    );
    assert!(
        imported.status.success(),
        "stderr: {}",
        text(&imported.stderr)
    );
    assert_eq!(report(&imported)["fidelity"], "lossless");
    let source = fs::read_to_string(&output).expect("imported Marksheet source");
    assert!(source.contains("@sheet imported \"Imported\""));
    assert!(source.contains("@table sales A1 csv"));
    let check = marksheet()
        .arg("check")
        .arg(output)
        .output()
        .expect("CLI executes");
    assert!(check.status.success(), "stderr: {}", text(&check.stderr));
}

#[test]
fn explicit_csv_target_does_not_mislabel_later_serialization_failure() {
    let temporary = TempDirectory::new("csv-invalid-formula");
    let source = temporary.path().join("invalid-formula.csv");
    fs::write(&source, b"=1+\n").expect("write malformed formula CSV");
    let output = temporary.path().join("invalid-formula.ms");

    let result = convert(
        &source,
        "marksheet",
        &[
            "--output",
            output.to_str().expect("UTF-8 path"),
            "--sheet",
            "imported",
            "--label",
            "Imported",
            "--range",
            "A1",
        ],
    );

    assert_eq!(result.status.code(), Some(1));
    assert!(!output.exists());
    let report = report(&result);
    assert_eq!(report["fidelity"], "unsupported");
    assert_eq!(report["diagnostics"][0]["code"], "MS4105");
    assert_ne!(report["outcomes"][0]["feature"], "csv_import_target");
    assert!(
        report["diagnostics"][0]["message"]
            .as_str()
            .expect("diagnostic message")
            .contains("MS2202")
    );
}

#[test]
fn conversion_errors_do_not_replace_an_existing_destination() {
    let temporary = TempDirectory::new("invalid-no-write");
    let invalid = temporary.path().join("invalid.ms");
    fs::write(
        &invalid,
        b"#!marksheet 0.1\n@require unavailable@1\n@sheet s \"Sheet\"\n",
    )
    .expect("write invalid source");
    let output = temporary.path().join("protected.xlsx");
    fs::write(&output, b"existing destination").expect("write sentinel");

    let result = convert(
        &invalid,
        "xlsx",
        &["--output", output.to_str().expect("UTF-8 path")],
    );
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        fs::read(output).expect("read sentinel"),
        b"existing destination"
    );
    let report = report(&result);
    assert_eq!(report["fidelity"], "unsupported");
    assert!(
        report["diagnostics"][0]["message"]
            .as_str()
            .expect("message")
            .contains("MS3101")
    );
}

#[cfg(unix)]
#[test]
fn conversion_refuses_a_symbolic_link_destination() {
    use std::os::unix::fs::symlink;

    let temporary = TempDirectory::new("symlink-output");
    let target = temporary.path().join("target.xlsx");
    fs::write(&target, b"target sentinel").expect("write target");
    let link = temporary.path().join("link.xlsx");
    symlink(&target, &link).expect("create symlink");

    let result = convert(
        &fixture("sources/core.ms"),
        "xlsx",
        &["--output", link.to_str().expect("UTF-8 path")],
    );
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(text(&result.stderr).contains("refusing to replace symbolic-link destination"));
    assert_eq!(fs::read(target).expect("read target"), b"target sentinel");
}

/// A circular reference and an unresolved name evaluate to `#CIRC!` and
/// `#NAME?` per SPEC section 13, so conversion carries them and reports the
/// loss. `--strict` is the opt-in that refuses them instead.
#[test]
fn evaluation_error_formulas_convert_by_default_and_are_refused_under_strict() {
    let temporary = TempDirectory::new("strict-evaluation-errors");
    let source = temporary.path().join("errors.ms");
    fs::write(
        &source,
        "#!marksheet 0.1\n\n@sheet data \"Data\"\n@block A1 csv\n=B1+1\n@end\n@block B1 csv\n=A1+1\n@end\n@block A3 csv\n=missing_name*2\n@end\n",
    )
    .expect("write source");

    let lenient = convert(&source, "xlsx", &["--output", out(&temporary, "a.xlsx")]);
    assert!(
        lenient.status.success(),
        "stderr: {}",
        text(&lenient.stderr)
    );

    let strict = convert(
        &source,
        "xlsx",
        &["--strict", "--output", out(&temporary, "b.xlsx")],
    );
    assert!(!strict.status.success(), "--strict must refuse");
    let detail = report(&strict)["outcomes"][0]["detail"]
        .as_str()
        .expect("detail")
        .to_owned();
    assert!(
        detail.contains("MS2303") || detail.contains("MS2103"),
        "{detail}"
    );
}

/// Import `--strict` refuses any source the report already calls lossy, which
/// is most real workbooks: it is a gate for callers that need an exact carry.
#[test]
fn strict_import_refuses_a_lossy_source() {
    let temporary = TempDirectory::new("strict-import");
    let source = temporary.path().join("advanced.ms");
    fs::copy(fixture("sources/advanced.ms"), &source).expect("copy fixture");
    let xlsx = out(&temporary, "advanced.xlsx");
    assert!(
        convert(&source, "xlsx", &["--output", xlsx])
            .status
            .success()
    );

    let xlsx_path = temporary.path().join("advanced.xlsx");
    let lenient = convert(
        &xlsx_path,
        "marksheet",
        &["--output", out(&temporary, "a.ms")],
    );
    assert!(
        lenient.status.success(),
        "stderr: {}",
        text(&lenient.stderr)
    );
    assert_eq!(report(&lenient)["fidelity"], "lossy");

    let strict = convert(
        &xlsx_path,
        "marksheet",
        &["--strict", "--output", out(&temporary, "b.ms")],
    );
    assert!(
        !strict.status.success(),
        "--strict must refuse a lossy import"
    );
    assert_eq!(report(&strict)["fidelity"], "lossy");
    assert!(
        !temporary.path().join("b.ms").exists(),
        "--strict must not write an artifact"
    );
}

fn out(temporary: &TempDirectory, name: &str) -> &'static str {
    Box::leak(
        temporary
            .path()
            .join(name)
            .to_str()
            .expect("UTF-8 path")
            .to_owned()
            .into_boxed_str(),
    )
}

#[test]
fn convert_refuses_to_overwrite_input_spelled_relative_to_absolute() {
    let temporary = TempDirectory::new("overwrite-guard-relative");
    let source = temporary.path().join("core.ms");
    fs::copy(fixture("sources/core.ms"), &source).expect("copy source fixture");
    let original = fs::read(&source).expect("read original source");

    // `source` is an absolute path; `--output ./core.ms` names the same file
    // through a relative spelling resolved against the process's working
    // directory. Literal PathBuf equality would miss this.
    let mut command = marksheet();
    command
        .current_dir(temporary.path())
        .arg("convert")
        .arg("--to")
        .arg("xlsx")
        .arg("--output")
        .arg("./core.ms")
        .arg(&source);
    let result = command.output().expect("CLI executes");

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(text(&result.stderr).contains("refusing to overwrite conversion input"));
    assert_eq!(
        fs::read(&source).expect("read source after refusal"),
        original,
        "input must be untouched"
    );
}

#[cfg(unix)]
#[test]
fn convert_refuses_to_overwrite_input_via_symlinked_alias() {
    use std::os::unix::fs::symlink;

    let temporary = TempDirectory::new("overwrite-guard-symlink");
    let source = temporary.path().join("core.ms");
    fs::copy(fixture("sources/core.ms"), &source).expect("copy source fixture");
    let original = fs::read(&source).expect("read original source");
    let alias = temporary.path().join("alias.ms");
    symlink(&source, &alias).expect("create symlink alias");

    // The input is given through a symlinked alias; the output is the real
    // path the alias resolves to. Canonicalization must collapse both to the
    // same file even though neither argument is a literal path match.
    let result = convert(
        &alias,
        "xlsx",
        &["--output", source.to_str().expect("UTF-8 path")],
    );

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(text(&result.stderr).contains("refusing to overwrite conversion input"));
    assert_eq!(
        fs::read(&source).expect("read source after refusal"),
        original,
        "input must be untouched"
    );
}

#[test]
fn convert_allows_distinct_files_that_share_a_name_in_different_directories() {
    let temporary = TempDirectory::new("overwrite-guard-distinct");
    let source_dir = temporary.path().join("source");
    let output_dir = temporary.path().join("output");
    fs::create_dir(&source_dir).expect("create source directory");
    fs::create_dir(&output_dir).expect("create output directory");
    let source = source_dir.join("core.ms");
    fs::copy(fixture("sources/core.ms"), &source).expect("copy source fixture");
    let output = output_dir.join("core.ms");

    let result = convert(
        &source,
        "xlsx",
        &["--output", output.to_str().expect("UTF-8 path")],
    );

    assert!(result.status.success(), "stderr: {}", text(&result.stderr));
    assert!(
        output.exists(),
        "distinct same-named output must be written"
    );
}

fn convert(source: &Path, target: &str, extra: &[&str]) -> Output {
    let mut command = marksheet();
    command.arg("convert").arg("--to").arg(target);
    command.args(extra).arg(source);
    command.output().expect("CLI executes")
}

fn report(output: &Output) -> serde_json::Value {
    assert!(output.stderr.is_empty(), "stderr: {}", text(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("conversion report JSON")
}

fn marksheet() -> Command {
    Command::new(env!("CARGO_BIN_EXE_marksheet"))
}

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/conversion")
        .join(relative)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(name: &str) -> Self {
        let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "marksheet-cli-{name}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
