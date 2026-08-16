//! Explicit canonical formatting for validated documents.

use marksheet_model::{Coordinate, Range, Value};

use crate::ParsedDocument;
use crate::cst::{CsvBlock, Directive, ExtensionBlock, Node};
use crate::diagnostic::Diagnostic;

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
        "fill" => split_target(arguments).map_or_else(
            || arguments.to_owned(),
            |(target, formula)| format!("{} {formula}", canonical_target(target)),
        ),
        "column" => format_geometry(arguments, canonical_column_range),
        "row" => format_geometry(arguments, canonical_row_range),
        _ => split_space_tokens(arguments)
            .into_iter()
            .map(canonical_token)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn format_geometry(arguments: &str, format_target: fn(&str) -> Option<String>) -> String {
    let mut tokens = split_space_tokens(arguments);
    if let Some(target) = tokens.first_mut() {
        let formatted = format_target(target);
        if let Some(formatted) = formatted {
            return std::iter::once(formatted)
                .chain(tokens[1..].iter().map(|token| canonical_token(token)))
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    tokens
        .into_iter()
        .map(canonical_token)
        .collect::<Vec<_>>()
        .join(" ")
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
    if let Some((key, value)) = token.split_once('=') {
        return format!("{key}={}", canonical_property_value(value));
    }
    if token.starts_with('"') {
        return serde_json::from_str::<String>(token)
            .ok()
            .and_then(|value| serde_json::to_string(&value).ok())
            .unwrap_or_else(|| token.to_owned());
    }
    canonical_target(token)
}

fn canonical_property_value(value: &str) -> String {
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value)
            .ok()
            .and_then(|decoded| serde_json::to_string(&decoded).ok())
            .unwrap_or_else(|| value.to_owned());
    }
    if let Ok(number) = value.parse::<f64>() {
        if number.is_finite() {
            return number.to_string();
        }
    }
    value.to_owned()
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

/// Tokenizes directive arguments while retaining JSON strings and structured
/// references containing spaces as single tokens.
fn split_space_tokens(arguments: &str) -> Vec<&str> {
    let bytes = arguments.as_bytes();
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut brackets = 0_u32;
    while index <= bytes.len() {
        let boundary =
            index == bytes.len() || (bytes[index] == b' ' && !in_string && brackets == 0);
        if boundary {
            if start < index {
                tokens.push(&arguments[start..index]);
            }
            index += 1;
            start = index;
            continue;
        }
        match bytes[index] {
            b'"' if !escaped => in_string = !in_string,
            b'\\' if in_string => escaped = !escaped,
            b'[' if !in_string => brackets += 1,
            b']' if !in_string && brackets > 0 => brackets -= 1,
            _ => escaped = false,
        }
        index += 1;
    }
    tokens
}

fn split_target(arguments: &str) -> Option<(&str, &str)> {
    let tokens = split_space_tokens(arguments);
    let target = *tokens.first()?;
    let rest = arguments.get(target.len()..)?.trim_start();
    (!rest.is_empty()).then_some((target, rest))
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
        Value::Number(number) => number.to_string(),
        Value::Boolean(boolean) => boolean.to_string(),
        Value::Date(date) => date.to_string(),
        Value::DateTime(datetime) => datetime
            .format(&time::format_description::well_known::Rfc3339)
            .expect("semantic datetimes are representable as RFC 3339"),
        Value::Formula(formula) => formula.to_string(),
        Value::Error(error) => error.to_string(),
    }
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
