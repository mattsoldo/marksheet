use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[test]
fn check_accepts_the_documented_example() {
    let output = marksheet()
        .arg("check")
        .arg(workspace_file("examples/budget.ms"))
        .output()
        .expect("CLI executes");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn check_reports_invalid_source_with_a_failure_exit() {
    let output = marksheet()
        .arg("check")
        .arg(workspace_file(
            "tests/conformance/invalid/malformed_version.ms",
        ))
        .output()
        .expect("CLI executes");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("diagnostics are UTF-8");
    assert!(stderr.contains("error[MS1001]"), "stderr: {stderr}");
    assert!(stderr.contains(":1:1:"), "stderr: {stderr}");
}

#[test]
fn check_json_includes_stable_codes_and_positions() {
    let output = marksheet()
        .args([
            "check",
            "--format",
            "json",
            workspace_file("tests/conformance/invalid/malformed_version.ms")
                .to_str()
                .expect("workspace paths are UTF-8"),
        ])
        .output()
        .expect("CLI executes");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics are valid");
    let first = &diagnostics[0];
    assert_eq!(first["code"], "MS1001");
    assert_eq!(first["severity"], "error");
    assert_eq!(first["primary"]["line"], 1);
    assert_eq!(first["primary"]["column"], 1);
}

#[test]
fn check_reports_io_failures_with_exit_code_two() {
    let missing = unique_path("missing.ms");
    let output = marksheet()
        .arg("check")
        .arg(&missing)
        .output()
        .expect("CLI executes");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("errors are UTF-8");
    assert!(stderr.contains("could not read"), "stderr: {stderr}");
}

#[test]
fn fmt_check_accepts_the_documented_example() {
    let output = marksheet()
        .args(["fmt", "--check"])
        .arg(workspace_file("examples/budget.ms"))
        .output()
        .expect("CLI executes");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fmt_check_detects_differences_and_fmt_rewrites_idempotently() {
    let source = fs::read(workspace_file("tests/roundtrip/canonical_mixed_input.ms"))
        .expect("read source fixture");
    let temporary = TempFile::write("format.ms", &source);

    let check_before = marksheet()
        .args(["fmt", "--check"])
        .arg(temporary.path())
        .output()
        .expect("CLI executes");
    assert_eq!(check_before.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&check_before.stderr).contains("not canonically formatted"));
    assert_eq!(
        fs::read(temporary.path()).expect("source remains readable"),
        source
    );

    let format = marksheet()
        .arg("fmt")
        .arg(temporary.path())
        .output()
        .expect("CLI executes");
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    let formatted = fs::read(temporary.path()).expect("formatted source is readable");
    assert_ne!(formatted, source);
    assert!(formatted.starts_with(b"#!marksheet 0.1\n"));
    assert!(!formatted.windows(2).any(|window| window == b"\r\n"));
    assert!(
        String::from_utf8(formatted.clone())
            .expect("formatter emits UTF-8")
            .contains("@block A1 csv")
    );

    let check_after = marksheet()
        .args(["fmt", "--check"])
        .arg(temporary.path())
        .output()
        .expect("CLI executes");
    assert!(check_after.status.success());

    let bytes_before_second_format = fs::read(temporary.path()).expect("read formatted source");
    let format_again = marksheet()
        .arg("fmt")
        .arg(temporary.path())
        .output()
        .expect("CLI executes");
    assert!(format_again.status.success());
    assert_eq!(
        fs::read(temporary.path()).expect("read formatted source"),
        bytes_before_second_format
    );
}

#[test]
fn fmt_rejects_invalid_source_without_rewriting_it() {
    let source = fs::read(workspace_file(
        "tests/conformance/invalid/malformed_version.ms",
    ))
    .expect("read invalid fixture");
    let temporary = TempFile::write("invalid-format.ms", &source);

    let output = marksheet()
        .arg("fmt")
        .arg(temporary.path())
        .output()
        .expect("CLI executes");

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("error[MS1001]"));
    assert_eq!(
        fs::read(temporary.path()).expect("source remains readable"),
        source
    );
}

#[cfg(unix)]
#[test]
fn fmt_refuses_symbolic_links_without_touching_the_target() {
    use std::os::unix::fs::symlink;

    let target_source = fs::read(workspace_file("tests/roundtrip/canonical_mixed_input.ms"))
        .expect("read source fixture");
    let target = TempFile::write("symlink-target.ms", &target_source);
    let link = unique_path("symlink-source.ms");
    symlink(target.path(), &link).expect("create symbolic link");
    let link_cleanup = TempFile { path: link.clone() };

    let output = marksheet()
        .arg("fmt")
        .arg(&link)
        .output()
        .expect("CLI executes");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to format symbolic link"));
    assert!(
        fs::symlink_metadata(&link)
            .expect("link remains present")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(target.path()).expect("target remains readable"),
        target_source
    );

    drop(link_cleanup);
}

fn marksheet() -> Command {
    Command::new(env!("CARGO_BIN_EXE_marksheet"))
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

#[allow(dead_code)]
struct TempFile {
    path: PathBuf,
}

#[allow(dead_code)]
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
