use std::collections::BTreeMap;

use marksheet_model::{ByteSpan, CellError, Coordinate};
use time::{Date, Month, OffsetDateTime, format_description::well_known::Rfc3339};

use super::*;
use crate::formula::{ParseLimits, Reference, parse};

#[derive(Default)]
struct TestContext {
    cells: BTreeMap<Coordinate, CalcValue>,
    names: BTreeMap<String, ResolvedValue>,
}

impl TestContext {
    fn with(mut self, address: &str, value: CalcValue) -> Self {
        self.cells
            .insert(Coordinate::parse(address).unwrap(), value);
        self
    }

    fn with_name(mut self, name: &str, value: ResolvedValue) -> Self {
        self.names.insert(name.to_owned(), value);
        self
    }
}

impl EvaluationContext for TestContext {
    fn resolve(&self, reference: &Reference, _span: ByteSpan) -> Result<ResolvedValue, CellError> {
        match reference {
            Reference::Cell { address, .. } => Ok(self
                .cells
                .get(&address.coordinate)
                .cloned()
                .unwrap_or(CalcValue::Blank)
                .into()),
            Reference::Range(range) => {
                let start_column = range
                    .start
                    .coordinate
                    .column
                    .min(range.end.coordinate.column);
                let end_column = range
                    .start
                    .coordinate
                    .column
                    .max(range.end.coordinate.column);
                let start_row = range.start.coordinate.row.min(range.end.coordinate.row);
                let end_row = range.start.coordinate.row.max(range.end.coordinate.row);
                let rows = usize::try_from(end_row - start_row + 1).unwrap();
                let columns = usize::try_from(end_column - start_column + 1).unwrap();
                let mut values = Vec::with_capacity(rows * columns);
                for row in start_row..=end_row {
                    for column in start_column..=end_column {
                        let coordinate = Coordinate::new(column, row).unwrap();
                        values.push(
                            self.cells
                                .get(&coordinate)
                                .cloned()
                                .unwrap_or(CalcValue::Blank),
                        );
                    }
                }
                Ok(RectangularRange::new(rows, columns, values).unwrap().into())
            }
            Reference::Name { name } => self
                .names
                .get(name.as_str())
                .cloned()
                .ok_or(CellError::Name),
            Reference::Structured(_) => Err(CellError::Reference),
        }
    }
}

fn value(formula: &str, context: &TestContext) -> CalcValue {
    let formula = parse(formula, &ParseLimits::default()).unwrap();
    evaluate_with_defaults(&formula, context).unwrap().value
}

fn empty() -> TestContext {
    TestContext::default()
}

mod operators {
    use super::*;

    #[test]
    fn arithmetic_and_coercion() {
        let context = TestContext::default().with("A1", CalcValue::Blank);
        assert_eq!(value("=1+2*3", &context), CalcValue::Number(7.0));
        assert_eq!(value("=2^3^2", &context), CalcValue::Number(512.0));
        assert_eq!(value("=-2^2", &context), CalcValue::Number(-4.0));
        assert_eq!(value("=2^-2", &context), CalcValue::Number(0.25));
        assert_eq!(value("=A1+TRUE+2", &context), CalcValue::Number(3.0));
        assert_eq!(
            value("=\"2\"+1", &context),
            CalcValue::Error(CellError::Value)
        );
    }

    #[test]
    fn arithmetic_domain_errors_are_values() {
        assert_eq!(
            value("=1/0", &empty()),
            CalcValue::Error(CellError::DivisionByZero)
        );
        assert_eq!(value("=0^0", &empty()), CalcValue::Number(1.0));
        assert_eq!(
            value("=0^-1", &empty()),
            CalcValue::Error(CellError::DivisionByZero)
        );
        assert_eq!(
            value("=(-1)^0.5", &empty()),
            CalcValue::Error(CellError::Number)
        );
        assert_eq!(
            value("=1e308*1e308", &empty()),
            CalcValue::Error(CellError::Number)
        );
    }

    #[test]
    fn concatenation_and_comparison_follow_typed_rules() {
        let date = Date::from_calendar_date(2026, Month::August, 16).unwrap();
        let context = TestContext::default()
            .with("A1", CalcValue::Blank)
            .with("A2", CalcValue::Text(String::new()))
            .with("A3", CalcValue::Date(date));
        assert_eq!(
            value("=A1&12&TRUE&A3", &context),
            CalcValue::Text("12TRUE2026-08-16".to_owned())
        );
        assert_eq!(value("=A1=A2", &context), CalcValue::Boolean(false));
        assert_eq!(value("=1=TRUE", &context), CalcValue::Boolean(false));
        assert_eq!(value("=\"A\"<\"a\"", &context), CalcValue::Boolean(true));
        assert_eq!(
            value("=1<TRUE", &context),
            CalcValue::Error(CellError::Value)
        );
    }

    #[test]
    fn errors_propagate_left_to_right() {
        assert_eq!(
            value("=#N/A+#REF!", &empty()),
            CalcValue::Error(CellError::NotAvailable)
        );
        assert_eq!(
            value("=1+#REF!", &empty()),
            CalcValue::Error(CellError::Reference)
        );
        assert_eq!(
            value("=MISSING(1/0)", &empty()),
            CalcValue::Error(CellError::Name)
        );
    }

    #[test]
    fn one_cell_range_syntax_remains_a_range() {
        let context = TestContext::default().with("A1", CalcValue::Number(4.0));
        assert_eq!(
            value("=A1:A1", &context),
            CalcValue::Error(CellError::Value)
        );
        assert_eq!(value("=SUM(A1:A1)", &context), CalcValue::Number(4.0));
        assert_eq!(value("=INDEX(A1:A1,1)", &context), CalcValue::Number(4.0));
    }

    #[test]
    fn datetime_equality_preserves_offset_but_ordering_uses_instant() {
        let first = OffsetDateTime::parse("2026-01-01T01:00:00+01:00", &Rfc3339).unwrap();
        let second = OffsetDateTime::parse("2026-01-01T00:00:00Z", &Rfc3339).unwrap();
        let context = TestContext::default()
            .with("A1", CalcValue::DateTime(first))
            .with("A2", CalcValue::DateTime(second));
        assert_eq!(value("=A1=A2", &context), CalcValue::Boolean(false));
        assert_eq!(value("=A1<=A2", &context), CalcValue::Boolean(true));
        assert_eq!(value("=A1>=A2", &context), CalcValue::Boolean(true));
    }
}

mod aggregation_and_logic {
    use super::*;

    fn mixed_context() -> TestContext {
        TestContext::default()
            .with("A1", CalcValue::Number(2.0))
            .with("A2", CalcValue::Blank)
            .with("A3", CalcValue::Text("3".to_owned()))
            .with("A4", CalcValue::Boolean(true))
            .with("A5", CalcValue::Number(4.0))
    }

    #[test]
    fn aggregations_filter_types_and_define_empty_results() {
        let context = mixed_context();
        assert_eq!(value("=SUM(A1:A5)", &context), CalcValue::Number(6.0));
        assert_eq!(value("=AVERAGE(A1:A5)", &context), CalcValue::Number(3.0));
        assert_eq!(value("=MIN(3,-1,2)", &context), CalcValue::Number(-1.0));
        assert_eq!(value("=MAX(3,-1,2)", &context), CalcValue::Number(3.0));
        assert_eq!(value("=COUNT(A1:A5)", &context), CalcValue::Number(2.0));
        assert_eq!(
            value("=AVERAGE(\"x\")", &context),
            CalcValue::Error(CellError::DivisionByZero)
        );
        assert_eq!(
            value("=MIN(\"x\")", &context),
            CalcValue::Error(CellError::Value)
        );
    }

    #[test]
    fn counta_counts_errors_while_other_aggregates_propagate_them() {
        let context = TestContext::default()
            .with("A1", CalcValue::Blank)
            .with("A2", CalcValue::Text(String::new()))
            .with("A3", CalcValue::Error(CellError::NotAvailable));
        assert_eq!(value("=COUNTA(A1:A3)", &context), CalcValue::Number(2.0));
        assert_eq!(
            value("=SUM(A1:A3)", &context),
            CalcValue::Error(CellError::NotAvailable)
        );
    }

    #[test]
    fn empty_ranges_have_normal_aggregate_results() {
        let context = TestContext::default().with_name(
            "empty_data",
            RectangularRange::new(0, 1, Vec::new()).unwrap().into(),
        );
        assert_eq!(value("=SUM(empty_data)", &context), CalcValue::Number(0.0));
        assert_eq!(
            value("=COUNT(empty_data)", &context),
            CalcValue::Number(0.0)
        );
        assert_eq!(
            value("=AVERAGE(empty_data)", &context),
            CalcValue::Error(CellError::DivisionByZero)
        );
    }

    #[test]
    fn lazy_logic_does_not_evaluate_unused_inputs() {
        assert_eq!(value("=IF(TRUE,7,1/0)", &empty()), CalcValue::Number(7.0));
        assert_eq!(value("=IFERROR(3,#N/A)", &empty()), CalcValue::Number(3.0));
        assert_eq!(
            value("=AND(FALSE,#N/A)", &empty()),
            CalcValue::Boolean(false)
        );
        assert_eq!(value("=OR(TRUE,#N/A)", &empty()), CalcValue::Boolean(true));
        assert_eq!(value("=NOT(0)", &empty()), CalcValue::Boolean(true));
        assert_eq!(
            value("=AND(#N/A,FALSE)", &empty()),
            CalcValue::Error(CellError::NotAvailable)
        );
    }
}

mod numeric_and_text {
    use super::*;

    #[test]
    fn numeric_functions_cover_sign_and_rounding_modes() {
        assert_eq!(value("=ABS(-3)", &empty()), CalcValue::Number(3.0));
        assert_eq!(value("=INT(-1.2)", &empty()), CalcValue::Number(-2.0));
        assert_eq!(value("=MOD(-5,3)", &empty()), CalcValue::Number(1.0));
        assert_eq!(value("=MOD(5,-3)", &empty()), CalcValue::Number(-1.0));
        assert_eq!(value("=ROUND(-2.5,0)", &empty()), CalcValue::Number(-3.0));
        assert_eq!(value("=ROUND(125,-1)", &empty()), CalcValue::Number(130.0));
        assert_eq!(
            value("=ROUNDUP(-1.21,1)", &empty()),
            CalcValue::Number(-1.3)
        );
        assert_eq!(
            value("=ROUNDDOWN(-1.29,1)", &empty()),
            CalcValue::Number(-1.2)
        );
    }

    #[test]
    fn rounding_uses_the_exact_binary64_value() {
        assert_eq!(value("=ROUND(0.15,1)", &empty()), CalcValue::Number(0.1));
        assert_eq!(value("=ROUND(2.675,2)", &empty()), CalcValue::Number(2.67));
        assert_eq!(value("=ROUND(-0.15,1)", &empty()), CalcValue::Number(-0.1));
        assert_eq!(value("=ROUNDUP(0.15,1)", &empty()), CalcValue::Number(0.2));
        assert_eq!(value("=ROUNDUP(0.25,2)", &empty()), CalcValue::Number(0.25));
        assert_eq!(
            value("=ROUNDDOWN(2.675,2)", &empty()),
            CalcValue::Number(2.67)
        );
    }

    #[test]
    fn rounding_places_left_of_the_point_keep_their_sign() {
        assert_eq!(
            value("=ROUND(-1500,-3)", &empty()),
            CalcValue::Number(-2000.0)
        );
        assert_eq!(
            value("=ROUNDUP(-125,-2)", &empty()),
            CalcValue::Number(-200.0)
        );
        assert_eq!(
            value("=ROUNDDOWN(-125,-1)", &empty()),
            CalcValue::Number(-120.0)
        );
        assert_eq!(
            value("=ROUNDDOWN(125,-3)", &empty()),
            CalcValue::Number(0.0)
        );
        assert_eq!(
            value("=ROUNDUP(125,-3)", &empty()),
            CalcValue::Number(1000.0)
        );
    }

    #[test]
    fn numeric_function_domains_are_checked() {
        assert_eq!(
            value("=MOD(1,0)", &empty()),
            CalcValue::Error(CellError::DivisionByZero)
        );
        assert_eq!(
            value("=ROUND(1,1.5)", &empty()),
            CalcValue::Error(CellError::Number)
        );
        assert_eq!(
            value("=ROUND(1,309)", &empty()),
            CalcValue::Error(CellError::Number)
        );
        assert_eq!(
            value("=ROUNDUP(1.7976931348623157E308,-308)", &empty()),
            CalcValue::Error(CellError::Number)
        );
    }

    #[test]
    fn text_functions_use_unicode_scalar_positions_and_ascii_case() {
        assert_eq!(
            value("=LEFT(\"A😀b\",2)", &empty()),
            CalcValue::Text("A😀".to_owned())
        );
        assert_eq!(
            value("=RIGHT(\"A😀b\",2)", &empty()),
            CalcValue::Text("😀b".to_owned())
        );
        assert_eq!(
            value("=MID(\"A😀bc\",2,2)", &empty()),
            CalcValue::Text("😀b".to_owned())
        );
        assert_eq!(value("=LEN(\"A😀\")", &empty()), CalcValue::Number(2.0));
        assert_eq!(
            value("=LOWER(\"ÄBC\")", &empty()),
            CalcValue::Text("Äbc".to_owned())
        );
        assert_eq!(
            value("=UPPER(\"äbc\")", &empty()),
            CalcValue::Text("äBC".to_owned())
        );
        assert_eq!(
            value("=TRIM(\"  a   b  \")", &empty()),
            CalcValue::Text("a b".to_owned())
        );
    }

    #[test]
    fn concat_flattens_ranges_row_major() {
        let context = TestContext::default()
            .with("A1", CalcValue::Text("a".to_owned()))
            .with("B1", CalcValue::Number(2.0))
            .with("A2", CalcValue::Boolean(true))
            .with("B2", CalcValue::Text("z".to_owned()));
        assert_eq!(
            value("=CONCAT(A1:B2)", &context),
            CalcValue::Text("a2TRUEz".to_owned())
        );
    }

    #[test]
    fn numeric_text_uses_canonical_number_spelling() {
        assert_eq!(
            value("=CONCAT(1e20)", &empty()),
            CalcValue::Text("1e20".to_owned())
        );
        assert_eq!(
            value("=CONCAT(1e-7)", &empty()),
            CalcValue::Text("1e-7".to_owned())
        );
        assert_eq!(
            value("=CONCAT(-0)", &empty()),
            CalcValue::Text("-0".to_owned())
        );
        assert_eq!(
            text_coercion(&CalcValue::Number(f64::INFINITY)),
            Err(CellError::Number)
        );
    }
}

mod lookup_date_and_inspection {
    use super::*;

    #[test]
    fn index_and_match_observe_shape_order_and_errors() {
        let context = TestContext::default()
            .with("A1", CalcValue::Text("x".to_owned()))
            .with("B1", CalcValue::Text("b".to_owned()))
            .with("A2", CalcValue::Text("y".to_owned()))
            .with("B2", CalcValue::Text("x".to_owned()));
        assert_eq!(
            value("=INDEX(A1:B2,2,2)", &context),
            CalcValue::Text("x".to_owned())
        );
        assert_eq!(
            value("=MATCH(\"x\",A1:A2,0)", &context),
            CalcValue::Number(1.0)
        );
        assert_eq!(
            value("=INDEX(A1:B2,2)", &context),
            CalcValue::Error(CellError::Value)
        );
        assert_eq!(
            value("=MATCH(\"q\",A1:A2)", &context),
            CalcValue::Error(CellError::NotAvailable)
        );
        assert_eq!(
            value("=MATCH(\"x\",A1:A2,1)", &context),
            CalcValue::Error(CellError::Value)
        );
        assert_eq!(
            value("=INDEX(A1,1)", &context),
            CalcValue::Error(CellError::Value)
        );
        assert_eq!(
            value("=MATCH(\"x\",A1,0)", &context),
            CalcValue::Error(CellError::Value)
        );
    }

    #[test]
    fn empty_lookup_ranges_return_ordinary_errors() {
        let context = TestContext::default().with_name(
            "empty_data",
            RectangularRange::new(0, 1, Vec::new()).unwrap().into(),
        );
        assert_eq!(
            value("=INDEX(empty_data,1)", &context),
            CalcValue::Error(CellError::Reference)
        );
        assert_eq!(
            value("=MATCH(1,empty_data,0)", &context),
            CalcValue::Error(CellError::NotAvailable)
        );
    }

    #[test]
    fn dates_are_strict_and_datetimes_use_their_stored_offset() {
        let datetime = OffsetDateTime::parse("2026-01-01T00:30:00+02:00", &Rfc3339).unwrap();
        let context = TestContext::default().with("A1", CalcValue::DateTime(datetime));
        assert_eq!(
            value("=DATE(2024,2,29)", &context),
            CalcValue::Date(Date::from_calendar_date(2024, Month::February, 29).unwrap())
        );
        assert_eq!(
            value("=DATE(2023,2,29)", &context),
            CalcValue::Error(CellError::Number)
        );
        assert_eq!(value("=YEAR(A1)", &context), CalcValue::Number(2026.0));
        assert_eq!(value("=MONTH(A1)", &context), CalcValue::Number(1.0));
        assert_eq!(value("=DAY(A1)", &context), CalcValue::Number(1.0));
    }

    #[test]
    fn inspection_functions_do_not_propagate_errors() {
        let context = TestContext::default()
            .with("A1", CalcValue::Blank)
            .with("A2", CalcValue::Text(String::new()));
        assert_eq!(value("=ISBLANK(A1)", &context), CalcValue::Boolean(true));
        assert_eq!(value("=ISBLANK(A2)", &context), CalcValue::Boolean(false));
        assert_eq!(value("=ISNUMBER(1)", &context), CalcValue::Boolean(true));
        assert_eq!(value("=ISTEXT(\"1\")", &context), CalcValue::Boolean(true));
        assert_eq!(value("=ISERROR(#N/A)", &context), CalcValue::Boolean(true));
        assert_eq!(
            value("=ISNUMBER(#N/A)", &context),
            CalcValue::Boolean(false)
        );
    }
}

mod limits {
    use super::*;

    #[test]
    fn work_limits_are_operational_errors() {
        let formula = parse("=1", &ParseLimits::default()).unwrap();
        let error = evaluate(
            &formula,
            &empty(),
            &EvaluationLimits {
                max_steps: 0,
                ..EvaluationLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            EvaluationError::StepLimitExceeded { limit: 0, .. }
        ));
        assert_eq!(
            error.stats(),
            EvaluationStats {
                steps: 1,
                range_cells: 0,
                text_bytes: 0,
            }
        );

        let formula = parse("=SUM(A1:A2)", &ParseLimits::default()).unwrap();
        let error = evaluate(
            &formula,
            &empty(),
            &EvaluationLimits {
                max_range_cells: 1,
                ..EvaluationLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            EvaluationError::RangeCellLimitExceeded { limit: 1, .. }
        ));
        assert_eq!(
            error.stats(),
            EvaluationStats {
                steps: 2,
                range_cells: 2,
                text_bytes: 0,
            }
        );
    }

    #[test]
    fn text_limit_is_not_a_cell_error() {
        let formula = parse("=\"abcd\"", &ParseLimits::default()).unwrap();
        let error = evaluate(
            &formula,
            &empty(),
            &EvaluationLimits {
                max_text_bytes: 3,
                ..EvaluationLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            EvaluationError::TextByteLimitExceeded { limit: 3, .. }
        ));
        assert_eq!(
            error.stats(),
            EvaluationStats {
                steps: 1,
                range_cells: 0,
                text_bytes: 4,
            }
        );

        let formula = parse("=A1", &ParseLimits::default()).unwrap();
        let context = TestContext::default().with("A1", CalcValue::Text("abcd".to_owned()));
        let error = evaluate(
            &formula,
            &context,
            &EvaluationLimits {
                max_text_bytes: 3,
                ..EvaluationLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            EvaluationError::TextByteLimitExceeded { limit: 3, .. }
        ));
        assert_eq!(
            error.stats(),
            EvaluationStats {
                steps: 1,
                range_cells: 0,
                text_bytes: 4,
            }
        );
    }
}
