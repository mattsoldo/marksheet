//! Black-box conformance coverage for the public syntax API.
//!
//! The fixture corpus is deliberately kept outside this crate so the CLI and
//! parser share one specification.  These tests exercise the parser directly:
//! a successful CLI check must not be the only guard against a syntax-library
//! regression.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use marksheet_model::{Severity, SheetItem};
use marksheet_syntax::{canonicalize, lossless_bytes, parse};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_paths(directory: &str) -> Vec<PathBuf> {
    let directory = repository_root().join(directory);
    let mut paths = fs::read_dir(&directory)
        .unwrap_or_else(|error| {
            panic!(
                "could not read fixture directory {}: {error}",
                directory.display()
            )
        })
        .map(|entry| {
            entry
                .expect("fixture directory entry must be readable")
                .path()
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "ms"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "fixture directory {} unexpectedly contains no Marksheet sources",
        directory.display()
    );
    paths
}

fn source(path: &Path) -> Vec<u8> {
    fs::read(path)
        .unwrap_or_else(|error| panic!("could not read fixture {}: {error}", path.display()))
}

fn sidecar_codes(path: &Path) -> Vec<String> {
    let sidecar = path.with_extension("diagnostics");
    fs::read_to_string(&sidecar)
        .unwrap_or_else(|error| {
            panic!(
                "could not read diagnostics sidecar {}: {error}",
                sidecar.display()
            )
        })
        .lines()
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn diagnostic_codes(document: &marksheet_syntax::ParsedDocument) -> Vec<String> {
    document
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str().to_owned())
        .collect()
}

fn code_counts(codes: &[String]) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for code in codes {
        *counts.entry(code.as_str()).or_insert(0) += 1;
    }
    counts
}

fn assert_expected_codes_are_present(path: &Path, expected: &[String], actual: &[String]) {
    let expected_counts = code_counts(expected);
    let actual_counts = code_counts(actual);
    for (code, expected_count) in expected_counts {
        let actual_count = actual_counts.get(code).copied().unwrap_or_default();
        assert!(
            actual_count >= expected_count,
            "{} is missing expected diagnostic {code}: expected at least {expected_count}, got {actual_count}; all diagnostics: {actual:?}",
            path.display()
        );
    }
}

fn assert_canonicalizes_cleanly(path: &Path, original: &[u8]) {
    let document = parse(original);
    assert!(
        !document.has_errors(),
        "{} should parse without errors before canonicalization: {:?}",
        path.display(),
        diagnostic_codes(&document)
    );
    assert_eq!(
        lossless_bytes(&document),
        original,
        "{} must preserve every source byte during a lossless parse",
        path.display()
    );

    let canonical = canonicalize(&document).unwrap_or_else(|diagnostics| {
        panic!("{} should canonicalize: {diagnostics:?}", path.display())
    });
    assert!(
        !canonical.contains(&b'\r'),
        "{} canonical output must use LF line endings and contain no carriage return",
        path.display()
    );

    let reparsed = parse(&canonical);
    assert!(
        !reparsed.has_errors(),
        "canonical output from {} must reparse without errors: {:?}",
        path.display(),
        diagnostic_codes(&reparsed)
    );
    let canonical_again = canonicalize(&reparsed).unwrap_or_else(|diagnostics| {
        panic!(
            "canonical output from {} must canonicalize again: {diagnostics:?}",
            path.display()
        )
    });
    assert_eq!(
        canonical_again,
        canonical,
        "canonicalization must be idempotent for {}",
        path.display()
    );
}

#[test]
fn valid_conformance_diagnostics_match_sidecars_exactly() {
    for path in fixture_paths("tests/conformance/valid") {
        let document = parse(&source(&path));
        let actual = diagnostic_codes(&document);
        assert_eq!(
            actual,
            sidecar_codes(&path),
            "{} diagnostics must exactly match its sidecar, including warnings",
            path.display()
        );
        assert!(
            !document.has_errors(),
            "valid fixture {} emitted errors: {actual:?}",
            path.display()
        );
    }
}

#[test]
fn invalid_conformance_diagnostics_cover_sidecars_deterministically() {
    for path in fixture_paths("tests/conformance/invalid") {
        let input = source(&path);
        let document = parse(&input);
        let actual = diagnostic_codes(&document);
        assert!(
            document.has_errors(),
            "invalid fixture {} must emit at least one error; diagnostics: {actual:?}",
            path.display()
        );
        assert_expected_codes_are_present(&path, &sidecar_codes(&path), &actual);
        assert_eq!(
            diagnostic_codes(&parse(&input)),
            actual,
            "{} diagnostics must have a stable code order across parses",
            path.display()
        );
    }
}

#[test]
fn all_valid_fixtures_are_lossless_and_canonicalizable() {
    let mut fixtures = fixture_paths("tests/conformance/valid");
    fixtures.extend(fixture_paths("tests/roundtrip"));
    for path in fixtures {
        let input = source(&path);
        assert_canonicalizes_cleanly(&path, &input);
    }
}

#[test]
fn canonical_roundtrip_pair_matches_fixture() {
    let input_path = repository_root().join("tests/roundtrip/canonical_mixed_input.ms");
    let expected_path =
        repository_root().join("tests/roundtrip/canonical_mixed_input.canonical.ms");
    let document = parse(&source(&input_path));
    assert!(
        !document.has_errors(),
        "{} should parse cleanly: {:?}",
        input_path.display(),
        diagnostic_codes(&document)
    );
    assert_eq!(
        canonicalize(&document).expect("canonical mixed input must format"),
        source(&expected_path),
        "canonical round-trip output must match {}",
        expected_path.display()
    );
}

#[test]
fn crlf_input_is_lossless_but_canonical_output_is_lf() {
    let path = repository_root().join("tests/roundtrip/crlf_input.ms");
    let input = source(&path);
    assert!(input.windows(2).any(|bytes| bytes == b"\r\n"));
    let document = parse(&input);
    assert_eq!(lossless_bytes(&document), input);
    let canonical = canonicalize(&document).expect("valid CRLF fixture must canonicalize");
    assert!(!canonical.contains(&b'\r'));
}

#[test]
fn unknown_extension_fixture_stays_byte_identical_when_saved_losslessly() {
    let path = repository_root().join("tests/roundtrip/lossless_unknown_extension.ms");
    let input = source(&path);
    let document = parse(&input);
    assert!(
        !document.has_errors(),
        "unknown optional extensions may warn but must remain parseable: {:?}",
        diagnostic_codes(&document)
    );
    assert_eq!(lossless_bytes(&document), input);
}

#[test]
fn sparse_fixture_stores_only_declared_cells() {
    let path = repository_root().join("tests/conformance/valid/sparse_blocks.ms");
    let document = parse(&source(&path));
    assert!(!document.has_errors(), "{:?}", diagnostic_codes(&document));
    let workbook = document
        .workbook
        .expect("valid sparse fixture must lower to a workbook");
    let stored_cells = workbook
        .sheets
        .iter()
        .flat_map(|sheet| &sheet.items)
        .map(|item| match item {
            SheetItem::Block(block) => block.cells.iter().map(Vec::len).sum::<usize>(),
            SheetItem::Table(table) => table.block.cells.iter().map(Vec::len).sum::<usize>(),
            SheetItem::Fill(_)
            | SheetItem::Apply(_)
            | SheetItem::ColumnGeometry(_)
            | SheetItem::RowGeometry(_)
            | SheetItem::Extension(_) => 0,
        })
        .sum::<usize>();

    assert_eq!(
        stored_cells, 2,
        "only the two declared fields should be stored"
    );
    assert!(
        stored_cells < 1_000,
        "storage must remain proportional to declared fields rather than the million-row coordinate gap"
    );
}

#[test]
fn malformed_utf8_is_diagnosed_without_losing_bytes() {
    let input = b"#!marksheet 0.1\n@sheet main \"Main\"\n\xff";
    let document = parse(input);
    let codes = diagnostic_codes(&document);
    assert!(document.has_errors());
    assert!(
        codes.iter().any(|code| code == "MS1003"),
        "expected MS1003, got {codes:?}"
    );
    assert_eq!(lossless_bytes(&document), input);
    assert!(
        canonicalize(&document).is_err(),
        "invalid UTF-8 must not be rewritten"
    );
}

#[test]
fn utf8_bom_is_diagnosed_without_losing_bytes() {
    let input = b"\xef\xbb\xbf#!marksheet 0.1\n@sheet main \"Main\"\n";
    let document = parse(input);
    let codes = diagnostic_codes(&document);
    assert!(document.has_errors());
    assert!(
        codes.iter().any(|code| code == "MS1002"),
        "expected MS1002, got {codes:?}"
    );
    assert_eq!(lossless_bytes(&document), input);
    assert!(
        canonicalize(&document).is_err(),
        "a BOM must not be rewritten implicitly"
    );
}

#[test]
fn warning_only_documents_are_still_canonicalizable() {
    let document = parse(&source(
        &repository_root().join("tests/conformance/valid/all_core.ms"),
    ));
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Warning)
    );
    assert!(!document.has_errors());
    assert!(canonicalize(&document).is_ok());
}
