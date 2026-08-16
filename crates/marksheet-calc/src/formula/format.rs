use std::fmt;

use marksheet_model::canonical_number;

use super::ast::{
    A1Reference, BinaryOperator, Expr, ExprKind, Formula, Literal, Reference, StructuredReference,
    TableRegion,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormulaFormatError {
    NonFiniteNumber,
    NegativeNumberLiteral,
}

impl fmt::Display for FormulaFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteNumber => formatter.write_str("formula number must be finite"),
            Self::NegativeNumberLiteral => {
                formatter.write_str("a negative formula number must use a unary operator")
            }
        }
    }
}

impl std::error::Error for FormulaFormatError {}

/// Emits the canonical complete formula spelling.
///
/// # Errors
///
/// Returns [`FormulaFormatError`] if a manually constructed AST contains a
/// non-finite or signed number literal. Parsed ASTs always satisfy this rule.
pub fn format_formula(formula: &Formula) -> Result<String, FormulaFormatError> {
    let mut output = String::from("=");
    render(&formula.expression, None, &mut output)?;
    Ok(output)
}

/// Emits a canonical expression without the leading formula marker.
///
/// # Errors
///
/// Returns [`FormulaFormatError`] under the same conditions as
/// [`format_formula`].
pub fn format_expression(expression: &Expr) -> Result<String, FormulaFormatError> {
    let mut output = String::new();
    render(expression, None, &mut output)?;
    Ok(output)
}

#[derive(Clone, Copy)]
enum Parent {
    Unary,
    Binary {
        operator: BinaryOperator,
        side: Side,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Side {
    Left,
    Right,
}

fn render(
    expression: &Expr,
    parent: Option<Parent>,
    output: &mut String,
) -> Result<(), FormulaFormatError> {
    let parentheses = parent.is_some_and(|context| needs_parentheses(expression, context));
    if parentheses {
        output.push('(');
    }
    match &expression.kind {
        ExprKind::Literal { value } => render_literal(value, output)?,
        ExprKind::Reference { reference } => render_reference(reference, output),
        ExprKind::Unary { operator, operand } => {
            output.push(operator.symbol());
            render(operand, Some(Parent::Unary), output)?;
        }
        ExprKind::Binary {
            operator,
            left,
            right,
        } => {
            render(
                left,
                Some(Parent::Binary {
                    operator: *operator,
                    side: Side::Left,
                }),
                output,
            )?;
            output.push_str(operator.symbol());
            render(
                right,
                Some(Parent::Binary {
                    operator: *operator,
                    side: Side::Right,
                }),
                output,
            )?;
        }
        ExprKind::Call { call } => {
            output.push_str(&call.name.to_ascii_uppercase());
            output.push('(');
            for (index, argument) in call.arguments.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                render(argument, None, output)?;
            }
            output.push(')');
        }
    }
    if parentheses {
        output.push(')');
    }
    Ok(())
}

fn render_literal(literal: &Literal, output: &mut String) -> Result<(), FormulaFormatError> {
    match literal {
        Literal::Number(number) => {
            let spelling =
                canonical_number(*number).map_err(|_| FormulaFormatError::NonFiniteNumber)?;
            if number.is_sign_negative() {
                return Err(FormulaFormatError::NegativeNumberLiteral);
            }
            output.push_str(&spelling);
        }
        Literal::Text(text) => {
            output.push('"');
            output.push_str(&text.replace('"', "\"\""));
            output.push('"');
        }
        Literal::Boolean(value) => output.push_str(if *value { "TRUE" } else { "FALSE" }),
        Literal::Error(error) => output.push_str(error.token()),
    }
    Ok(())
}

fn render_reference(reference: &Reference, output: &mut String) {
    match reference {
        Reference::Cell { sheet, address } => {
            if let Some(sheet) = sheet {
                output.push_str(sheet.as_str());
                output.push('!');
            }
            render_a1(address, output);
        }
        Reference::Range(range) => {
            if let Some(sheet) = &range.sheet {
                output.push_str(sheet.as_str());
                output.push('!');
            }
            render_a1(&range.start, output);
            output.push(':');
            render_a1(&range.end, output);
        }
        Reference::Name { name } => output.push_str(name.as_str()),
        Reference::Structured(structured) => render_structured(structured, output),
    }
}

fn render_a1(reference: &A1Reference, output: &mut String) {
    if reference.column_absolute {
        output.push('$');
    }
    output.push_str(&reference.coordinate.column_name());
    if reference.row_absolute {
        output.push('$');
    }
    output.push_str(&reference.coordinate.row.to_string());
}

fn render_structured(reference: &StructuredReference, output: &mut String) {
    let (table, selector): (Option<&str>, String) = match reference {
        StructuredReference::Column { table, header } => {
            (Some(table.as_str()), escape_header(header))
        }
        StructuredReference::Region { table, region } => (
            Some(table.as_str()),
            match region {
                TableRegion::Headers => "#Headers".to_owned(),
                TableRegion::Data => "#Data".to_owned(),
            },
        ),
        StructuredReference::CurrentRow { table, header } => (
            table.as_ref().map(marksheet_model::TableId::as_str),
            format!("@{}", escape_header(header)),
        ),
    };
    if let Some(table) = table {
        output.push_str(table);
    }
    output.push('[');
    output.push_str(&selector);
    output.push(']');
}

fn escape_header(header: &str) -> String {
    header.replace(']', "]]")
}

fn needs_parentheses(expression: &Expr, parent: Parent) -> bool {
    let child_precedence = precedence(expression);
    match parent {
        Parent::Unary => child_precedence < UNARY_PRECEDENCE,
        Parent::Binary { operator, side } => {
            let parent_precedence = binary_precedence(operator);
            if operator == BinaryOperator::Power
                && side == Side::Right
                && matches!(expression.kind, ExprKind::Unary { .. })
            {
                return false;
            }
            if child_precedence != parent_precedence {
                return child_precedence < parent_precedence;
            }
            if parent_precedence == COMPARISON_PRECEDENCE {
                // Comparisons cannot chain in the grammar, so either child
                // comparison must retain explicit grouping.
                return true;
            }
            if operator == BinaryOperator::Power {
                side == Side::Left
            } else {
                side == Side::Right
            }
        }
    }
}

const COMPARISON_PRECEDENCE: u8 = 1;
const CONCATENATION_PRECEDENCE: u8 = 2;
const ADDITIVE_PRECEDENCE: u8 = 3;
const MULTIPLICATIVE_PRECEDENCE: u8 = 4;
const UNARY_PRECEDENCE: u8 = 5;
const POWER_PRECEDENCE: u8 = 6;
const PRIMARY_PRECEDENCE: u8 = 7;

fn precedence(expression: &Expr) -> u8 {
    match &expression.kind {
        ExprKind::Literal { .. } | ExprKind::Reference { .. } | ExprKind::Call { .. } => {
            PRIMARY_PRECEDENCE
        }
        ExprKind::Unary { .. } => UNARY_PRECEDENCE,
        ExprKind::Binary { operator, .. } => binary_precedence(*operator),
    }
}

const fn binary_precedence(operator: BinaryOperator) -> u8 {
    match operator {
        BinaryOperator::Power => POWER_PRECEDENCE,
        BinaryOperator::Multiply | BinaryOperator::Divide => MULTIPLICATIVE_PRECEDENCE,
        BinaryOperator::Add | BinaryOperator::Subtract => ADDITIVE_PRECEDENCE,
        BinaryOperator::Concatenate => CONCATENATION_PRECEDENCE,
        BinaryOperator::Equal
        | BinaryOperator::NotEqual
        | BinaryOperator::Less
        | BinaryOperator::LessEqual
        | BinaryOperator::Greater
        | BinaryOperator::GreaterEqual => COMPARISON_PRECEDENCE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::{ParseLimits, parse};

    fn canonical(source: &str) -> String {
        format_formula(&parse(source, &ParseLimits::default()).expect("valid formula"))
            .expect("formattable formula")
    }

    #[test]
    fn canonicalizes_names_numbers_and_spacing() {
        assert_eq!(canonical("= sum ( 1.0, a1 ) "), "=SUM(1,A1)");
        assert_eq!(canonical("=100000000000000000000"), "=1e20");
        assert_eq!(canonical("=0.0000001"), "=1e-7");
        assert_eq!(canonical("=-0.0"), "=-0");
    }

    #[test]
    fn rejects_signed_number_literals_in_manually_constructed_asts() {
        let mut formula = parse("=0", &ParseLimits::default()).expect("valid formula");
        set_number_literal(&mut formula, -0.0);
        assert_eq!(
            format_formula(&formula),
            Err(FormulaFormatError::NegativeNumberLiteral)
        );

        set_number_literal(&mut formula, f64::INFINITY);
        assert_eq!(
            format_formula(&formula),
            Err(FormulaFormatError::NonFiniteNumber)
        );
    }

    fn set_number_literal(formula: &mut Formula, value: f64) {
        let ExprKind::Literal {
            value: Literal::Number(number),
        } = &mut formula.expression.kind
        else {
            panic!("expected number literal");
        };
        *number = value;
    }

    #[test]
    fn preserves_operator_meaning_with_minimal_parentheses() {
        assert_eq!(canonical("=(-2)^2"), "=(-2)^2");
        assert_eq!(canonical("=2^-2"), "=2^-2");
        assert_eq!(canonical("=1-(2-3)"), "=1-(2-3)");
        assert_eq!(canonical("=(1-2)-3"), "=1-2-3");
        assert_eq!(canonical("=(1<2)<3"), "=(1<2)<3");
    }

    #[test]
    fn escapes_text_and_structured_headers() {
        assert_eq!(
            canonical("=\"a\"\"b\"&fees[Fee]]]"),
            "=\"a\"\"b\"&fees[Fee]]]"
        );
        assert_eq!(canonical("=costs[Unit Cost]"), "=costs[Unit Cost]");
        assert_eq!(canonical("=[@Unit Cost]"), "=[@Unit Cost]");
    }
}
