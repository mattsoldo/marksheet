use marksheet_edit::{
    history::{EditSession, HistoryErrorKind},
    inverse::InverseEditErrorKind,
    transaction::{EditErrorKind, EditOperation, EditTransaction},
};
use marksheet_model::{Coordinate, SheetId, Value};
use marksheet_syntax::ParseOptions;

const REQUIRED_EXTENSION_SOURCE: &[u8] = b"#!marksheet 0.1\n\
@require assertions@1\n\
\n\
@sheet data \"Data\"\n\
@block A1 csv\n\
Value\n\
1\n\
@end\n";

fn set_value(value: f64) -> EditTransaction {
    EditTransaction::single(EditOperation::SetCell {
        sheet: SheetId::parse("data").expect("valid sheet id"),
        coordinate: Coordinate::parse("A2").expect("valid coordinate"),
        value: Value::Number(value),
    })
}

fn options(extension: &str) -> ParseOptions {
    ParseOptions {
        supported_extensions: vec![extension.to_owned()],
    }
}

fn diagnostic_codes(diagnostics: &[marksheet_model::Diagnostic]) -> Vec<&str> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

#[test]
fn exact_host_extension_id_validates_both_sides_of_an_atomic_edit() {
    let transaction = set_value(2.0);

    let default_error = transaction.execute(REQUIRED_EXTENSION_SOURCE).unwrap_err();
    assert_eq!(default_error.kind, EditErrorKind::InvalidBase);
    assert_eq!(diagnostic_codes(&default_error.diagnostics), ["MS3101"]);

    let exact = options("assertions@1");
    let edited = transaction
        .execute_with_parse_options(REQUIRED_EXTENSION_SOURCE, &exact)
        .expect("the exact installed capability validates base and result");
    assert!(edited.source.windows(3).any(|window| window == b"\n2\n"));
    assert!(edited.diagnostics.is_empty());

    let wrong_major = options("assertions@2");
    let mismatch = transaction
        .execute_with_parse_options(REQUIRED_EXTENSION_SOURCE, &wrong_major)
        .unwrap_err();
    assert_eq!(mismatch.kind, EditErrorKind::InvalidBase);
    assert_eq!(diagnostic_codes(&mismatch.diagnostics), ["MS3101"]);
}

#[test]
fn option_aware_session_executes_undoes_and_redoes_with_one_capability_set() {
    let exact = options("assertions@1");
    let mut session = EditSession::new_with_parse_options(REQUIRED_EXTENSION_SOURCE, &exact);
    assert_eq!(
        session.parse_options().supported_extensions,
        ["assertions@1"]
    );

    let edited = session.execute(set_value(2.0)).expect("execute");
    assert_eq!(session.source(), edited.source);
    assert_eq!(session.undo_len(), 1);

    let restored = session.undo_edit().expect("undo");
    assert_eq!(restored.source, REQUIRED_EXTENSION_SOURCE);
    assert_eq!(session.undo_len(), 0);
    assert_eq!(session.redo_len(), 1);

    let redone = session.redo_edit().expect("redo");
    assert_eq!(redone.source, edited.source);
    assert_eq!(session.undo_len(), 1);
    assert_eq!(session.redo_len(), 0);

    let default_inverse_error = edited
        .inverse_transaction
        .execute(&edited.source)
        .unwrap_err();
    assert_eq!(
        default_inverse_error.kind,
        InverseEditErrorKind::InvalidResult
    );
    assert_eq!(
        diagnostic_codes(default_inverse_error.diagnostics()),
        ["MS3101"]
    );
    assert_eq!(
        edited
            .inverse_transaction
            .execute_with_parse_options(&edited.source, &exact)
            .expect("standalone inverse uses host options")
            .source,
        REQUIRED_EXTENSION_SOURCE
    );
}

#[test]
fn major_mismatch_leaves_session_source_and_history_unchanged() {
    let wrong_major = options("assertions@2");
    let mut session = EditSession::new_with_parse_options(REQUIRED_EXTENSION_SOURCE, &wrong_major);
    let before = session.source().to_vec();

    let error = session.execute(set_value(2.0)).unwrap_err();
    assert_eq!(error.kind, HistoryErrorKind::InvalidSource);
    assert_eq!(diagnostic_codes(error.diagnostics()), ["MS3101"]);
    assert_eq!(session.source(), before);
    assert_eq!(session.undo_len(), 0);
    assert_eq!(session.redo_len(), 0);
}
