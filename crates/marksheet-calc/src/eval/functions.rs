impl<C: EvaluationContext + ?Sized> Evaluator<'_, C> {
    fn aggregate(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if call.arguments.is_empty() {
            return Ok(error_value(CellError::Value));
        }

        let mut numbers = Vec::new();
        let mut counta = 0usize;
        for argument in &call.arguments {
            let value = self.evaluated_argument(argument)?;
            match value {
                RuntimeValue::Scalar(value) => {
                    let value = finite_or_error(value);
                    if let Some(error) = collect_aggregate_value(
                        call.name.as_str(),
                        &value,
                        &mut numbers,
                        &mut counta,
                    ) {
                        return Ok(error_value(error));
                    }
                }
                RuntimeValue::Range(range) => {
                    for value in range.values() {
                        self.range_cell()?;
                        if let Some(error) = collect_aggregate_value(
                            call.name.as_str(),
                            value,
                            &mut numbers,
                            &mut counta,
                        ) {
                            return Ok(error_value(error));
                        }
                    }
                }
            }
        }

        let value = match call.name.as_str() {
            "COUNTA" => count_result(counta),
            "COUNT" => count_result(numbers.len()),
            "SUM" => sum_numbers(&numbers),
            "AVERAGE" if numbers.is_empty() => CalcValue::Error(CellError::DivisionByZero),
            "AVERAGE" => match sum_numbers(&numbers) {
                #[allow(clippy::cast_precision_loss)]
                CalcValue::Number(sum) => number_result(sum / numbers.len() as f64),
                error => error,
            },
            "MIN" | "MAX" if numbers.is_empty() => CalcValue::Error(CellError::Value),
            "MIN" => CalcValue::Number(
                numbers
                    .iter()
                    .copied()
                    .reduce(f64::min)
                    .expect("non-empty numbers"),
            ),
            "MAX" => CalcValue::Number(
                numbers
                    .iter()
                    .copied()
                    .reduce(f64::max)
                    .expect("non-empty numbers"),
            ),
            _ => unreachable!("aggregate dispatcher validates the function name"),
        };
        Ok(RuntimeValue::Scalar(value))
    }

    fn function_if(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if call.arguments.len() != 3 {
            return Ok(error_value(CellError::Value));
        }
        let condition = self.scalar(&call.arguments[0])?;
        let condition = match logical_coercion(&condition) {
            Ok(condition) => condition,
            Err(error) => return Ok(error_value(error)),
        };
        self.expression(&call.arguments[usize::from(!condition) + 1])
    }

    fn function_iferror(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if call.arguments.len() != 2 {
            return Ok(error_value(CellError::Value));
        }
        let value = self.expression(&call.arguments[0])?;
        if matches!(value, RuntimeValue::Scalar(CalcValue::Error(_))) {
            self.expression(&call.arguments[1])
        } else {
            Ok(value)
        }
    }

    fn function_and_or(
        &mut self,
        call: &FunctionCall,
        is_or: bool,
    ) -> Result<RuntimeValue, EvaluationError> {
        if call.arguments.is_empty() {
            return Ok(error_value(CellError::Value));
        }
        for argument in &call.arguments {
            let value = self.expression(argument)?;
            match value {
                RuntimeValue::Scalar(value) => {
                    let logical = match logical_coercion(&finite_or_error(value)) {
                        Ok(value) => value,
                        Err(error) => return Ok(error_value(error)),
                    };
                    if logical == is_or {
                        return Ok(RuntimeValue::Scalar(CalcValue::Boolean(is_or)));
                    }
                }
                RuntimeValue::Range(range) => {
                    for value in range.values() {
                        self.range_cell()?;
                        let logical = match logical_coercion(value) {
                            Ok(value) => value,
                            Err(error) => return Ok(error_value(error)),
                        };
                        if logical == is_or {
                            return Ok(RuntimeValue::Scalar(CalcValue::Boolean(is_or)));
                        }
                    }
                }
            }
        }
        Ok(RuntimeValue::Scalar(CalcValue::Boolean(!is_or)))
    }

    fn function_not(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if call.arguments.len() != 1 {
            return Ok(error_value(CellError::Value));
        }
        let value = self.scalar(&call.arguments[0])?;
        Ok(RuntimeValue::Scalar(match logical_coercion(&value) {
            Ok(value) => CalcValue::Boolean(!value),
            Err(error) => CalcValue::Error(error),
        }))
    }

    fn numeric_function(
        &mut self,
        call: &FunctionCall,
    ) -> Result<RuntimeValue, EvaluationError> {
        let expected = match call.name.as_str() {
            "ABS" | "INT" => 1,
            "MOD" | "ROUND" | "ROUNDUP" | "ROUNDDOWN" => 2,
            _ => unreachable!("numeric dispatcher validates the function name"),
        };
        if call.arguments.len() != expected {
            return Ok(error_value(CellError::Value));
        }
        let first = self.scalar(&call.arguments[0])?;
        if let Some(error) = first.as_error() {
            return Ok(error_value(error));
        }
        let first = match numeric_coercion(&first) {
            Ok(value) => value,
            Err(error) => return Ok(error_value(error)),
        };

        let value = match call.name.as_str() {
            "ABS" => number_result(first.abs()),
            "INT" => number_result(first.floor()),
            "MOD" => {
                let divisor = self.scalar(&call.arguments[1])?;
                if let Some(error) = divisor.as_error() {
                    return Ok(error_value(error));
                }
                let divisor = match numeric_coercion(&divisor) {
                    Ok(value) => value,
                    Err(error) => return Ok(error_value(error)),
                };
                if divisor == 0.0 {
                    CalcValue::Error(CellError::DivisionByZero)
                } else {
                    number_result(first - divisor * (first / divisor).floor())
                }
            }
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                let digits = self.scalar(&call.arguments[1])?;
                if let Some(error) = digits.as_error() {
                    return Ok(error_value(error));
                }
                let digits = match strict_integer(&digits) {
                    Ok(value @ -308..=308) => value,
                    Ok(_) | Err(CellError::Number) => {
                        return Ok(error_value(CellError::Number));
                    }
                    Err(error) => return Ok(error_value(error)),
                };
                round_decimal(first, digits, call.name.as_str())
            }
            _ => unreachable!("numeric dispatcher validates the function name"),
        };
        Ok(RuntimeValue::Scalar(value))
    }

    fn text_function(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if call.name == "CONCAT" {
            return self.function_concat(call);
        }
        let valid_arity = match call.name.as_str() {
            "LEFT" | "RIGHT" => matches!(call.arguments.len(), 1 | 2),
            "MID" => call.arguments.len() == 3,
            "LEN" | "LOWER" | "UPPER" | "TRIM" => call.arguments.len() == 1,
            _ => false,
        };
        if !valid_arity {
            return Ok(error_value(CellError::Value));
        }

        let input = self.scalar(&call.arguments[0])?;
        if let Some(error) = input.as_error() {
            return Ok(error_value(error));
        }
        let input = match text_coercion(&input) {
            Ok(value) => value,
            Err(error) => return Ok(error_value(error)),
        };

        let value = match call.name.as_str() {
            "LEN" => count_result(input.chars().count()),
            "LOWER" => {
                let output = input.to_ascii_lowercase();
                self.text(output.len())?;
                CalcValue::Text(output)
            }
            "UPPER" => {
                let output = input.to_ascii_uppercase();
                self.text(output.len())?;
                CalcValue::Text(output)
            }
            "TRIM" => {
                let output = trim_ascii_spaces(&input);
                self.text(output.len())?;
                CalcValue::Text(output)
            }
            "LEFT" | "RIGHT" => {
                let count = if call.arguments.len() == 1 {
                    1
                } else {
                    let value = self.scalar(&call.arguments[1])?;
                    if let Some(error) = value.as_error() {
                        return Ok(error_value(error));
                    }
                    match nonnegative_count(&value) {
                        Ok(value) => value,
                        Err(error) => return Ok(error_value(error)),
                    }
                };
                let output = if call.name == "LEFT" {
                    input.chars().take(count).collect::<String>()
                } else {
                    let length = input.chars().count();
                    input
                        .chars()
                        .skip(length.saturating_sub(count))
                        .collect::<String>()
                };
                self.text(output.len())?;
                CalcValue::Text(output)
            }
            "MID" => {
                let start = self.scalar(&call.arguments[1])?;
                if let Some(error) = start.as_error() {
                    return Ok(error_value(error));
                }
                let start = match positive_index(&start) {
                    Ok(value) => value,
                    Err(error) => return Ok(error_value(error)),
                };
                let count = self.scalar(&call.arguments[2])?;
                if let Some(error) = count.as_error() {
                    return Ok(error_value(error));
                }
                let count = match nonnegative_count(&count) {
                    Ok(value) => value,
                    Err(error) => return Ok(error_value(error)),
                };
                let output = input
                    .chars()
                    .skip(start - 1)
                    .take(count)
                    .collect::<String>();
                self.text(output.len())?;
                CalcValue::Text(output)
            }
            _ => unreachable!("text dispatcher validates the function name"),
        };
        Ok(RuntimeValue::Scalar(value))
    }

    fn function_concat(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if call.arguments.is_empty() {
            return Ok(error_value(CellError::Value));
        }
        let mut output = String::new();
        for argument in &call.arguments {
            let value = self.expression(argument)?;
            match value {
                RuntimeValue::Scalar(value) => {
                    if let Err(error) = self.append_text(&mut output, &finite_or_error(value)) {
                        return match error {
                            AppendTextError::Cell(error) => Ok(error_value(error)),
                            AppendTextError::Operational(error) => Err(error),
                        };
                    }
                }
                RuntimeValue::Range(range) => {
                    for value in range.values() {
                        self.range_cell()?;
                        if let Err(error) = self.append_text(&mut output, &finite_or_error(value.clone())) {
                            return match error {
                                AppendTextError::Cell(error) => Ok(error_value(error)),
                                AppendTextError::Operational(error) => Err(error),
                            };
                        }
                    }
                }
            }
        }
        Ok(RuntimeValue::Scalar(CalcValue::Text(output)))
    }

    fn append_text(
        &mut self,
        output: &mut String,
        value: &CalcValue,
    ) -> Result<(), AppendTextError> {
        let value = text_coercion(value).map_err(AppendTextError::Cell)?;
        self.text(value.len()).map_err(AppendTextError::Operational)?;
        output.push_str(&value);
        Ok(())
    }

    fn lookup_function(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        match call.name.as_str() {
            "INDEX" => self.function_index(call),
            "MATCH" => self.function_match(call),
            _ => unreachable!("lookup dispatcher validates the function name"),
        }
    }

    fn function_index(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if !matches!(call.arguments.len(), 2 | 3) {
            return Ok(error_value(CellError::Value));
        }
        let array = self.expression(&call.arguments[0])?;
        let range = match array {
            RuntimeValue::Range(range) => range,
            RuntimeValue::Scalar(CalcValue::Error(error)) => return Ok(error_value(error)),
            RuntimeValue::Scalar(_) => return Ok(error_value(CellError::Value)),
        };
        let row_or_position = self.scalar(&call.arguments[1])?;
        if let Some(error) = row_or_position.as_error() {
            return Ok(error_value(error));
        }
        let row_or_position = match positive_index(&row_or_position) {
            Ok(value) => value,
            Err(error) => return Ok(error_value(error)),
        };

        let (rows, columns, values) = (range.rows(), range.columns(), range.values());
        let position = if call.arguments.len() == 2 {
            if rows != 1 && columns != 1 {
                return Ok(error_value(CellError::Value));
            }
            row_or_position.checked_sub(1)
        } else {
            let column = self.scalar(&call.arguments[2])?;
            if let Some(error) = column.as_error() {
                return Ok(error_value(error));
            }
            let column = match positive_index(&column) {
                Ok(value) => value,
                Err(error) => return Ok(error_value(error)),
            };
            if row_or_position > rows || column > columns {
                return Ok(error_value(CellError::Reference));
            }
            (row_or_position - 1)
                .checked_mul(columns)
                .and_then(|offset| offset.checked_add(column - 1))
        };
        let Some(position) = position.filter(|position| *position < values.len()) else {
            return Ok(error_value(CellError::Reference));
        };
        self.range_cell()?;
        let value = finite_or_error(values[position].clone());
        if let CalcValue::Text(text) = &value {
            self.text(text.len())?;
        }
        Ok(RuntimeValue::Scalar(value))
    }

    fn function_match(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        if !matches!(call.arguments.len(), 2 | 3) {
            return Ok(error_value(CellError::Value));
        }
        let needle = self.scalar(&call.arguments[0])?;
        if let Some(error) = needle.as_error() {
            return Ok(error_value(error));
        }
        let array = self.expression(&call.arguments[1])?;
        let range = match array {
            RuntimeValue::Range(range) => range,
            RuntimeValue::Scalar(CalcValue::Error(error)) => return Ok(error_value(error)),
            RuntimeValue::Scalar(_) => return Ok(error_value(CellError::Value)),
        };
        if call.arguments.len() == 3 {
            let mode = self.scalar(&call.arguments[2])?;
            if let Some(error) = mode.as_error() {
                return Ok(error_value(error));
            }
            if let Err(error) = exact_mode(&mode) {
                return Ok(error_value(error));
            }
        }

        let (rows, columns, values) = (range.rows(), range.columns(), range.values());
        if rows != 1 && columns != 1 {
            return Ok(error_value(CellError::Value));
        }
        for (position, value) in values.iter().enumerate() {
            self.range_cell()?;
            if let CalcValue::Error(error) = value {
                return Ok(error_value(*error));
            }
            if matches!(value, CalcValue::Number(number) if !number.is_finite()) {
                return Ok(error_value(CellError::Number));
            }
            if equal_values(&needle, value) {
                return Ok(RuntimeValue::Scalar(count_result(position + 1)));
            }
        }
        Ok(error_value(CellError::NotAvailable))
    }

    fn date_function(&mut self, call: &FunctionCall) -> Result<RuntimeValue, EvaluationError> {
        let expected = if call.name == "DATE" { 3 } else { 1 };
        if call.arguments.len() != expected {
            return Ok(error_value(CellError::Value));
        }
        if call.name == "DATE" {
            let mut components = [0_i32; 3];
            for (target, argument) in components.iter_mut().zip(&call.arguments) {
                let value = self.scalar(argument)?;
                if let Some(error) = value.as_error() {
                    return Ok(error_value(error));
                }
                *target = match strict_integer(&value) {
                    Ok(value) => value,
                    Err(error) => return Ok(error_value(error)),
                };
            }
            let [year, month, day] = components;
            let date = u8::try_from(month)
                .ok()
                .and_then(|month| Month::try_from(month).ok())
                .and_then(|month| {
                    u8::try_from(day)
                        .ok()
                        .and_then(|day| Date::from_calendar_date(year, month, day).ok())
                });
            return Ok(RuntimeValue::Scalar(match date {
                Some(date) if (1..=9999).contains(&year) => CalcValue::Date(date),
                _ => CalcValue::Error(CellError::Number),
            }));
        }

        let input = self.scalar(&call.arguments[0])?;
        if let Some(error) = input.as_error() {
            return Ok(error_value(error));
        }
        let component = match (&input, call.name.as_str()) {
            (CalcValue::Date(value), "YEAR") => value.year(),
            (CalcValue::DateTime(value), "YEAR") => value.year(),
            (CalcValue::Date(value), "MONTH") => i32::from(u8::from(value.month())),
            (CalcValue::DateTime(value), "MONTH") => i32::from(u8::from(value.month())),
            (CalcValue::Date(value), "DAY") => i32::from(value.day()),
            (CalcValue::DateTime(value), "DAY") => i32::from(value.day()),
            _ => return Ok(error_value(CellError::Value)),
        };
        Ok(RuntimeValue::Scalar(CalcValue::Number(f64::from(component))))
    }

    fn inspection_function(
        &mut self,
        call: &FunctionCall,
    ) -> Result<RuntimeValue, EvaluationError> {
        if call.arguments.len() != 1 {
            return Ok(error_value(CellError::Value));
        }
        let value = self.expression(&call.arguments[0])?;
        let RuntimeValue::Scalar(value) = value else {
            return Ok(error_value(CellError::Value));
        };
        let result = match call.name.as_str() {
            "ISBLANK" => matches!(value, CalcValue::Blank),
            "ISNUMBER" => matches!(value, CalcValue::Number(number) if number.is_finite()),
            "ISTEXT" => matches!(value, CalcValue::Text(_)),
            "ISERROR" => matches!(value, CalcValue::Error(_))
                || matches!(value, CalcValue::Number(number) if !number.is_finite()),
            _ => unreachable!("inspection dispatcher validates the function name"),
        };
        Ok(RuntimeValue::Scalar(CalcValue::Boolean(result)))
    }
}

enum AppendTextError {
    Cell(CellError),
    Operational(EvaluationError),
}

fn error_value(error: CellError) -> RuntimeValue {
    RuntimeValue::Scalar(CalcValue::Error(error))
}

fn collect_aggregate_value(
    function: &str,
    value: &CalcValue,
    numbers: &mut Vec<f64>,
    counta: &mut usize,
) -> Option<CellError> {
    if function == "COUNTA" {
        if !matches!(value, CalcValue::Blank) {
            *counta = counta.saturating_add(1);
        }
        return None;
    }
    match value {
        CalcValue::Number(value) if value.is_finite() => numbers.push(*value),
        CalcValue::Number(_) => return Some(CellError::Number),
        CalcValue::Error(error) => return Some(*error),
        CalcValue::Blank
        | CalcValue::Text(_)
        | CalcValue::Boolean(_)
        | CalcValue::Date(_)
        | CalcValue::DateTime(_) => {}
    }
    None
}

#[allow(clippy::cast_precision_loss)]
fn count_result(count: usize) -> CalcValue {
    let value = count as f64;
    number_result(value)
}

fn sum_numbers(numbers: &[f64]) -> CalcValue {
    let mut sum = 0.0;
    for number in numbers {
        sum += number;
        if !sum.is_finite() {
            return CalcValue::Error(CellError::Number);
        }
    }
    CalcValue::Number(sum)
}

fn round_decimal(value: f64, digits: i32, mode: &str) -> CalcValue {
    let exponent = digits.unsigned_abs();
    let Ok(exponent) = i32::try_from(exponent) else {
        return CalcValue::Error(CellError::Number);
    };
    let factor = 10_f64.powi(exponent);
    if !factor.is_finite() {
        return CalcValue::Error(CellError::Number);
    }
    let scaled = if digits >= 0 {
        value * factor
    } else {
        value / factor
    };
    if !scaled.is_finite() {
        return CalcValue::Error(CellError::Number);
    }
    let rounded = match mode {
        "ROUND" => scaled.round(),
        "ROUNDUP" => scaled.abs().ceil().copysign(scaled),
        "ROUNDDOWN" => scaled.trunc(),
        _ => unreachable!("rounding dispatcher validates the function name"),
    };
    let result = if digits >= 0 {
        rounded / factor
    } else {
        rounded * factor
    };
    number_result(result)
}

#[allow(clippy::cast_precision_loss)]
fn nonnegative_count(value: &CalcValue) -> Result<usize, CellError> {
    match value {
        CalcValue::Number(number)
            if number.is_finite()
                && number.fract() == 0.0
                && *number >= 0.0
                && *number <= usize::MAX as f64 =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(*number as usize)
        }
        CalcValue::Number(_) => Err(CellError::Number),
        CalcValue::Error(error) => Err(*error),
        _ => Err(CellError::Value),
    }
}

fn trim_ascii_spaces(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut pending_space = false;
    for character in input.trim_matches(' ').chars() {
        if character == ' ' {
            pending_space = true;
        } else {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            output.push(character);
            pending_space = false;
        }
    }
    output
}
