//! Explicit canonical formatting for validated documents.

use marksheet_calc::formula::{ParseLimits, format_formula, parse};
use marksheet_model::{Coordinate, Range, Value, canonical_number};

use crate::ParsedDocument;
use crate::cst::{CsvBlock, Directive, ExtensionBlock, Node};
use crate::diagnostic::Diagnostic;
use crate::tokens::{split_target_and_rest, split_tokens};

/// Canonicalizes a valid document. Invalid documents are returned unchanged by
/// omission: callers receive the diagnostics and can refuse a destructive
/// rewrite.
///
/// # Errors
///
/// Returns the document diagnostics when an error makes canonical rewriting
/// unsafe.
pub fn canonicalize(document: &ParsedDocument) -> Result<Vec<u8>, Vec<Diagnostic>> {
    if document.has_errors() {
        return Err(document.diagnostics.clone());
    }
    let source = document.source_bytes();
    let mut output = Vec::with_capacity(source.len());
    for node in &document.cst.nodes {
        match node {
            Node::Header(_) => output.extend_from_slice(b"#!marksheet 0.1\n"),
            Node::Comment(line) => push_line(&mut output, &source[line.content.range()]),
            Node::Blank(_) => output.push(b'\n'),
            Node::Recovery(_) => unreachable!("valid documents contain no recovery nodes"),
            Node::Directive(directive) => {
                if text(source, directive.name) == "sheet" {
                    ensure_one_blank_line(&mut output);
                }
                format_directive(&mut output, source, directive);
            }
            Node::CsvBlock(block) => format_csv_block(&mut output, source, block),
            Node::Extension(extension) => format_extension(&mut output, source, extension),
        }
    }
    while output.last() == Some(&b'\n') {
        output.pop();
    }
    output.push(b'\n');
    Ok(output)
}

fn ensure_one_blank_line(output: &mut Vec<u8>) {
    while output.ends_with(b"\n\n") {
        output.pop();
    }
    if !output.is_empty() && output.last() == Some(&b'\n') {
        output.push(b'\n');
    }
}

fn push_line(output: &mut Vec<u8>, content: &[u8]) {
    output.extend_from_slice(content);
    output.push(b'\n');
}

fn format_directive(output: &mut Vec<u8>, source: &[u8], directive: &Directive) {
    let name = text(source, directive.name);
    output.push(b'@');
    output.extend_from_slice(name.as_bytes());
    let arguments = text(source, directive.arguments);
    if !arguments.is_empty() {
        output.push(b' ');
        output.extend_from_slice(format_arguments(name, arguments).as_bytes());
    }
    output.push(b'\n');
}

fn format_csv_block(output: &mut Vec<u8>, source: &[u8], block: &CsvBlock) {
    format_directive(output, source, &block.directive);
    for record in &block.records {
        for (index, field) in record.fields.iter().enumerate() {
            if index != 0 {
                output.push(b',');
            }
            let scalar = canonical_scalar(&Value::from_csv_field(&field.decoded));
            let must_quote_end = record.fields.len() == 1 && scalar == "@end";
            output.extend_from_slice(csv_quote(&scalar, must_quote_end).as_bytes());
        }
        output.push(b'\n');
    }
    output.extend_from_slice(b"@end\n");
}

fn format_extension(output: &mut Vec<u8>, source: &[u8], extension: &ExtensionBlock) {
    format_directive(output, source, &extension.directive);
    normalize_line_endings(output, &source[extension.payload.range()]);
    output.extend_from_slice(b"@end\n");
}

fn normalize_line_endings(output: &mut Vec<u8>, bytes: &[u8]) {
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                output.push(b'\n');
                index += 1;
                if bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
}

fn format_arguments(name: &str, arguments: &str) -> String {
    match name {
        "name" => arguments.split_once(" = ").map_or_else(
            || arguments.to_owned(),
            |(id, target)| format!("{id} = {}", canonical_target(target)),
        ),
        "fill" => split_target_and_rest(arguments).map_or_else(
            || arguments.to_owned(),
            |(target, formula)| {
                format!(
                    "{} {}",
                    canonical_target(target),
                    canonical_formula(formula)
                )
            },
        ),
        "column" => format_geometry(arguments, canonical_column_range),
        "row" => format_geometry(arguments, canonical_row_range),
        _ => canonical_tokens(arguments, |index, token| {
            if holds_identifier(name, index) {
                token.to_owned()
            } else {
                canonical_token(token)
            }
        }),
    }
}

/// Reports whether an argument position holds a stable identifier rather than
/// a cell target.
///
/// `canonical_target` rewrites A1-shaped text to its upper-case spelling, which
/// is correct for anchors and references but would rewrite identifiers such as
/// `q1` or `data1` into spellings the `[a-z][a-z0-9_]*` identifier grammar
/// rejects, turning a valid workbook into an invalid one.
fn holds_identifier(directive: &str, index: usize) -> bool {
    match directive {
        "sheet" | "table" | "style" => index == 0,
        "apply" => index > 0,
        _ => false,
    }
}

fn format_geometry(arguments: &str, format_target: fn(&str) -> Option<String>) -> String {
    canonical_tokens(arguments, |index, token| {
        if index == 0 {
            if let Some(formatted) = format_target(token) {
                return formatted;
            }
        }
        canonical_token(token)
    })
}

/// Rewrites each argument token, leaving arguments that cannot be tokenized
/// untouched rather than dropping them.
fn canonical_tokens(arguments: &str, format_token: impl Fn(usize, &str) -> String) -> String {
    split_tokens(arguments).map_or_else(
        |_| arguments.to_owned(),
        |tokens| {
            tokens
                .iter()
                .enumerate()
                .map(|(index, token)| format_token(index, token.text))
                .collect::<Vec<_>>()
                .join(" ")
        },
    )
}

fn canonical_column_range(value: &str) -> Option<String> {
    let (first, second) = value.split_once(':').unwrap_or((value, value));
    let first = Coordinate::parse(&format!("{first}1")).ok()?;
    let second = Coordinate::parse(&format!("{second}1")).ok()?;
    let start = Coordinate {
        column: first.column.min(second.column),
        row: 1,
    }
    .column_name();
    let end = Coordinate {
        column: first.column.max(second.column),
        row: 1,
    }
    .column_name();
    Some(if start == end {
        start
    } else {
        format!("{start}:{end}")
    })
}

fn canonical_row_range(value: &str) -> Option<String> {
    let (first, second) = value.split_once(':').unwrap_or((value, value));
    let first = first.parse::<u64>().ok()?;
    let second = second.parse::<u64>().ok()?;
    let start = first.min(second);
    let end = first.max(second);
    Some(if start == end {
        start.to_string()
    } else {
        format!("{start}:{end}")
    })
}

fn canonical_token(token: &str) -> String {
    // A quoted argument is one JSON string, so an `=` inside it never starts a
    // property and must not be split off as a key.
    if token.starts_with('"') {
        return canonical_json_string(token);
    }
    if let Some((key, value)) = token.split_once('=') {
        return format!("{key}={}", canonical_property_value(value));
    }
    canonical_target(token)
}

fn canonical_property_value(value: &str) -> String {
    if value.starts_with('"') {
        return canonical_json_string(value);
    }
    if let Ok(number) = value.parse::<f64>() {
        if number.is_finite() {
            return number.to_string();
        }
    }
    value.to_owned()
}

fn canonical_json_string(value: &str) -> String {
    serde_json::from_str::<String>(value)
        .ok()
        .and_then(|decoded| serde_json::to_string(&decoded).ok())
        .unwrap_or_else(|| value.to_owned())
}

fn canonical_target(target: &str) -> String {
    if let Some((sheet, range)) = target.split_once('!') {
        if let Ok(range) = Range::parse(range) {
            return format!("{sheet}!{range}");
        }
    }
    if let Ok(range) = Range::parse(target) {
        return range.to_string();
    }
    target.to_owned()
}

fn canonical_scalar(value: &Value) -> String {
    match value {
        Value::Blank => String::new(),
        Value::Text(text) => {
            if matches!(Value::from_csv_field(text), Value::Text(ref parsed) if parsed == text)
                && !text.starts_with('\'')
            {
                text.clone()
            } else {
                format!("'{text}")
            }
        }
        Value::Number(number) => canonical_number(*number)
            .expect("validated Marksheet source values contain only finite numbers"),
        Value::Boolean(boolean) => boolean.to_string(),
        Value::Date(date) => date.to_string(),
        Value::DateTime(datetime) => datetime
            .format(&time::format_description::well_known::Rfc3339)
            .expect("semantic datetimes are representable as RFC 3339"),
        Value::Formula(formula) => canonical_formula(formula.as_str()),
        Value::Error(error) => error.to_string(),
    }
}

/// Formats a formula only after the Marksheet-owned parser has accepted it.
///
/// Canonical formatting must never turn malformed source into a different,
/// plausible formula. Formula diagnostics normally make the enclosing document
/// ineligible for formatting, but retaining the original spelling here keeps
/// this boundary safe if a caller uses a partially validated document.
fn canonical_formula(source: &str) -> String {
    let Ok(formula) = parse(source, &ParseLimits::default()) else {
        return source.to_owned();
    };
    format_formula(&formula).unwrap_or_else(|_| source.to_owned())
}

fn csv_quote(value: &str, force: bool) -> String {
    if force
        || value
            .bytes()
            .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn text(source: &[u8], span: crate::cst::Span) -> &str {
    std::str::from_utf8(&source[span.range()]).expect("valid documents have UTF-8 CST spans")
}

#[cfg(test)]
mod tests {
    use super::canonicalize;
    use crate::parse;

    fn canonical(source: &[u8]) -> Vec<u8> {
        let document = parse(source);
        canonicalize(&document).expect("valid document")
    }

    #[test]
    fn a1_shaped_identifiers_survive_canonical_formatting() {
        let source = b"#!marksheet 0.1\n@style h2 bold=true\n\n@sheet q1 \"Q1\"\n@table data1 A1 csv\nItem,Total\nRent,5\n@end\n@apply data1[Total] h2\n";

        let formatted = canonical(source);
        let text = String::from_utf8(formatted).expect("canonical output is UTF-8");

        assert!(text.contains("@sheet q1 \"Q1\"\n"), "{text}");
        assert!(text.contains("@table data1 A1 csv\n"), "{text}");
        assert!(text.contains("@style h2 bold=true\n"), "{text}");
        assert!(text.contains("@apply data1[Total] h2\n"), "{text}");
        assert!(
            !parse(text.as_bytes()).has_errors(),
            "canonical formatting must not invalidate a valid workbook: {text}"
        );
    }

    #[test]
    fn block_anchors_are_still_canonicalized_beside_identifiers() {
        let source = b"#!marksheet 0.1\n@sheet q1 \"Q1\"\n@table data1 b2 csv\nItem\nRent\n@end\n";

        let formatted = canonical(source);
        let text = String::from_utf8(formatted).expect("canonical output is UTF-8");

        assert!(text.contains("@table data1 B2 csv\n"), "{text}");
    }

    #[test]
    fn canonicalizes_cell_formulas_and_requotes_csv_fields() {
        let source = b"#!marksheet 0.1\n@sheet main \"Main\"\n@block A1 csv\n\"=sum ( a1 , $b$2 )\",\"= \"\"a,b\"\" & a1\",=sum ( costs[Unit Cost] )\n@end\n";

        let formatted = canonical(source);

        assert_eq!(
            formatted,
            b"#!marksheet 0.1\n\n@sheet main \"Main\"\n@block A1 csv\n\"=SUM(A1,$B$2)\",\"=\"\"a,b\"\"&A1\",=SUM(costs[Unit Cost])\n@end\n"
        );
    }

    #[test]
    fn canonicalizes_fill_formulas_without_changing_targets() {
        let source = b"#!marksheet 0.1\n@sheet main \"Main\"\n@block A1 csv\nInput,Output\n1,\n@end\n@fill B2 = sum ( a2 , 1 )\n";

        let formatted = canonical(source);

        assert!(
            String::from_utf8_lossy(&formatted).contains("@fill B2 =SUM(A2,1)\n"),
            "{}",
            String::from_utf8_lossy(&formatted)
        );
    }

    #[test]
    fn formula_canonicalization_is_idempotent() {
        let source =
            b"#!marksheet 0.1\n@sheet main \"Main\"\n@block A1 csv\n= sum ( a1 , 1 )\n@end\n";

        let once = canonical(source);
        let twice = canonical(&once);

        assert_eq!(once, twice);
    }

    #[test]
    fn malformed_formula_never_rewrites_to_a_different_formula() {
        let source = b"#!marksheet 0.1\n@sheet main \"Main\"\n@block A1 csv\n=SUM(\n@end\n";
        let document = parse(source);

        match canonicalize(&document) {
            Ok(formatted) => assert!(
                String::from_utf8_lossy(&formatted).contains("=SUM(\n"),
                "{formatted:?}"
            ),
            Err(_) => assert_eq!(document.source_bytes(), source),
        }
    }
}
