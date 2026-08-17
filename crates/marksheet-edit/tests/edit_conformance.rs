//! End-to-end assertions for the public source-edit fixture manifest.
//!
//! The shell validator in `tests/edit` checks byte-patch arithmetic without
//! depending on Rust. This test deliberately exercises the real semantic-diff
//! implementation for the corpus's equivalence claim.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use marksheet_edit::{
    diff::SemanticDiff,
    history::{EditSession, HistoryErrorKind},
    transaction::{EditErrorKind, EditOperation, EditTransaction},
};
use marksheet_model::{
    ApplyTarget, Coordinate, FormulaSource, NameId, Range, SheetId, StyleId, TableId, Value,
    Workbook,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditManifest {
    version: u8,
    cases: Vec<ManifestCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCase {
    id: String,
    kind: String,
    fixture: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticEquivalenceFixture {
    left: String,
    right: String,
    expected: EquivalenceExpectation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquivalenceExpectation {
    equivalent: bool,
    ignored: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitFixture {
    transaction: FixtureTransaction,
    before: String,
    after: String,
    patches: Vec<FixturePatch>,
    #[serde(rename = "inversePatches")]
    inverse_patches: Option<Vec<FixturePatch>>,
    assertions: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectFixture {
    transaction: FixtureTransaction,
    before: String,
    patches: Vec<FixturePatch>,
    expected: FixtureOutcome,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoOpFixture {
    transaction: FixtureTransaction,
    before: String,
    after: String,
    patches: Vec<FixturePatch>,
    expected: FixtureOutcome,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RebaseConflictFixture {
    transaction: FixtureTransaction,
    before: String,
    current: String,
    patches: Vec<FixturePatch>,
    expected: FixtureOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct FixturePatch {
    start: u64,
    end: u64,
    replacement: String,
    #[serde(default)]
    expected: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureOutcome {
    outcome: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    changed: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
enum FixtureTransaction {
    SetCell {
        sheet: String,
        coordinate: String,
        #[serde(default)]
        value: Option<JsonValue>,
        #[serde(default)]
        formula: Option<String>,
    },
    AppendTableRow {
        table: String,
        fields: Vec<JsonValue>,
    },
    RenameSheetLabel {
        sheet: String,
        label: String,
    },
    RenameSheetId {
        old: String,
        new: String,
    },
    RenameNameId {
        old: String,
        new: String,
    },
    ApplyStyle {
        sheet: String,
        target: String,
        style: String,
    },
    MoveBlock {
        sheet: String,
        source: String,
        #[serde(rename = "destinationAnchor")]
        destination_anchor: String,
    },
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/edit")
}

fn manifest() -> EditManifest {
    let path = fixture_root().join("manifest.json");
    serde_json::from_slice(&fs::read(&path).expect("read edit fixture manifest"))
        .expect("valid edit fixture manifest")
}

fn workbook(path: &Path) -> Workbook {
    let document = marksheet_syntax::parse(&fs::read(path).expect("read fixture source"));
    assert!(
        !document.has_errors(),
        "fixture {} did not parse: {:#?}",
        path.display(),
        document.diagnostics
    );
    document.workbook.expect("valid fixture workbook")
}

fn source(name: &str) -> Vec<u8> {
    fs::read(fixture_root().join(name)).expect("read fixture source")
}

fn parse_fixture<T: serde::de::DeserializeOwned>(case: &ManifestCase) -> T {
    let path = fixture_root().join(&case.fixture);
    serde_json::from_slice(&fs::read(&path).expect("read transaction fixture"))
        .expect("valid transaction fixture")
}

impl FixtureTransaction {
    fn to_edit_operation(&self) -> EditOperation {
        match self {
            Self::SetCell {
                sheet,
                coordinate,
                value,
                formula,
            } => {
                let value = match (value, formula) {
                    (Some(value), None) => fixture_value(value),
                    (None, Some(formula)) => Value::Formula(
                        FormulaSource::new(formula.clone())
                            .expect("fixture formulas start with '='"),
                    ),
                    _ => panic!("set_cell fixture must provide exactly one of value or formula"),
                };
                EditOperation::SetCell {
                    sheet: SheetId::parse(sheet).expect("valid fixture sheet id"),
                    coordinate: Coordinate::parse(coordinate)
                        .expect("valid fixture cell coordinate"),
                    value,
                }
            }
            Self::AppendTableRow { table, fields } => EditOperation::AppendTableRow {
                table: TableId::parse(table).expect("valid fixture table id"),
                fields: fields.iter().map(fixture_value).collect(),
            },
            Self::RenameSheetLabel { sheet, label } => EditOperation::RenameSheetLabel {
                sheet: SheetId::parse(sheet).expect("valid fixture sheet id"),
                label: label.clone(),
            },
            Self::RenameSheetId { old, new } => EditOperation::RenameSheetId {
                old: SheetId::parse(old).expect("valid fixture old sheet id"),
                new: SheetId::parse(new).expect("valid fixture new sheet id"),
            },
            Self::RenameNameId { old, new } => EditOperation::RenameNameId {
                old: NameId::parse(old).expect("valid fixture old name id"),
                new: NameId::parse(new).expect("valid fixture new name id"),
            },
            Self::ApplyStyle {
                sheet,
                target,
                style,
            } => EditOperation::ApplyStyle {
                sheet: SheetId::parse(sheet).expect("valid fixture sheet id"),
                target: ApplyTarget::Range(Range::parse(target).expect("valid fixture target")),
                style: StyleId::parse(style).expect("valid fixture style id"),
            },
            Self::MoveBlock {
                sheet,
                source,
                destination_anchor,
            } => EditOperation::MoveBlock {
                sheet: SheetId::parse(sheet).expect("valid fixture sheet id"),
                source: Range::parse(source).expect("valid fixture source range"),
                destination: Coordinate::parse(destination_anchor)
                    .expect("valid fixture destination coordinate"),
            },
        }
    }
}

fn fixture_value(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Blank,
        JsonValue::Bool(value) => Value::Boolean(*value),
        JsonValue::Number(value) => Value::Number(value.as_f64().expect("finite JSON number")),
        JsonValue::String(value) => Value::Text(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            panic!("fixture values must be a scalar JSON value")
        }
    }
}

fn actual_patches(patches: &[marksheet_edit::patch::SourcePatch]) -> Vec<FixturePatch> {
    patches
        .iter()
        .map(|patch| FixturePatch {
            start: patch.span.start,
            end: patch.span.end,
            replacement: String::from_utf8(patch.replacement.clone())
                .expect("fixture patch replacement is UTF-8"),
            expected: None,
        })
        .collect()
}

fn expected_error_kind(reason: &str) -> EditErrorKind {
    match reason {
        "virtual_cell" => EditErrorKind::VirtualCell,
        "partial_block_footprint" => EditErrorKind::PartialFootprint,
        other => panic!("fixture reason has no EditErrorKind mapping: {other}"),
    }
}

#[test]
fn manifest_covers_the_milestone_three_editing_proof() {
    let manifest = manifest();
    assert_eq!(manifest.version, 1);

    let expected = BTreeMap::from([
        ("apply_existing_style", "commit"),
        ("formula_field", "commit"),
        ("move_block", "commit"),
        ("no_op", "no_op"),
        ("partial_block_move_refused", "reject"),
        ("rebase_conflict", "rebase_conflict"),
        ("rename_label", "commit"),
        ("rename_label_utf8", "commit"),
        ("rename_name_id", "commit"),
        ("rename_name_id_quoted_formula", "commit"),
        ("rename_sheet_id", "commit"),
        ("rename_sheet_id_crlf", "commit"),
        ("scalar_csv_quote", "commit"),
        ("semantic_equivalence", "semantic_equivalence"),
        ("table_append", "commit"),
        ("virtual_cell_refused", "reject"),
    ]);
    let actual = manifest
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case.kind.as_str()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    assert_eq!(
        actual.len(),
        manifest.cases.len(),
        "fixture ids must be unique"
    );

    for case in &manifest.cases {
        assert!(
            fixture_root().join(&case.fixture).is_file(),
            "manifest fixture is missing: {}",
            case.fixture
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn transaction_fixtures_execute_through_the_public_edit_api() {
    for case in manifest().cases {
        match case.kind.as_str() {
            "commit" => {
                let fixture: CommitFixture = parse_fixture(&case);
                assert!(
                    !fixture.assertions.is_empty(),
                    "commit fixture {} must document its behavioral assertion",
                    case.id
                );
                let before = source(&fixture.before);
                let result = EditTransaction::single(fixture.transaction.to_edit_operation())
                    .execute(&before)
                    .unwrap_or_else(|error| panic!("commit fixture {} failed: {error}", case.id));

                assert_eq!(result.source, source(&fixture.after), "fixture {}", case.id);
                assert_eq!(
                    actual_patches(result.patches.patches()),
                    fixture.patches,
                    "fixture {} produced a different patch plan",
                    case.id
                );
                assert_eq!(
                    result
                        .inverse
                        .apply(&result.source)
                        .expect("inverse applies"),
                    before,
                    "fixture {} inverse does not restore the original bytes",
                    case.id
                );
                if let Some(expected_inverse) = fixture.inverse_patches {
                    assert_eq!(
                        actual_patches(result.inverse.patches()),
                        expected_inverse,
                        "fixture {} produced a different inverse patch plan",
                        case.id
                    );
                }
            }
            "no_op" => {
                let fixture: NoOpFixture = parse_fixture(&case);
                assert_eq!(fixture.expected.outcome, "committed");
                assert_eq!(fixture.expected.changed, Some(false));
                assert!(fixture.patches.is_empty());
                let before = source(&fixture.before);
                let result = EditTransaction::single(fixture.transaction.to_edit_operation())
                    .execute(&before)
                    .unwrap_or_else(|error| panic!("no-op fixture {} failed: {error}", case.id));
                assert!(!result.changed());
                assert!(result.patches.is_empty());
                assert!(result.inverse.is_empty());
                assert_eq!(result.source, source(&fixture.after));
            }
            "reject" => {
                let fixture: RejectFixture = parse_fixture(&case);
                assert_eq!(fixture.expected.outcome, "rejected");
                assert!(fixture.patches.is_empty());
                let before = source(&fixture.before);
                let error = EditTransaction::single(fixture.transaction.to_edit_operation())
                    .execute(&before)
                    .expect_err("rejected fixture must not commit");
                assert_eq!(
                    error.kind,
                    expected_error_kind(fixture.expected.reason.as_deref().expect("reason")),
                    "fixture {} rejected with the wrong error kind",
                    case.id
                );
                assert_eq!(before, source(&fixture.before));
            }
            "rebase_conflict" => {
                let fixture: RebaseConflictFixture = parse_fixture(&case);
                assert_eq!(fixture.expected.outcome, "conflict");
                assert_eq!(
                    fixture.expected.reason.as_deref(),
                    Some("affected_span_changed")
                );
                let before = source(&fixture.before);
                let current = source(&fixture.current);
                let transaction = EditTransaction::single(fixture.transaction.to_edit_operation());
                let mut session = EditSession::new(before.clone());
                let intent = session.intent(transaction).expect("base intent is valid");
                let error = session
                    .rebase_and_execute(&current, intent)
                    .expect_err("changed target must conflict during rebase");
                assert_eq!(error.kind, HistoryErrorKind::Conflict);
                assert_eq!(
                    session.source(),
                    before.as_slice(),
                    "conflict mutated session bytes"
                );
                assert!(
                    fixture.patches.iter().any(|patch| {
                        let expected = patch.expected.as_deref().expect("rebase patch preimage");
                        let start =
                            usize::try_from(patch.start).expect("fixture offset fits usize");
                        let end = usize::try_from(patch.end).expect("fixture offset fits usize");
                        current[start..end] != *expected.as_bytes()
                    }),
                    "fixture must change an operation precondition"
                );
            }
            "semantic_equivalence" => {}
            other => panic!("unsupported fixture kind in manifest: {other}"),
        }
    }
}

#[test]
fn semantic_equivalence_fixture_uses_the_real_semantic_diff() {
    let manifest = manifest();
    let case = manifest
        .cases
        .iter()
        .find(|case| case.id == "semantic_equivalence")
        .expect("semantic equivalence fixture is listed in the manifest");
    assert_eq!(case.kind, "semantic_equivalence");

    let path = fixture_root().join(&case.fixture);
    let fixture: SemanticEquivalenceFixture =
        serde_json::from_slice(&fs::read(&path).expect("read semantic-equivalence fixture"))
            .expect("valid semantic-equivalence fixture");
    assert!(
        fixture.expected.ignored.contains(&"comment".to_owned())
            && fixture
                .expected
                .ignored
                .contains(&"function_case".to_owned()),
        "the fixture must document the presentation-only differences it covers"
    );

    let diff = SemanticDiff::between(
        &workbook(&fixture_root().join(fixture.left)),
        &workbook(&fixture_root().join(fixture.right)),
    );
    assert_eq!(
        diff.is_empty(),
        fixture.expected.equivalent,
        "semantic equivalence fixture produced changes: {:#?}",
        diff.changes
    );
}
