use std::fmt;

use marksheet_model::{CellError, Value};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

/// A calculated scalar value.
///
/// Formula source is deliberately absent: callers must parse and evaluate a
/// formula before it can enter calculation state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CalcValue {
    Blank,
    Text(String),
    Number(f64),
    Boolean(bool),
    Date(Date),
    DateTime(OffsetDateTime),
    Error(CellError),
}

impl CalcValue {
    #[must_use]
    pub const fn error(error: CellError) -> Self {
        Self::Error(error)
    }

    #[must_use]
    pub const fn as_error(&self) -> Option<CellError> {
        match self {
            Self::Error(error) => Some(*error),
            _ => None,
        }
    }
}

/// A source [`Value`] that cannot be used directly as a calculated scalar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormulaValueError;

impl fmt::Display for FormulaValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("formula source must be evaluated before conversion to CalcValue")
    }
}

impl std::error::Error for FormulaValueError {}

impl TryFrom<Value> for CalcValue {
    type Error = FormulaValueError;

    fn try_from(value: Value) -> Result<Self, FormulaValueError> {
        Ok(match value {
            Value::Blank => Self::Blank,
            Value::Text(value) => Self::Text(value),
            Value::Number(value) => Self::Number(value),
            Value::Boolean(value) => Self::Boolean(value),
            Value::Date(value) => Self::Date(value),
            Value::DateTime(value) => Self::DateTime(value),
            Value::Error(value) => Self::Error(value),
            Value::Formula(_) => return Err(FormulaValueError),
        })
    }
}

impl TryFrom<&Value> for CalcValue {
    type Error = FormulaValueError;

    fn try_from(value: &Value) -> Result<Self, FormulaValueError> {
        Self::try_from(value.clone())
    }
}

/// A finite, possibly empty rectangular value, stored in row-major order.
#[derive(Clone, Debug, PartialEq)]
pub struct RectangularRange {
    rows: usize,
    columns: usize,
    values: Vec<CalcValue>,
}

impl RectangularRange {
    /// Constructs a range after validating its dimensions and cell count.
    ///
    /// A zero-length axis is valid when the other axis retains the range's
    /// shape. This represents, for example, the data region of a header-only
    /// table. A `0 x 0` range has no meaningful shape and is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`RangeShapeError`] if both dimensions are zero, their product
    /// overflows, or the number of values does not match the dimensions.
    pub fn new(
        rows: usize,
        columns: usize,
        values: Vec<CalcValue>,
    ) -> Result<Self, RangeShapeError> {
        if rows == 0 && columns == 0 {
            return Err(RangeShapeError::MissingShape);
        }
        let expected = rows
            .checked_mul(columns)
            .ok_or(RangeShapeError::SizeOverflow)?;
        if values.len() != expected {
            return Err(RangeShapeError::LengthMismatch {
                expected,
                actual: values.len(),
            });
        }
        Ok(Self {
            rows,
            columns,
            values,
        })
    }

    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    #[must_use]
    pub fn values(&self) -> &[CalcValue] {
        &self.values
    }

    #[must_use]
    pub fn get(&self, row: usize, column: usize) -> Option<&CalcValue> {
        if row >= self.rows || column >= self.columns {
            return None;
        }
        self.values.get(row * self.columns + column)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RangeShapeError {
    MissingShape,
    SizeOverflow,
    LengthMismatch { expected: usize, actual: usize },
}

impl fmt::Display for RangeShapeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingShape => formatter.write_str("an empty range must retain one dimension"),
            Self::SizeOverflow => formatter.write_str("range dimensions overflow usize"),
            Self::LengthMismatch { expected, actual } => write!(
                formatter,
                "range dimensions require {expected} values, but {actual} were supplied"
            ),
        }
    }
}

impl std::error::Error for RangeShapeError {}

/// The value returned by reference resolution.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedValue {
    Scalar(CalcValue),
    Range(RectangularRange),
}

impl From<CalcValue> for ResolvedValue {
    fn from(value: CalcValue) -> Self {
        Self::Scalar(value)
    }
}

impl From<RectangularRange> for ResolvedValue {
    fn from(value: RectangularRange) -> Self {
        Self::Range(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_rejects_invalid_shapes() {
        assert_eq!(
            RectangularRange::new(0, 0, Vec::new()),
            Err(RangeShapeError::MissingShape)
        );
        assert!(RectangularRange::new(0, 2, Vec::new()).unwrap().is_empty());
        assert_eq!(
            RectangularRange::new(2, 2, vec![CalcValue::Blank]),
            Err(RangeShapeError::LengthMismatch {
                expected: 4,
                actual: 1
            })
        );
        assert_eq!(
            RectangularRange::new(usize::MAX, 2, Vec::new()),
            Err(RangeShapeError::SizeOverflow)
        );
    }

    #[test]
    fn formulas_do_not_convert_to_calculated_values() {
        let formula = marksheet_model::FormulaSource::new("=1").unwrap();
        assert_eq!(
            CalcValue::try_from(Value::Formula(formula)),
            Err(FormulaValueError)
        );
    }
}
