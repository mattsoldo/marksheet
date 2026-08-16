use std::fmt;

use marksheet_model::{Coordinate, CoordinateError};
use serde::{Deserialize, Serialize};

use super::ast::{A1Reference, Expr, ExprKind, Formula, RangeReference, Reference};

/// Signed displacement between a formula's authored cell and a copied cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CopyOffset {
    pub columns: i128,
    pub rows: i128,
}

impl CopyOffset {
    #[must_use]
    pub fn between(origin: Coordinate, target: Coordinate) -> Self {
        Self {
            columns: i128::from(target.column) - i128::from(origin.column),
            rows: i128::from(target.row) - i128::from(origin.row),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdjustmentError {
    ColumnOutOfBounds,
    RowOutOfBounds,
}

impl fmt::Display for AdjustmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ColumnOutOfBounds => {
                formatter.write_str("copied formula column reference is out of bounds")
            }
            Self::RowOutOfBounds => {
                formatter.write_str("copied formula row reference is out of bounds")
            }
        }
    }
}

impl std::error::Error for AdjustmentError {}

/// A formula interpreted at an origin cell, as required by an A1 `@fill`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormulaTemplate {
    pub origin: Coordinate,
    pub formula: Formula,
}

impl FormulaTemplate {
    #[must_use]
    pub const fn new(origin: Coordinate, formula: Formula) -> Self {
        Self { origin, formula }
    }

    /// Copies the template to `target`, adjusting only relative A1 axes.
    /// Structured and named references are intentionally unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`AdjustmentError`] when a translated relative axis would be
    /// zero or overflow `u64`.
    pub fn bind(&self, target: Coordinate) -> Result<Formula, AdjustmentError> {
        Ok(Formula {
            expression: adjust_references(
                &self.formula.expression,
                CopyOffset::between(self.origin, target),
            )?,
        })
    }
}

/// Clones an expression while translating its relative A1 reference axes.
///
/// Source spans are retained because a virtual fill formula still originates
/// from the single authored `@fill` expression.
///
/// # Errors
///
/// Returns [`AdjustmentError`] when a translated relative axis would be zero
/// or overflow `u64`.
pub fn adjust_references(expression: &Expr, offset: CopyOffset) -> Result<Expr, AdjustmentError> {
    let kind = match &expression.kind {
        ExprKind::Literal { value } => ExprKind::Literal {
            value: value.clone(),
        },
        ExprKind::Reference { reference } => ExprKind::Reference {
            reference: adjust_reference(reference, offset)?,
        },
        ExprKind::Unary { operator, operand } => ExprKind::Unary {
            operator: *operator,
            operand: Box::new(adjust_references(operand, offset)?),
        },
        ExprKind::Binary {
            operator,
            left,
            right,
        } => ExprKind::Binary {
            operator: *operator,
            left: Box::new(adjust_references(left, offset)?),
            right: Box::new(adjust_references(right, offset)?),
        },
        ExprKind::Call { call } => ExprKind::Call {
            call: super::ast::FunctionCall {
                name: call.name.clone(),
                arguments: call
                    .arguments
                    .iter()
                    .map(|argument| adjust_references(argument, offset))
                    .collect::<Result<Vec<_>, _>>()?,
            },
        },
    };
    Ok(Expr {
        kind,
        span: expression.span,
    })
}

fn adjust_reference(
    reference: &Reference,
    offset: CopyOffset,
) -> Result<Reference, AdjustmentError> {
    Ok(match reference {
        Reference::Cell { sheet, address } => Reference::Cell {
            sheet: sheet.clone(),
            address: adjust_a1(address, offset)?,
        },
        Reference::Range(range) => Reference::Range(RangeReference {
            sheet: range.sheet.clone(),
            start: adjust_a1(&range.start, offset)?,
            end: adjust_a1(&range.end, offset)?,
        }),
        Reference::Name { name } => Reference::Name { name: name.clone() },
        Reference::Structured(structured) => Reference::Structured(structured.clone()),
    })
}

fn adjust_a1(reference: &A1Reference, offset: CopyOffset) -> Result<A1Reference, AdjustmentError> {
    let column = if reference.column_absolute {
        reference.coordinate.column
    } else {
        adjust_axis(reference.coordinate.column, offset.columns)
            .ok_or(AdjustmentError::ColumnOutOfBounds)?
    };
    let row = if reference.row_absolute {
        reference.coordinate.row
    } else {
        adjust_axis(reference.coordinate.row, offset.rows).ok_or(AdjustmentError::RowOutOfBounds)?
    };
    let coordinate = Coordinate::new(column, row).map_err(|error| match error {
        CoordinateError::ZeroColumn => AdjustmentError::ColumnOutOfBounds,
        CoordinateError::ZeroRow => AdjustmentError::RowOutOfBounds,
        CoordinateError::Invalid { .. } | CoordinateError::Overflow => {
            AdjustmentError::ColumnOutOfBounds
        }
    })?;
    Ok(A1Reference {
        coordinate,
        column_absolute: reference.column_absolute,
        row_absolute: reference.row_absolute,
    })
}

fn adjust_axis(value: u64, delta: i128) -> Option<u64> {
    if delta >= 0 {
        let amount = u64::try_from(delta).ok()?;
        value.checked_add(amount)
    } else {
        let amount = u64::try_from(delta.unsigned_abs()).ok()?;
        value.checked_sub(amount).filter(|result| *result > 0)
    }
}

#[cfg(test)]
mod tests {
    use marksheet_model::Coordinate;

    use super::*;
    use crate::formula::{ParseLimits, format_formula, parse};

    fn coordinate(value: &str) -> Coordinate {
        Coordinate::parse(value).expect("valid coordinate")
    }

    #[test]
    fn fill_binding_adjusts_only_relative_axes() {
        let template = FormulaTemplate::new(
            coordinate("C2"),
            parse("=A2+$B2+C$1+$D$4", &ParseLimits::default()).expect("valid formula"),
        );
        let bound = template.bind(coordinate("E5")).expect("valid copy");
        assert_eq!(
            format_formula(&bound).expect("formattable"),
            "=C5+$B5+E$1+$D$4"
        );
    }

    #[test]
    fn fill_binding_reports_reference_underflow() {
        let template = FormulaTemplate::new(
            coordinate("B2"),
            parse("=A1", &ParseLimits::default()).expect("valid formula"),
        );
        let error = template
            .bind(coordinate("A1"))
            .expect_err("copy underflows");
        assert_eq!(error, AdjustmentError::ColumnOutOfBounds);
    }

    #[test]
    fn names_and_structured_references_do_not_move() {
        let template = FormulaTemplate::new(
            coordinate("A1"),
            parse("=tax_rate+costs[@Cost]", &ParseLimits::default()).expect("valid formula"),
        );
        let bound = template.bind(coordinate("Z99")).expect("valid copy");
        assert_eq!(
            format_formula(&bound).expect("formattable"),
            "=tax_rate+costs[@Cost]"
        );
    }
}
