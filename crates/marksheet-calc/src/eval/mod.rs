//! Deterministic evaluation for the `portable-a1@1` formula profile.
//!
//! Reference resolution is supplied by the calculation engine through
//! [`EvaluationContext`]. This keeps formula semantics independent of workbook
//! storage while retaining the scalar/range distinction needed by functions.

mod value;

use std::cmp::Ordering;
use std::fmt;

use marksheet_model::{ByteSpan, CellError, canonical_number};
use time::{Date, Month, format_description::well_known::Rfc3339};

use crate::formula::{
    BinaryOperator, Expr, ExprKind, Formula, FunctionCall, Literal, Reference, UnaryOperator,
};

pub use value::{CalcValue, FormulaValueError, RangeShapeError, RectangularRange, ResolvedValue};

/// Resolves authored formula references in the caller's workbook context.
///
/// Unknown bare names should return [`CellError::Name`]; all other unresolved
/// or out-of-bounds references should return [`CellError::Reference`].
pub trait EvaluationContext {
    /// Resolves a reference to its scalar or rectangular calculated value.
    ///
    /// # Errors
    ///
    /// Returns the spreadsheet error appropriate for an unresolved reference.
    fn resolve(&self, reference: &Reference, span: ByteSpan) -> Result<ResolvedValue, CellError>;
}

/// Resource bounds for one formula evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationLimits {
    pub max_steps: usize,
    pub max_range_cells: usize,
    pub max_text_bytes: usize,
}

impl Default for EvaluationLimits {
    fn default() -> Self {
        Self {
            max_steps: 1_000_000,
            max_range_cells: 1_000_000,
            max_text_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Work performed or attempted by an evaluation.
///
/// On a limit failure, the rejected operation is included in the relevant
/// counter. This makes unsuccessful work additive with successful outcomes
/// when a workbook engine reports aggregate calculation telemetry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EvaluationStats {
    pub steps: usize,
    pub range_cells: usize,
    pub text_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvaluationOutcome {
    pub value: CalcValue,
    pub stats: EvaluationStats,
}

/// Operational failure distinct from a spreadsheet error value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvaluationError {
    StepLimitExceeded {
        limit: usize,
        stats: EvaluationStats,
    },
    RangeCellLimitExceeded {
        limit: usize,
        stats: EvaluationStats,
    },
    TextByteLimitExceeded {
        limit: usize,
        stats: EvaluationStats,
    },
}

impl EvaluationError {
    /// Returns the exact work counters at the point evaluation was rejected.
    #[must_use]
    pub const fn stats(&self) -> EvaluationStats {
        match self {
            Self::StepLimitExceeded { stats, .. }
            | Self::RangeCellLimitExceeded { stats, .. }
            | Self::TextByteLimitExceeded { stats, .. } => *stats,
        }
    }
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StepLimitExceeded { limit, .. } => {
                write!(
                    formatter,
                    "formula evaluation exceeded its {limit}-step limit"
                )
            }
            Self::RangeCellLimitExceeded { limit, .. } => write!(
                formatter,
                "formula evaluation traversed more than {limit} range cells"
            ),
            Self::TextByteLimitExceeded { limit, .. } => write!(
                formatter,
                "formula evaluation produced more than {limit} text bytes"
            ),
        }
    }
}

impl std::error::Error for EvaluationError {}

/// Evaluates one parsed formula with explicit resource bounds.
///
/// # Errors
///
/// Returns [`EvaluationError`] if evaluation exceeds an operational resource
/// limit. Spreadsheet failures remain successful [`CalcValue::Error`] values.
pub fn evaluate<C: EvaluationContext + ?Sized>(
    formula: &Formula,
    context: &C,
    limits: &EvaluationLimits,
) -> Result<EvaluationOutcome, EvaluationError> {
    let mut evaluator = Evaluator {
        context,
        limits,
        stats: EvaluationStats::default(),
    };
    let value = match evaluator.expression(&formula.expression)? {
        RuntimeValue::Scalar(value) => value,
        RuntimeValue::Range(_) => CalcValue::Error(CellError::Value),
    };
    Ok(EvaluationOutcome {
        value: finite_or_error(value),
        stats: evaluator.stats,
    })
}

/// Convenience entry point using [`EvaluationLimits::default`].
///
/// # Errors
///
/// Returns [`EvaluationError`] if evaluation exceeds a default resource limit.
pub fn evaluate_with_defaults<C: EvaluationContext + ?Sized>(
    formula: &Formula,
    context: &C,
) -> Result<EvaluationOutcome, EvaluationError> {
    evaluate(formula, context, &EvaluationLimits::default())
}

#[derive(Clone, Debug, PartialEq)]
enum RuntimeValue {
    Scalar(CalcValue),
    Range(RectangularRange),
}

struct Evaluator<'a, C: ?Sized> {
    context: &'a C,
    limits: &'a EvaluationLimits,
    stats: EvaluationStats,
}

impl<C: EvaluationContext + ?Sized> Evaluator<'_, C> {
    fn expression(&mut self, expression: &Expr) -> Result<RuntimeValue, EvaluationError> {
        self.step()?;
        match &expression.kind {
            ExprKind::Literal { value } => self.literal(value),
            ExprKind::Reference { reference } => {
                let resolved = self.context.resolve(reference, expression.span);
                Ok(match resolved {
                    Ok(ResolvedValue::Scalar(value)) => {
                        let value = finite_or_error(value);
                        if let CalcValue::Text(text) = &value {
                            self.text(text.len())?;
                        }
                        RuntimeValue::Scalar(value)
                    }
                    Ok(ResolvedValue::Range(range)) => RuntimeValue::Range(range),
                    Err(error) => RuntimeValue::Scalar(CalcValue::Error(error)),
                })
            }
            ExprKind::Unary { operator, operand } => self.unary(*operator, operand),
            ExprKind::Binary {
                operator,
                left,
                right,
            } => self.binary(*operator, left, right),
            ExprKind::Call { call } => self.call(call),
        }
    }

    fn literal(&mut self, literal: &Literal) -> Result<RuntimeValue, EvaluationError> {
        let value = match literal {
            Literal::Number(value) => CalcValue::Number(*value),
            Literal::Text(value) => {
                self.text(value.len())?;
                CalcValue::Text(value.clone())
            }
            Literal::Boolean(value) => CalcValue::Boolean(*value),
            Literal::Error(value) => CalcValue::Error(*value),
        };
        Ok(RuntimeValue::Scalar(finite_or_error(value)))
    }

    fn unary(
        &mut self,
        operator: UnaryOperator,
        operand: &Expr,
    ) -> Result<RuntimeValue, EvaluationError> {
        let operand = self.scalar(operand)?;
        if operand.as_error().is_some() {
            return Ok(RuntimeValue::Scalar(operand));
        }
        let number = match numeric_coercion(&operand) {
            Ok(value) => value,
            Err(error) => return Ok(RuntimeValue::Scalar(CalcValue::Error(error))),
        };
        let result = match operator {
            UnaryOperator::Positive => number,
            UnaryOperator::Negative => -number,
        };
        Ok(RuntimeValue::Scalar(number_result(result)))
    }

    fn binary(
        &mut self,
        operator: BinaryOperator,
        left: &Expr,
        right: &Expr,
    ) -> Result<RuntimeValue, EvaluationError> {
        let left = self.scalar(left)?;
        if left.as_error().is_some() {
            return Ok(RuntimeValue::Scalar(left));
        }
        let right = self.scalar(right)?;
        if right.as_error().is_some() {
            return Ok(RuntimeValue::Scalar(right));
        }

        let result = match operator {
            BinaryOperator::Concatenate => self.concatenate(&left, &right)?,
            BinaryOperator::Equal => CalcValue::Boolean(equal_values(&left, &right)),
            BinaryOperator::NotEqual => CalcValue::Boolean(!equal_values(&left, &right)),
            BinaryOperator::Less
            | BinaryOperator::LessEqual
            | BinaryOperator::Greater
            | BinaryOperator::GreaterEqual => compare_values(operator, &left, &right),
            _ => arithmetic(operator, &left, &right),
        };
        Ok(RuntimeValue::Scalar(result))
    }

    fn call(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        match call.name.as_str() {
            "SUM" | "AVERAGE" | "MIN" | "MAX" | "COUNT" | "COUNTA" => self.aggregate(call),
            "IF" => self.function_if(call),
            "IFERROR" => self.function_iferror(call),
            "AND" => self.function_and_or(call, false),
            "OR" => self.function_and_or(call, true),
            "NOT" => self.function_not(call),
            "ABS" | "INT" | "MOD" | "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                self.numeric_function(call)
            }
            "CONCAT" | "LEFT" | "RIGHT" | "MID" | "LEN" | "LOWER" | "UPPER" | "TRIM" => {
                self.text_function(call)
            }
            "INDEX" | "MATCH" => self.lookup_function(call),
            "DATE" | "YEAR" | "MONTH" | "DAY" => self.date_function(call),
            "ISBLANK" | "ISNUMBER" | "ISTEXT" | "ISERROR" => self.inspection_function(call),
            _ => Ok(RuntimeValue::Scalar(CalcValue::Error(CellError::Name))),
        }
    }

    fn scalar(&mut self, expression: &Expr) -> Result<CalcValue, EvaluationError> {
        Ok(match self.expression(expression)? {
            RuntimeValue::Scalar(value) => finite_or_error(value),
            RuntimeValue::Range(_) => CalcValue::Error(CellError::Value),
        })
    }

    fn evaluated_argument(&mut self, expression: &Expr) -> Result<RuntimeValue, EvaluationError> {
        self.expression(expression)
    }

    fn step(&mut self) -> Result<(), EvaluationError> {
        self.stats.steps = self.stats.steps.saturating_add(1);
        if self.stats.steps > self.limits.max_steps {
            return Err(EvaluationError::StepLimitExceeded {
                limit: self.limits.max_steps,
                stats: self.stats,
            });
        }
        Ok(())
    }

    fn range_cell(&mut self) -> Result<(), EvaluationError> {
        self.stats.range_cells = self.stats.range_cells.saturating_add(1);
        if self.stats.range_cells > self.limits.max_range_cells {
            return Err(EvaluationError::RangeCellLimitExceeded {
                limit: self.limits.max_range_cells,
                stats: self.stats,
            });
        }
        Ok(())
    }

    fn text(&mut self, bytes: usize) -> Result<(), EvaluationError> {
        self.stats.text_bytes = self.stats.text_bytes.saturating_add(bytes);
        if self.stats.text_bytes > self.limits.max_text_bytes {
            return Err(EvaluationError::TextByteLimitExceeded {
                limit: self.limits.max_text_bytes,
                stats: self.stats,
            });
        }
        Ok(())
    }

    fn concatenate(
        &mut self,
        left: &CalcValue,
        right: &CalcValue,
    ) -> Result<CalcValue, EvaluationError> {
        let left = match text_coercion(left) {
            Ok(value) => value,
            Err(error) => return Ok(CalcValue::Error(error)),
        };
        let right = match text_coercion(right) {
            Ok(value) => value,
            Err(error) => return Ok(CalcValue::Error(error)),
        };
        let bytes = left.len().saturating_add(right.len());
        self.text(bytes)?;
        Ok(CalcValue::Text(left + &right))
    }
}

fn finite_or_error(value: CalcValue) -> CalcValue {
    match value {
        CalcValue::Number(number) if !number.is_finite() => CalcValue::Error(CellError::Number),
        other => other,
    }
}

fn number_result(value: f64) -> CalcValue {
    if value.is_finite() {
        CalcValue::Number(value)
    } else {
        CalcValue::Error(CellError::Number)
    }
}

fn numeric_coercion(value: &CalcValue) -> Result<f64, CellError> {
    match value {
        CalcValue::Blank => Ok(0.0),
        CalcValue::Number(value) if value.is_finite() => Ok(*value),
        CalcValue::Number(_) => Err(CellError::Number),
        CalcValue::Boolean(value) => Ok(u8::from(*value).into()),
        CalcValue::Error(error) => Err(*error),
        CalcValue::Text(_) | CalcValue::Date(_) | CalcValue::DateTime(_) => Err(CellError::Value),
    }
}

fn logical_coercion(value: &CalcValue) -> Result<bool, CellError> {
    match value {
        CalcValue::Blank => Ok(false),
        CalcValue::Number(value) if value.is_finite() => Ok(*value != 0.0),
        CalcValue::Number(_) => Err(CellError::Number),
        CalcValue::Boolean(value) => Ok(*value),
        CalcValue::Error(error) => Err(*error),
        CalcValue::Text(_) | CalcValue::Date(_) | CalcValue::DateTime(_) => Err(CellError::Value),
    }
}

fn text_coercion(value: &CalcValue) -> Result<String, CellError> {
    match value {
        CalcValue::Blank => Ok(String::new()),
        CalcValue::Text(value) => Ok(value.clone()),
        CalcValue::Number(value) => canonical_number(*value).map_err(|_| CellError::Number),
        CalcValue::Boolean(true) => Ok("TRUE".to_owned()),
        CalcValue::Boolean(false) => Ok("FALSE".to_owned()),
        CalcValue::Date(value) => Ok(value.to_string()),
        CalcValue::DateTime(value) => value.format(&Rfc3339).map_err(|_| CellError::Number),
        CalcValue::Error(error) => Err(*error),
    }
}

fn arithmetic(operator: BinaryOperator, left: &CalcValue, right: &CalcValue) -> CalcValue {
    let left = match numeric_coercion(left) {
        Ok(value) => value,
        Err(error) => return CalcValue::Error(error),
    };
    let right = match numeric_coercion(right) {
        Ok(value) => value,
        Err(error) => return CalcValue::Error(error),
    };

    let value = match operator {
        BinaryOperator::Power => {
            if left == 0.0 && right < 0.0 {
                return CalcValue::Error(CellError::DivisionByZero);
            }
            if left < 0.0 && right.fract() != 0.0 {
                return CalcValue::Error(CellError::Number);
            }
            left.powf(right)
        }
        BinaryOperator::Multiply => left * right,
        BinaryOperator::Divide => {
            if right == 0.0 {
                return CalcValue::Error(CellError::DivisionByZero);
            }
            left / right
        }
        BinaryOperator::Add => left + right,
        BinaryOperator::Subtract => left - right,
        _ => unreachable!("caller supplies only arithmetic operators"),
    };
    number_result(value)
}

#[allow(clippy::float_cmp)] // The profile specifies exact Number equality.
fn equal_values(left: &CalcValue, right: &CalcValue) -> bool {
    match (left, right) {
        (CalcValue::Blank, CalcValue::Blank) => true,
        (CalcValue::Text(left), CalcValue::Text(right)) => left == right,
        (CalcValue::Number(left), CalcValue::Number(right)) => left == right,
        (CalcValue::Boolean(left), CalcValue::Boolean(right)) => left == right,
        (CalcValue::Date(left), CalcValue::Date(right)) => left == right,
        // The stored offset is part of a DateTime's exact typed value even
        // though ordering below compares DateTimes by instant.
        (CalcValue::DateTime(left), CalcValue::DateTime(right)) => {
            left == right && left.offset() == right.offset()
        }
        (CalcValue::Error(left), CalcValue::Error(right)) => left == right,
        _ => false,
    }
}

fn compare_values(operator: BinaryOperator, left: &CalcValue, right: &CalcValue) -> CalcValue {
    let ordering = match (left, right) {
        (CalcValue::Number(left), CalcValue::Number(right)) => left.partial_cmp(right),
        (CalcValue::Text(left), CalcValue::Text(right)) => Some(left.cmp(right)),
        (CalcValue::Boolean(left), CalcValue::Boolean(right)) => Some(left.cmp(right)),
        (CalcValue::Date(left), CalcValue::Date(right)) => Some(left.cmp(right)),
        (CalcValue::DateTime(left), CalcValue::DateTime(right)) => Some(left.cmp(right)),
        _ => None,
    };
    let Some(ordering) = ordering else {
        return CalcValue::Error(CellError::Value);
    };
    CalcValue::Boolean(match operator {
        BinaryOperator::Less => ordering == Ordering::Less,
        BinaryOperator::LessEqual => ordering != Ordering::Greater,
        BinaryOperator::Greater => ordering == Ordering::Greater,
        BinaryOperator::GreaterEqual => ordering != Ordering::Less,
        _ => unreachable!("caller supplies only ordering operators"),
    })
}

fn strict_integer(value: &CalcValue) -> Result<i32, CellError> {
    match value {
        CalcValue::Number(number)
            if number.is_finite()
                && number.fract() == 0.0
                && *number >= f64::from(i32::MIN)
                && *number <= f64::from(i32::MAX) =>
        {
            #[allow(clippy::cast_possible_truncation)]
            Ok(*number as i32)
        }
        CalcValue::Number(_) => Err(CellError::Number),
        CalcValue::Error(error) => Err(*error),
        _ => Err(CellError::Value),
    }
}

#[allow(clippy::cast_precision_loss)]
fn positive_index(value: &CalcValue) -> Result<usize, CellError> {
    let number = match value {
        CalcValue::Number(number) if number.is_finite() && number.fract() == 0.0 => *number,
        CalcValue::Number(_) => return Err(CellError::Number),
        CalcValue::Error(error) => return Err(*error),
        _ => return Err(CellError::Value),
    };
    if number < 1.0 || number > usize::MAX as f64 {
        return Err(CellError::Number);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(number as usize)
}

fn exact_mode(value: &CalcValue) -> Result<(), CellError> {
    match value {
        CalcValue::Number(value) if *value == 0.0 => Ok(()),
        CalcValue::Error(error) => Err(*error),
        _ => Err(CellError::Value),
    }
}

// Function implementations are kept below the evaluator so every function
// shares the same work accounting and left-to-right traversal helpers.
include!("functions.rs");

#[cfg(test)]
mod tests;
