//! Canonical encoding of one edited CSV field.
//!
//! This module intentionally works at the field boundary.  It does not decide
//! where a cell lives in a document or rewrite surrounding rows; that is the
//! patch planner's responsibility.

use std::fmt::{self, Write as _};

use marksheet_model::{CanonicalNumberError, Value, canonical_number};

/// Context which affects the legal spelling of an otherwise independent CSV
/// field.
///
/// A Marksheet `@end` line terminates a CSV payload only when it is the sole
/// field of a physical record.  Callers replacing a field must therefore pass
/// the record shape rather than trying to infer it from the field value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FieldContext {
    /// The record contains another field before or after this field.
    #[default]
    DelimitedRecord,
    /// This is the only field in its physical record.
    SoleFieldRecord,
}

/// A value cannot be represented by the portable Marksheet CSV scalar syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// Marksheet source numbers are finite binary64 values only.
    NonFiniteNumber,
    /// Dates require a four-digit, non-negative year.
    DateOutsidePortableRange,
    /// RFC 3339 source offsets have minute precision.
    DateTimeOffsetHasSeconds,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteNumber => formatter.write_str("Marksheet numbers must be finite"),
            Self::DateOutsidePortableRange => {
                formatter.write_str("Marksheet dates require a four-digit non-negative year")
            }
            Self::DateTimeOffsetHasSeconds => {
                formatter.write_str("Marksheet datetime offsets must have minute precision")
            }
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<CanonicalNumberError> for EncodeError {
    fn from(_: CanonicalNumberError) -> Self {
        Self::NonFiniteNumber
    }
}

/// Encodes a semantic value as the exact bytes of one CSV field.
///
/// The result contains no surrounding delimiter or record-ending newline.
/// Quoting is deliberately minimal: it is used only for RFC 4180 syntax and
/// to protect the special one-field `@end` record.
///
/// # Errors
///
/// Returns an error when a manually-constructed model value has no portable
/// Marksheet spelling (for example, a non-finite number).
pub fn encode_field(value: &Value, context: FieldContext) -> Result<Vec<u8>, EncodeError> {
    let scalar = encode_scalar(value)?;
    let must_quote = matches!(context, FieldContext::SoleFieldRecord) && scalar == "@end";
    Ok(quote_csv_field(&scalar, must_quote))
}

/// Encodes a value to its decoded CSV scalar spelling, before RFC 4180 quoting.
///
/// This is public because patch code often needs to inspect the scalar when
/// choosing a replacement span, but writes should normally use
/// [`encode_field`] so the record context is not accidentally lost.
///
/// # Errors
///
/// Returns an error when the value has no portable Marksheet scalar spelling.
pub fn encode_scalar(value: &Value) -> Result<String, EncodeError> {
    match value {
        Value::Blank => Ok(String::new()),
        Value::Text(text) => Ok(encode_text(text)),
        Value::Number(number) => Ok(canonical_number(*number)?),
        Value::Boolean(boolean) => Ok(boolean.to_string()),
        Value::Date(date) => {
            let scalar = date.to_string();
            is_portable_date(&scalar)
                .then_some(scalar)
                .ok_or(EncodeError::DateOutsidePortableRange)
        }
        Value::DateTime(datetime) => {
            let date = datetime.date().to_string();
            if !is_portable_date(&date) {
                return Err(EncodeError::DateOutsidePortableRange);
            }
            encode_datetime_components(
                &date,
                datetime.hour(),
                datetime.minute(),
                datetime.second(),
                datetime.nanosecond(),
                datetime.offset().whole_seconds(),
            )
        }
        Value::Formula(formula) => Ok(formula.as_str().to_owned()),
        Value::Error(error) => Ok(error.to_string()),
    }
}

fn encode_text(text: &str) -> String {
    // A raw text spelling is safe only if the strict scalar parser produces the
    // exact same text.  `parse_strict` matters for ISO-looking invalid dates:
    // they are text in tolerant parsing but are validation errors in source.
    let raw_is_safe = matches!(
        Value::parse_strict(text),
        Ok(Value::Text(parsed)) if parsed == text
    ) && !text.starts_with('\'')
        // Preserve the intent of text with a number-shaped spelling such as
        // `00123`. Although the current scalar grammar rejects leading zeroes,
        // emitting an apostrophe makes this stable if numeric recognition is
        // extended and matches the format's documented force-text examples.
        && !resembles_number(text);
    if raw_is_safe {
        text.to_owned()
    } else {
        format!("'{text}")
    }
}

fn resembles_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    if bytes.first() == Some(&b'-') {
        cursor += 1;
    }
    let integer_start = cursor;
    while matches!(bytes.get(cursor), Some(b'0'..=b'9')) {
        cursor += 1;
    }
    if cursor == integer_start {
        return false;
    }
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while matches!(bytes.get(cursor), Some(b'0'..=b'9')) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return false;
        }
    }
    if matches!(bytes.get(cursor), Some(b'e' | b'E')) {
        cursor += 1;
        if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
            cursor += 1;
        }
        let exponent_start = cursor;
        while matches!(bytes.get(cursor), Some(b'0'..=b'9')) {
            cursor += 1;
        }
        if cursor == exponent_start {
            return false;
        }
    }
    cursor == bytes.len()
}

fn is_portable_date(scalar: &str) -> bool {
    let bytes = scalar.as_bytes();
    bytes.len() == 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..].iter().all(u8::is_ascii_digit)
}

fn encode_datetime_components(
    date: &str,
    hour: u8,
    minute: u8,
    second: u8,
    nanoseconds: u32,
    offset_seconds: i32,
) -> Result<String, EncodeError> {
    let mut scalar = format!("{date}T{hour:02}:{minute:02}:{second:02}");
    if nanoseconds != 0 {
        let fraction = format!("{nanoseconds:09}");
        scalar.push('.');
        scalar.push_str(fraction.trim_end_matches('0'));
    }

    if offset_seconds % 60 != 0 {
        return Err(EncodeError::DateTimeOffsetHasSeconds);
    }
    if offset_seconds == 0 {
        scalar.push('Z');
    } else {
        let sign = if offset_seconds < 0 { '-' } else { '+' };
        let absolute = offset_seconds.unsigned_abs();
        scalar.push(sign);
        write!(
            scalar,
            "{:02}:{:02}",
            absolute / 3_600,
            (absolute % 3_600) / 60
        )
        .expect("writing to a String is infallible");
    }
    Ok(scalar)
}

fn quote_csv_field(scalar: &str, force: bool) -> Vec<u8> {
    if !force
        && !scalar
            .bytes()
            .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        return scalar.as_bytes().to_vec();
    }

    let mut encoded = Vec::with_capacity(scalar.len() + 2);
    encoded.push(b'"');
    for byte in scalar.bytes() {
        encoded.push(byte);
        if byte == b'"' {
            encoded.push(b'"');
        }
    }
    encoded.push(b'"');
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::{Block, CellError, FormulaSource, SheetItem};

    fn parse_one_field(encoded: &[u8]) -> Value {
        let mut source = b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@block A1 csv\n".to_vec();
        source.extend_from_slice(encoded);
        source.extend_from_slice(b"\n@end\n");
        let parsed = marksheet_syntax::parse(&source);
        assert!(
            !parsed.has_errors(),
            "{encoded:?}: {:?}",
            parsed.diagnostics
        );
        let workbook = parsed.workbook.expect("valid document has a workbook");
        let SheetItem::Block(Block { cells, .. }) = &workbook.sheets[0].items[0] else {
            panic!("expected a block")
        };
        cells[0][0].value.clone()
    }

    fn assert_round_trip(value: &Value, context: FieldContext) {
        let encoded = encode_field(value, context).expect("representable value");
        assert_eq!(parse_one_field(&encoded), *value, "encoded={encoded:?}");
    }

    #[test]
    fn encodes_each_scalar_kind_and_round_trips_through_the_parser() {
        let formula = FormulaSource::new("=SUM(A1,2)").unwrap();
        let values = [
            Value::Blank,
            Value::Text(String::new()),
            Value::Text("plain text".to_owned()),
            Value::Number(-0.0),
            Value::Number(1e20),
            Value::Boolean(true),
            Value::from_csv_field("2024-02-29"),
            Value::from_csv_field("2024-02-29T12:34:56.120000000-04:30"),
            Value::Formula(formula),
            Value::Error(CellError::DivisionByZero),
        ];
        for value in values {
            assert_round_trip(&value, FieldContext::SoleFieldRecord);
        }
        for error in [
            CellError::DivisionByZero,
            CellError::NotAvailable,
            CellError::Name,
            CellError::Number,
            CellError::Reference,
            CellError::Value,
            CellError::Circular,
        ] {
            assert_round_trip(&Value::Error(error), FieldContext::SoleFieldRecord);
        }
    }

    #[test]
    fn scalar_spelling_is_canonical_before_csv_quoting() {
        assert_eq!(encode_scalar(&Value::Blank).unwrap(), "");
        assert_eq!(encode_scalar(&Value::Text(String::new())).unwrap(), "'");
        assert_eq!(encode_scalar(&Value::Number(-0.0)).unwrap(), "-0");
        assert_eq!(encode_scalar(&Value::Number(1e20)).unwrap(), "1e20");
        assert_eq!(
            encode_scalar(&Value::from_csv_field("2024-02-29")).unwrap(),
            "2024-02-29"
        );
        assert_eq!(
            encode_scalar(&Value::from_csv_field("2024-02-29T12:34:56.120000000Z")).unwrap(),
            "2024-02-29T12:34:56.12Z"
        );
    }

    #[test]
    fn text_that_would_change_type_is_forced_with_an_apostrophe() {
        for text in [
            "",
            "'already forced",
            "=not a formula",
            "#REF!",
            "true",
            "00123",
            "2024-02-29",
            "2024-02-30",
            "2024-02-29T12:00:00",
        ] {
            let scalar = encode_scalar(&Value::Text(text.to_owned())).unwrap();
            assert!(scalar.starts_with('\''), "{text:?} -> {scalar:?}");
            assert_round_trip(&Value::Text(text.to_owned()), FieldContext::SoleFieldRecord);
        }
        assert_eq!(
            encode_scalar(&Value::Text("ordinary text".to_owned())).unwrap(),
            "ordinary text"
        );
    }

    #[test]
    fn quoting_is_minimal_and_rfc4180_compatible() {
        let cases = [
            (" leading", b" leading".as_slice()),
            ("trailing ", b"trailing ".as_slice()),
            ("a,b", b"\"a,b\"".as_slice()),
            ("a\"b", b"\"a\"\"b\"".as_slice()),
            ("a\nb", b"\"a\nb\"".as_slice()),
            ("a\rb", b"\"a\rb\"".as_slice()),
        ];
        for (text, expected) in cases {
            let encoded =
                encode_field(&Value::Text(text.to_owned()), FieldContext::DelimitedRecord).unwrap();
            assert_eq!(encoded, expected);
            assert_round_trip(&Value::Text(text.to_owned()), FieldContext::SoleFieldRecord);
        }
    }

    #[test]
    fn sole_end_field_is_quoted_but_delimited_end_field_is_not() {
        let value = Value::Text("@end".to_owned());
        assert_eq!(
            encode_field(&value, FieldContext::SoleFieldRecord).unwrap(),
            b"\"@end\""
        );
        assert_eq!(
            encode_field(&value, FieldContext::DelimitedRecord).unwrap(),
            b"@end"
        );
        assert_round_trip(&value, FieldContext::SoleFieldRecord);
    }

    #[test]
    fn representative_unicode_and_control_text_round_trips() {
        for text in [
            "\0",
            "\u{0001}\u{001f}\u{007f}",
            "\u{0085}\u{2028}\u{2029}",
            "café — 東京 — 😀",
            "\u{0301}combining",
            "\u{feff}byte-order-mark-as-text",
            "comma, quote\" CR\r LF\n",
        ] {
            assert_round_trip(&Value::Text(text.to_owned()), FieldContext::SoleFieldRecord);
        }
    }

    #[test]
    fn rejects_non_finite_numbers() {
        assert_eq!(
            encode_field(&Value::Number(f64::NAN), FieldContext::DelimitedRecord),
            Err(EncodeError::NonFiniteNumber)
        );
    }
}
