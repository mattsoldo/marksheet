//! Capability checks for the `portable-a1@1` evaluator profile.
//!
//! The formula parser intentionally accepts unknown function names and any
//! argument count. Conversion needs a stricter boundary: a formula is only
//! preserved when the portable evaluator can execute both its function names
//! and their arities. Keeping that policy here prevents CSV and OOXML paths
//! from drifting apart.

use std::fmt;

use marksheet_calc::formula::{Expr, ExprKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FormulaProfileError {
    UnsupportedFunction {
        name: String,
    },
    InvalidArity {
        name: String,
        actual: usize,
        expected: &'static str,
    },
}

impl FormulaProfileError {
    pub(crate) const fn is_invalid_arity(&self) -> bool {
        matches!(self, Self::InvalidArity { .. })
    }
}

impl fmt::Display for FormulaProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFunction { name } => write!(
                formatter,
                "function {name} is outside the portable-a1@1 evaluation profile"
            ),
            Self::InvalidArity {
                name,
                actual,
                expected,
            } => write!(
                formatter,
                "function {name} has {actual} arguments; portable-a1@1 requires {expected}"
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum Arity {
    Exact(usize),
    OneOf(&'static [usize]),
    AtLeast(usize),
}

impl Arity {
    fn accepts(self, actual: usize) -> bool {
        match self {
            Self::Exact(expected) => actual == expected,
            Self::OneOf(expected) => expected.contains(&actual),
            Self::AtLeast(minimum) => actual >= minimum,
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Exact(1) => "exactly 1 argument",
            Self::Exact(2) => "exactly 2 arguments",
            Self::Exact(3) => "exactly 3 arguments",
            Self::Exact(_) => "the evaluator-defined exact argument count",
            Self::OneOf(&[1, 2]) => "1 or 2 arguments",
            Self::OneOf(&[2, 3]) => "2 or 3 arguments",
            Self::OneOf(_) => "an evaluator-defined argument count",
            Self::AtLeast(1) => "at least 1 argument",
            Self::AtLeast(_) => "the evaluator-defined minimum argument count",
        }
    }
}

/// Validates every call in an expression against the portable evaluator.
pub(crate) fn validate_formula_expression(expression: &Expr) -> Result<(), FormulaProfileError> {
    match &expression.kind {
        ExprKind::Literal { .. } | ExprKind::Reference { .. } => Ok(()),
        ExprKind::Unary { operand, .. } => validate_formula_expression(operand),
        ExprKind::Binary { left, right, .. } => {
            validate_formula_expression(left)?;
            validate_formula_expression(right)
        }
        ExprKind::Call { call } => {
            let arity = function_arity(&call.name).ok_or_else(|| {
                FormulaProfileError::UnsupportedFunction {
                    name: call.name.clone(),
                }
            })?;
            if !arity.accepts(call.arguments.len()) {
                return Err(FormulaProfileError::InvalidArity {
                    name: call.name.clone(),
                    actual: call.arguments.len(),
                    expected: arity.description(),
                });
            }
            for argument in &call.arguments {
                validate_formula_expression(argument)?;
            }
            Ok(())
        }
    }
}

fn function_arity(name: &str) -> Option<Arity> {
    Some(match name {
        "SUM" | "AVERAGE" | "MIN" | "MAX" | "COUNT" | "COUNTA" | "AND" | "OR" | "CONCAT" => {
            Arity::AtLeast(1)
        }
        "IF" | "MID" | "DATE" => Arity::Exact(3),
        "IFERROR" | "MOD" | "ROUND" | "ROUNDUP" | "ROUNDDOWN" => Arity::Exact(2),
        "NOT" | "ABS" | "INT" | "LEN" | "LOWER" | "UPPER" | "TRIM" | "YEAR" | "MONTH" | "DAY"
        | "ISBLANK" | "ISNUMBER" | "ISTEXT" | "ISERROR" => Arity::Exact(1),
        "LEFT" | "RIGHT" => Arity::OneOf(&[1, 2]),
        "INDEX" | "MATCH" => Arity::OneOf(&[2, 3]),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use marksheet_calc::formula::{ParseLimits, parse};

    use super::{FormulaProfileError, validate_formula_expression};

    fn validate(source: &str) -> Result<(), FormulaProfileError> {
        let formula = parse(source, &ParseLimits::default()).expect("test formula parses");
        validate_formula_expression(&formula.expression)
    }

    #[test]
    fn rejects_known_function_with_wrong_arity() {
        let error = validate("=IF(TRUE,1)").unwrap_err();
        assert!(matches!(
            error,
            FormulaProfileError::InvalidArity {
                ref name,
                actual: 2,
                ..
            } if name == "IF"
        ));
    }

    #[test]
    fn accepts_optional_and_variable_arities() {
        for source in [
            "=LEFT(\"abc\")",
            "=LEFT(\"abc\",2)",
            "=INDEX(A1:A2,1)",
            "=INDEX(A1:B2,1,2)",
            "=SUM(1)",
            "=SUM(1,2,3)",
            "=CONCAT(\"a\")",
            "=CONCAT(\"a\",\"b\")",
        ] {
            validate(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        }
    }

    #[test]
    fn rejects_empty_variable_arity_and_unknown_calls_recursively() {
        assert!(matches!(
            validate("=SUM()"),
            Err(FormulaProfileError::InvalidArity { .. })
        ));
        assert!(matches!(
            validate("=IF(TRUE,XLOOKUP(A1,A1:A2,B1:B2),0)"),
            Err(FormulaProfileError::UnsupportedFunction { .. })
        ));
    }
}
