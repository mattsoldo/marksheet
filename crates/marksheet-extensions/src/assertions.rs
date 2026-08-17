use std::cmp::Ordering;

use marksheet_calc::eval::CalcValue;
use marksheet_model::{
    ByteSpan, Coordinate, DiagnosticCode, DiagnosticContext, ExtensionId, SheetId, Value,
};

use crate::{
    DiagnosticEmission, ExtensionPlugin, ExtensionScopeRef, ExtensionWork, OpaqueExtensionInput,
    PluginContext, PluginDiagnostic, PluginDiagnosticSink, PluginResult,
};

/// An assertion comparison evaluated to false.
pub const ASSERTION_FAILED_DIAGNOSTIC: &str = "MS3201";
/// An assertion payload line is malformed or cannot be evaluated.
pub const ASSERTION_MALFORMED_DIAGNOSTIC: &str = "MS3202";
/// An assertions-specific resource bound stopped the instance.
pub const ASSERTION_LIMIT_DIAGNOSTIC: &str = "MS3203";

/// The trusted `assertions@1` validation extension.
#[derive(Clone, Copy, Debug, Default)]
pub struct AssertionsV1;

/// Statically linked `assertions@1` implementation.
pub static ASSERTIONS_V1: AssertionsV1 = AssertionsV1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operator {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Debug)]
struct Assertion {
    sheet: SheetId,
    coordinate: Coordinate,
    operator: Operator,
    expected: CalcValue,
    span: ByteSpan,
    line: u64,
}

#[derive(Clone, Copy, Debug)]
struct PhysicalLine<'a> {
    text: &'a str,
    span: ByteSpan,
    number: u64,
}

impl ExtensionPlugin for AssertionsV1 {
    fn id(&self) -> ExtensionId {
        ExtensionId::parse("assertions@1").expect("built-in extension identity is valid")
    }

    fn resource_limit_code(&self) -> DiagnosticCode {
        code(ASSERTION_LIMIT_DIAGNOSTIC)
    }

    fn validate(
        &self,
        input: OpaqueExtensionInput<'_>,
        context: PluginContext<'_>,
        diagnostics: &mut PluginDiagnosticSink,
    ) -> PluginResult {
        PluginResult {
            work: validate_assertions(input, context, diagnostics),
        }
    }
}

// The early-return sequence is intentionally kept together so reviewers can
// audit that every untrusted-input bound precedes parsing or calculation.
#[allow(clippy::too_many_lines)]
fn validate_assertions(
    input: OpaqueExtensionInput<'_>,
    context: PluginContext<'_>,
    diagnostics: &mut PluginDiagnosticSink,
) -> ExtensionWork {
    let payload_bytes = input.payload.len();
    let mut work = ExtensionWork {
        payload_bytes,
        ..ExtensionWork::default()
    };
    if payload_bytes > context.limits.max_payload_bytes {
        return limit_result(
            work,
            ByteSpan::try_new(0, u64::try_from(payload_bytes).unwrap_or(u64::MAX))
                .unwrap_or_default(),
            1,
            "assertions.payload_bytes",
            format!(
                "assertions payload has {payload_bytes} bytes; the configured limit is {}",
                context.limits.max_payload_bytes
            ),
            diagnostics,
        );
    }

    let payload = match std::str::from_utf8(input.payload) {
        Ok(payload) => payload,
        Err(error) => {
            let start = u64::try_from(error.valid_up_to()).unwrap_or(u64::MAX);
            let end = start
                .saturating_add(1)
                .min(u64::try_from(input.payload.len()).unwrap_or(u64::MAX));
            let _ = diagnostics.emit(malformed(
                ByteSpan::try_new(start, end).unwrap_or_default(),
                1,
                "assertions.invalid_utf8",
                "assertions payload is not valid UTF-8",
            ));
            return work;
        }
    };
    let lines = match physical_lines(payload, context.limits.max_lines) {
        Ok(lines) => lines,
        Err(offending) => {
            work.payload_lines = context.limits.max_lines.saturating_add(1);
            return limit_result(
                work,
                offending.span,
                offending.number,
                "assertions.lines",
                format!(
                    "assertions payload exceeds the configured {}-line limit",
                    context.limits.max_lines
                ),
                diagnostics,
            );
        }
    };
    work.payload_lines = lines.len();

    let candidate_lines: Vec<PhysicalLine<'_>> = lines
        .iter()
        .copied()
        .filter(|line| !line.text.is_empty() && !line.text.starts_with('#'))
        .collect();
    work.targets = candidate_lines.len();
    if candidate_lines.len() > context.limits.max_targets {
        let offending = candidate_lines[context.limits.max_targets];
        return limit_result(
            work,
            offending.span,
            offending.number,
            "assertions.targets",
            format!(
                "assertions payload exceeds the configured {}-target limit",
                context.limits.max_targets
            ),
            diagnostics,
        );
    }
    if !candidate_lines.is_empty() && context.limits.max_target_area < 1 {
        let offending = candidate_lines[0];
        return limit_result(
            work,
            offending.span,
            offending.number,
            "assertions.target_area",
            "a one-cell assertion target exceeds the configured target-area limit",
            diagnostics,
        );
    }
    if work.total_units() > context.limits.max_work_units {
        let offending = candidate_lines.first().copied().unwrap_or(PhysicalLine {
            text: "",
            span: ByteSpan::default(),
            number: 1,
        });
        return limit_result(
            work,
            offending.span,
            offending.number,
            "assertions.work",
            "assertions payload exceeds the configured aggregate work limit",
            diagnostics,
        );
    }

    let mut assertions = Vec::with_capacity(candidate_lines.len());
    for line in candidate_lines {
        match parse_assertion(line, input.scope) {
            Ok(assertion) => assertions.push(assertion),
            Err(diagnostic) => {
                if diagnostics.emit(*diagnostic) == DiagnosticEmission::Stop {
                    return work;
                }
            }
        }
    }
    if assertions.is_empty() {
        return work;
    }

    for assertion in &assertions {
        let lookup = context.calculated_cell(&assertion.sheet, assertion.coordinate);
        let Some(actual) = lookup.value.as_ref() else {
            if lookup.resource_limited {
                return limit_result_with_context(
                    work,
                    assertion,
                    "assertions.calculation_limit",
                    "core calculation exceeded a configured limit for assertion target",
                    diagnostics,
                );
            }
            let diagnostic = malformed(
                assertion.span,
                assertion.line,
                "assertions.unresolved_target",
                "assertion target does not resolve to a calculable workbook cell",
            );
            if diagnostics.emit(with_target_context(diagnostic, assertion))
                == DiagnosticEmission::Stop
            {
                return work;
            }
            continue;
        };

        if !comparison_holds(actual, assertion.operator, &assertion.expected) {
            let diagnostic = failed(
                assertion.span,
                assertion.line,
                format!(
                    "assertion failed: calculated {} does not satisfy the typed comparison with {}",
                    describe(actual),
                    describe(&assertion.expected)
                ),
            );
            if diagnostics.emit(with_target_context(diagnostic, assertion))
                == DiagnosticEmission::Stop
            {
                return work;
            }
        }
    }

    work
}

fn physical_lines(
    payload: &str,
    max_lines: usize,
) -> Result<Vec<PhysicalLine<'_>>, PhysicalLine<'_>> {
    let bytes = payload.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0_usize;
    let mut number = 1_u64;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let content_end = if index > start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        let line = PhysicalLine {
            text: &payload[start..content_end],
            span: byte_span(start, content_end),
            number,
        };
        if lines.len() == max_lines {
            return Err(line);
        }
        lines.push(line);
        start = index + 1;
        number = number.saturating_add(1);
    }
    if start < bytes.len() {
        let content_end = if bytes.last() == Some(&b'\r') {
            bytes.len() - 1
        } else {
            bytes.len()
        };
        let line = PhysicalLine {
            text: &payload[start..content_end],
            span: byte_span(start, content_end),
            number,
        };
        if lines.len() == max_lines {
            return Err(line);
        }
        lines.push(line);
    }
    Ok(lines)
}

fn parse_assertion(
    line: PhysicalLine<'_>,
    scope: ExtensionScopeRef<'_>,
) -> Result<Assertion, Box<PluginDiagnostic>> {
    let Some(rest) = line.text.strip_prefix("assert ") else {
        return Err(Box::new(malformed_line(
            line,
            "line must start with exact `assert `",
        )));
    };
    let Some((target, rest)) = rest.split_once(' ') else {
        return Err(Box::new(malformed_line(
            line,
            "assertion is missing an operator and literal",
        )));
    };
    let Some((operator, literal)) = rest.split_once(' ') else {
        return Err(Box::new(malformed_line(
            line,
            "assertion is missing a literal",
        )));
    };
    if target.is_empty() || operator.is_empty() || literal.is_empty() || literal.starts_with(' ') {
        return Err(Box::new(malformed_line(
            line,
            "assertion tokens require exactly one ASCII separator space",
        )));
    }
    let operator = parse_operator(operator)
        .ok_or_else(|| Box::new(malformed_line(line, "assertion operator is not supported")))?;
    let (sheet, coordinate) =
        parse_target(target, scope).map_err(|message| Box::new(malformed_line(line, message)))?;
    let expected =
        parse_literal(literal).map_err(|message| Box::new(malformed_line(line, message)))?;
    Ok(Assertion {
        sheet,
        coordinate,
        operator,
        expected,
        span: line.span,
        line: line.number,
    })
}

fn parse_operator(operator: &str) -> Option<Operator> {
    Some(match operator {
        "=" => Operator::Equal,
        "!=" => Operator::NotEqual,
        "<" => Operator::Less,
        "<=" => Operator::LessEqual,
        ">" => Operator::Greater,
        ">=" => Operator::GreaterEqual,
        _ => return None,
    })
}

fn parse_target(
    target: &str,
    scope: ExtensionScopeRef<'_>,
) -> Result<(SheetId, Coordinate), &'static str> {
    match scope {
        ExtensionScopeRef::Workbook => {
            let (sheet, coordinate) = target
                .split_once('!')
                .filter(|(_, coordinate)| !coordinate.contains('!'))
                .ok_or("workbook-scoped assertion target must be sheet-qualified")?;
            let sheet =
                SheetId::parse(sheet).map_err(|_| "assertion target has invalid sheet ID")?;
            let coordinate = parse_canonical_coordinate(coordinate)?;
            Ok((sheet, coordinate))
        }
        ExtensionScopeRef::Sheet(sheet) => {
            if target.contains('!') {
                return Err("sheet-scoped assertion target must be unqualified");
            }
            Ok((sheet.clone(), parse_canonical_coordinate(target)?))
        }
    }
}

fn parse_canonical_coordinate(value: &str) -> Result<Coordinate, &'static str> {
    let coordinate = Coordinate::parse(value).map_err(|_| "assertion target is not one A1 cell")?;
    if coordinate.to_string() != value {
        return Err("assertion target must use canonical uppercase A1 spelling");
    }
    Ok(coordinate)
}

fn parse_literal(literal: &str) -> Result<CalcValue, &'static str> {
    let value = if literal.starts_with('"') {
        if literal.trim_matches(|character: char| character.is_ascii_whitespace()) != literal {
            return Err("assertion text literal cannot have surrounding whitespace");
        }
        let text = serde_json::from_str::<String>(literal)
            .map_err(|_| "assertion text literal is not one valid JSON string")?;
        Value::Text(text)
    } else if literal == "blank" {
        Value::Blank
    } else {
        let parsed = Value::parse_strict(literal)
            .map_err(|_| "assertion literal is not a valid core scalar")?;
        match parsed {
            Value::Blank | Value::Text(_) | Value::Formula(_) => {
                return Err("assertion literal must be typed, `blank`, or a JSON string");
            }
            scalar => scalar,
        }
    };
    CalcValue::try_from(value).map_err(|_| "formula literals are not allowed in assertions")
}

fn comparison_holds(actual: &CalcValue, operator: Operator, expected: &CalcValue) -> bool {
    match operator {
        Operator::Equal => exact_equal(actual, expected),
        Operator::NotEqual => !exact_equal(actual, expected),
        Operator::Less => ordered(actual, expected) == Some(Ordering::Less),
        Operator::LessEqual => matches!(
            ordered(actual, expected),
            Some(Ordering::Less | Ordering::Equal)
        ),
        Operator::Greater => ordered(actual, expected) == Some(Ordering::Greater),
        Operator::GreaterEqual => matches!(
            ordered(actual, expected),
            Some(Ordering::Greater | Ordering::Equal)
        ),
    }
}

fn exact_equal(actual: &CalcValue, expected: &CalcValue) -> bool {
    match (actual, expected) {
        (CalcValue::DateTime(left), CalcValue::DateTime(right)) => {
            left == right && left.offset() == right.offset()
        }
        _ => actual == expected,
    }
}

fn ordered(actual: &CalcValue, expected: &CalcValue) -> Option<Ordering> {
    match (actual, expected) {
        (CalcValue::Number(left), CalcValue::Number(right)) => left.partial_cmp(right),
        (CalcValue::Date(left), CalcValue::Date(right)) => Some(left.cmp(right)),
        (CalcValue::DateTime(left), CalcValue::DateTime(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn describe(value: &CalcValue) -> String {
    match value {
        CalcValue::Blank => "blank".to_owned(),
        CalcValue::Text(text) => format!("text ({} UTF-8 bytes)", text.len()),
        CalcValue::Number(number) => format!("number {number}"),
        CalcValue::Boolean(boolean) => format!("Boolean {boolean}"),
        CalcValue::Date(date) => format!("date {date}"),
        CalcValue::DateTime(datetime) => format!("datetime {datetime}"),
        CalcValue::Error(error) => format!("error {error}"),
    }
}

fn malformed_line(line: PhysicalLine<'_>, message: impl Into<String>) -> PluginDiagnostic {
    malformed(line.span, line.number, "assertions.malformed", message)
}

fn failed(span: ByteSpan, line: u64, message: impl Into<String>) -> PluginDiagnostic {
    PluginDiagnostic::validation_failure("assertions.failed", message, span)
        .with_code(code(ASSERTION_FAILED_DIAGNOSTIC))
        .with_payload_line(line)
}

fn malformed(
    span: ByteSpan,
    line: u64,
    subcode: &str,
    message: impl Into<String>,
) -> PluginDiagnostic {
    PluginDiagnostic::rejected(subcode, message, span)
        .with_code(code(ASSERTION_MALFORMED_DIAGNOSTIC))
        .with_payload_line(line)
}

fn assertion_limit(
    span: ByteSpan,
    line: u64,
    subcode: &str,
    message: impl Into<String>,
) -> PluginDiagnostic {
    PluginDiagnostic::limit(subcode, message, span)
        .with_code(code(ASSERTION_LIMIT_DIAGNOSTIC))
        .with_payload_line(line)
}

fn limit_result(
    work: ExtensionWork,
    span: ByteSpan,
    line: u64,
    subcode: &str,
    message: impl Into<String>,
    diagnostics: &mut PluginDiagnosticSink,
) -> ExtensionWork {
    let _ = diagnostics.emit(assertion_limit(span, line, subcode, message));
    work
}

fn limit_result_with_context(
    work: ExtensionWork,
    assertion: &Assertion,
    subcode: &str,
    message: impl Into<String>,
    diagnostics: &mut PluginDiagnosticSink,
) -> ExtensionWork {
    let _ = diagnostics.emit(with_target_context(
        assertion_limit(assertion.span, assertion.line, subcode, message),
        assertion,
    ));
    work
}

fn with_target_context(diagnostic: PluginDiagnostic, assertion: &Assertion) -> PluginDiagnostic {
    diagnostic.with_context(DiagnosticContext {
        sheet: Some(assertion.sheet.clone()),
        cell: Some(assertion.coordinate),
    })
}

fn code(value: &str) -> DiagnosticCode {
    DiagnosticCode::new(value).expect("assertion diagnostic constants are valid")
}

fn byte_span(start: usize, end: usize) -> ByteSpan {
    ByteSpan::try_new(
        u64::try_from(start).unwrap_or(u64::MAX),
        u64::try_from(end).unwrap_or(u64::MAX),
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_lines_preserve_local_byte_offsets_across_crlf() {
        let lines = physical_lines("assert A1 = 1\r\n# comment\n\n", usize::MAX).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "assert A1 = 1");
        assert_eq!(lines[0].span, ByteSpan::try_new(0, 13).unwrap());
        assert_eq!(lines[1].text, "# comment");
        assert_eq!(lines[2].text, "");
    }

    #[test]
    fn comparison_is_typed_and_never_coerces() {
        assert!(comparison_holds(
            &CalcValue::Number(1.0),
            Operator::Equal,
            &CalcValue::Number(1.0)
        ));
        assert!(!comparison_holds(
            &CalcValue::Number(1.0),
            Operator::Equal,
            &CalcValue::Text("1".to_owned())
        ));
        assert!(!comparison_holds(
            &CalcValue::Text("a".to_owned()),
            Operator::Less,
            &CalcValue::Text("b".to_owned())
        ));
    }
}
