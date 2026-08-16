use std::fmt;

use marksheet_model::{CellError, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use time::{Date, OffsetDateTime};

/// A calculated scalar value.
///
/// Formula source is deliberately absent: callers must parse and evaluate a
/// formula before it can enter calculation state.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcValue {
    Blank,
    Text(String),
    Number(f64),
    Boolean(bool),
    Date(Date),
    DateTime(OffsetDateTime),
    Error(CellError),
}

/// `CalcValue` deliberately delegates its wire representation to [`Value`].
///
/// Calculation results have the same scalar variants except for `Formula`,
/// which must be evaluated before it can be represented as a calculation
/// value. Keeping one wire implementation means source and calculated dates
/// always use the same ISO/RFC 3339 rules.
impl Serialize for CalcValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Blank => Value::Blank,
            Self::Text(value) => Value::Text(value.clone()),
            Self::Number(value) => Value::Number(*value),
            Self::Boolean(value) => Value::Boolean(*value),
            Self::Date(value) => Value::Date(*value),
            Self::DateTime(value) => Value::DateTime(*value),
            Self::Error(value) => Value::Error(*value),
        };
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CalcValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer)
            .and_then(|value| Self::try_from(value).map_err(D::Error::custom))
    }
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
    use time::{Month, format_description::well_known::Rfc3339};

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
        assert!(
            serde_json::from_value::<CalcValue>(serde_json::json!({
                "kind": "formula",
                "value": "=1"
            }))
            .is_err()
        );
    }

    #[test]
    fn dates_and_datetimes_use_string_wire_values_and_round_trip() {
        let date = Date::from_calendar_date(2024, Month::February, 29).unwrap();
        let datetime = OffsetDateTime::parse("2026-08-16T14:30:00.125-04:00", &Rfc3339).unwrap();

        for (source, calculated, expected) in [
            (
                Value::Date(date),
                CalcValue::Date(date),
                serde_json::json!({ "kind": "date", "value": "2024-02-29" }),
            ),
            (
                Value::DateTime(datetime),
                CalcValue::DateTime(datetime),
                serde_json::json!({
                    "kind": "date_time",
                    "value": "2026-08-16T14:30:00.125-04:00"
                }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&source).unwrap(), expected);
            assert_eq!(serde_json::to_value(&calculated).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<Value>(expected.clone()).unwrap(),
                source
            );
            assert_eq!(
                serde_json::from_value::<CalcValue>(expected).unwrap(),
                calculated
            );
        }

        let source = Value::DateTime(datetime);
        let calculated = CalcValue::DateTime(datetime);
        let source_round_trip =
            serde_json::from_value::<Value>(serde_json::to_value(source).unwrap()).unwrap();
        let calculated_round_trip =
            serde_json::from_value::<CalcValue>(serde_json::to_value(calculated).unwrap()).unwrap();
        assert!(
            matches!(source_round_trip, Value::DateTime(value) if value.offset() == datetime.offset())
        );
        assert!(
            matches!(calculated_round_trip, CalcValue::DateTime(value) if value.offset() == datetime.offset())
        );
    }

    #[test]
    fn date_wire_rejects_malformed_or_non_string_values() {
        let invalid_date_values = [
            serde_json::json!({ "kind": "date", "value": "2024-2-29" }),
            serde_json::json!({ "kind": "date", "value": "2023-02-29" }),
            serde_json::json!({ "kind": "date", "value": [2024, 2, 29] }),
        ];
        let invalid_datetime_values = [
            serde_json::json!({ "kind": "date_time", "value": "2026-08-16T14:30:00" }),
            serde_json::json!({ "kind": "date_time", "value": "2026-08-16t14:30:00Z" }),
            serde_json::json!({ "kind": "date_time", "value": "2026-08-16T14:30:00+25:00" }),
            serde_json::json!({ "kind": "date_time", "value": [2026, 8, 16] }),
        ];

        for value in invalid_date_values
            .into_iter()
            .chain(invalid_datetime_values)
        {
            assert!(serde_json::from_value::<Value>(value.clone()).is_err());
            assert!(serde_json::from_value::<CalcValue>(value).is_err());
        }
    }

    #[test]
    fn non_temporal_variants_keep_their_existing_wire_shape() {
        let formula = marksheet_model::FormulaSource::new("=A1").unwrap();
        let source_values = [
            (Value::Blank, serde_json::json!({ "kind": "blank" })),
            (
                Value::Text("text".to_owned()),
                serde_json::json!({ "kind": "text", "value": "text" }),
            ),
            (
                Value::Number(42.5),
                serde_json::json!({ "kind": "number", "value": 42.5 }),
            ),
            (
                Value::Boolean(true),
                serde_json::json!({ "kind": "boolean", "value": true }),
            ),
            (
                Value::Formula(formula),
                serde_json::json!({ "kind": "formula", "value": "=A1" }),
            ),
            (
                Value::Error(CellError::Reference),
                serde_json::json!({ "kind": "error", "value": "#REF!" }),
            ),
        ];
        for (value, expected) in source_values {
            assert_eq!(serde_json::to_value(value).unwrap(), expected);
        }

        let calculated_values = [
            (CalcValue::Blank, serde_json::json!({ "kind": "blank" })),
            (
                CalcValue::Text("text".to_owned()),
                serde_json::json!({ "kind": "text", "value": "text" }),
            ),
            (
                CalcValue::Number(42.5),
                serde_json::json!({ "kind": "number", "value": 42.5 }),
            ),
            (
                CalcValue::Boolean(true),
                serde_json::json!({ "kind": "boolean", "value": true }),
            ),
            (
                CalcValue::Error(CellError::Reference),
                serde_json::json!({ "kind": "error", "value": "#REF!" }),
            ),
        ];
        for (value, expected) in calculated_values {
            assert_eq!(serde_json::to_value(value).unwrap(), expected);
        }
    }
}
