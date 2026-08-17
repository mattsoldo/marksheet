use std::collections::HashMap;

use marksheet_model::{ByteSpan, NameId, SheetId, TableId};
use serde::{Deserialize, Serialize};

use super::ast::{
    BinaryOperator, Expr, ExprKind, Formula, FunctionCall, Literal, RangeReference, Reference,
    StructuredReference, TableRegion, UnaryOperator,
};
use super::lexer::{FormulaError, FormulaErrorKind, Token, TokenKind, lex_with_limits};

/// Resource limits applied before and during formula parsing.
///
/// These limits prevent adversarial source from turning a single cell into an
/// unbounded allocation or recursion request. Applications may choose tighter
/// values, but should surface the resulting [`FormulaError`] rather than
/// silently truncating a formula.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseLimits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_function_arguments: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 1_048_576,
            max_tokens: 100_000,
            max_depth: 256,
            max_nodes: 100_000,
            max_function_arguments: 10_000,
        }
    }
}

/// Parses a complete formula using the supplied limits.
///
/// # Errors
///
/// Returns [`FormulaError`] for malformed syntax or when a configured resource
/// limit is exceeded.
pub fn parse(source: &str, limits: &ParseLimits) -> Result<Formula, FormulaError> {
    let tokens = lex_with_limits(source, limits)?;
    let mut parser = Parser {
        tokens,
        cursor: 0,
        limits,
        nodes: 0,
        depths: HashMap::new(),
    };
    if matches!(parser.current().kind, TokenKind::End) {
        return Err(FormulaError::new(
            FormulaErrorKind::EmptyExpression,
            parser.current().span,
            "formula expression is empty",
        ));
    }
    let expression = parser.parse_comparison(1)?;
    if !matches!(parser.current().kind, TokenKind::End) {
        let token = parser.current();
        return Err(FormulaError::new(
            FormulaErrorKind::UnexpectedToken,
            token.span,
            format!("unexpected {} after expression", token.kind.description()),
        ));
    }
    Ok(Formula { expression })
}

struct Parser<'a> {
    tokens: Vec<Token>,
    cursor: usize,
    limits: &'a ParseLimits,
    nodes: usize,
    // Depth is recorded at construction time so long left-associative chains
    // remain O(n) to validate. Re-walking each growing left subtree would make
    // the limit check quadratic and would itself risk stack exhaustion.
    depths: HashMap<ByteSpan, usize>,
}

impl Parser<'_> {
    fn parse_comparison(&mut self, depth: usize) -> Result<Expr, FormulaError> {
        self.check_depth(depth)?;
        let mut left = self.parse_concatenation(depth)?;
        let Some(operator) = comparison_operator(&self.current().kind) else {
            return Ok(left);
        };
        self.advance();
        let right = self.parse_concatenation(depth)?;
        let span = joined(left.span, right.span);
        left = self.node(
            ExprKind::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            },
            span,
        )?;
        if comparison_operator(&self.current().kind).is_some() {
            return Err(FormulaError::new(
                FormulaErrorKind::ChainedComparison,
                self.current().span,
                "comparisons cannot be chained without parentheses",
            ));
        }
        Ok(left)
    }

    fn parse_concatenation(&mut self, depth: usize) -> Result<Expr, FormulaError> {
        let mut left = self.parse_additive(depth)?;
        while matches!(self.current().kind, TokenKind::Ampersand) {
            self.advance();
            let right = self.parse_additive(depth)?;
            let span = joined(left.span, right.span);
            left = self.node(
                ExprKind::Binary {
                    operator: BinaryOperator::Concatenate,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            )?;
        }
        Ok(left)
    }

    fn parse_additive(&mut self, depth: usize) -> Result<Expr, FormulaError> {
        let mut left = self.parse_multiplicative(depth)?;
        loop {
            let operator = match self.current().kind {
                TokenKind::Plus => BinaryOperator::Add,
                TokenKind::Minus => BinaryOperator::Subtract,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative(depth)?;
            let span = joined(left.span, right.span);
            left = self.node(
                ExprKind::Binary {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            )?;
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self, depth: usize) -> Result<Expr, FormulaError> {
        let mut left = self.parse_unary(depth)?;
        loop {
            let operator = match self.current().kind {
                TokenKind::Star => BinaryOperator::Multiply,
                TokenKind::Slash => BinaryOperator::Divide,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary(depth)?;
            let span = joined(left.span, right.span);
            left = self.node(
                ExprKind::Binary {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            )?;
        }
        Ok(left)
    }

    fn parse_unary(&mut self, depth: usize) -> Result<Expr, FormulaError> {
        let operator = match self.current().kind {
            TokenKind::Plus => Some(UnaryOperator::Positive),
            TokenKind::Minus => Some(UnaryOperator::Negative),
            _ => None,
        };
        if let Some(operator) = operator {
            self.check_depth(depth + 1)?;
            let start = self.current().span;
            self.advance();
            let operand = self.parse_unary(depth + 1)?;
            let span = joined(start, operand.span);
            return self.node(
                ExprKind::Unary {
                    operator,
                    operand: Box::new(operand),
                },
                span,
            );
        }
        self.parse_power(depth)
    }

    fn parse_power(&mut self, depth: usize) -> Result<Expr, FormulaError> {
        let left = self.parse_primary(depth)?;
        if !matches!(self.current().kind, TokenKind::Caret) {
            return Ok(left);
        }
        self.advance();
        // Parsing the RHS as unary produces both right associativity and the
        // spreadsheet convention that `2^-2` is valid while `-2^2` negates
        // the completed power expression.
        let right = self.parse_unary(depth + 1)?;
        let span = joined(left.span, right.span);
        self.node(
            ExprKind::Binary {
                operator: BinaryOperator::Power,
                left: Box::new(left),
                right: Box::new(right),
            },
            span,
        )
    }

    fn parse_primary(&mut self, depth: usize) -> Result<Expr, FormulaError> {
        self.check_depth(depth)?;
        let token = self.current().clone();
        match token.kind {
            TokenKind::Number(value) => {
                self.advance();
                self.node(
                    ExprKind::Literal {
                        value: Literal::Number(value),
                    },
                    token.span,
                )
            }
            TokenKind::Text(value) => {
                self.advance();
                self.node(
                    ExprKind::Literal {
                        value: Literal::Text(value),
                    },
                    token.span,
                )
            }
            TokenKind::Error(value) => {
                self.advance();
                self.node(
                    ExprKind::Literal {
                        value: Literal::Error(value),
                    },
                    token.span,
                )
            }
            TokenKind::Word(word) => self.parse_word(&word, token.span, depth),
            TokenKind::Cell(address) => {
                if matches!(self.peek().kind, TokenKind::LeftParen)
                    && !address.column_absolute
                    && !address.row_absolute
                {
                    let name = format!(
                        "{}{}",
                        address.coordinate.column_name(),
                        address.coordinate.row
                    );
                    self.parse_call(&name, token.span, depth)
                } else {
                    self.parse_cell_reference(address, token.span)
                }
            }
            TokenKind::Structured(selector) => {
                self.advance();
                let reference = parse_structured(None, selector, token.span)?;
                self.node(ExprKind::Reference { reference }, token.span)
            }
            TokenKind::LeftParen => {
                self.advance();
                let mut expression = self.parse_comparison(depth + 1)?;
                let closing = self.expect_right_paren()?;
                let expression_depth = self.expression_depth(&expression);
                expression.span = joined(token.span, closing.span);
                self.depths.insert(expression.span, expression_depth);
                Ok(expression)
            }
            TokenKind::End => Err(FormulaError::new(
                FormulaErrorKind::ExpectedExpression,
                token.span,
                "expected an expression before the end of the formula",
            )),
            _ => Err(FormulaError::new(
                FormulaErrorKind::ExpectedExpression,
                token.span,
                format!("expected an expression, found {}", token.kind.description()),
            )),
        }
    }

    fn parse_word(
        &mut self,
        word: &str,
        word_span: ByteSpan,
        depth: usize,
    ) -> Result<Expr, FormulaError> {
        if matches!(self.peek().kind, TokenKind::LeftParen) {
            return self.parse_call(word, word_span, depth);
        }
        // A following qualifier or selector makes this token an identifier,
        // even when its spelling is also a boolean keyword. Stable IDs such as
        // `true` and `false` are valid in the container grammar.
        if matches!(self.peek().kind, TokenKind::Bang) {
            return self.parse_qualified_reference(word, word_span);
        }
        if matches!(self.peek().kind, TokenKind::Structured(_)) {
            return self.parse_qualified_structured(word, word_span);
        }
        if word.eq_ignore_ascii_case("TRUE") || word.eq_ignore_ascii_case("FALSE") {
            self.advance();
            return self.node(
                ExprKind::Literal {
                    value: Literal::Boolean(word.eq_ignore_ascii_case("TRUE")),
                },
                word_span,
            );
        }
        self.advance();
        let name = NameId::parse(word).map_err(|_| {
            FormulaError::new(
                FormulaErrorKind::InvalidReference,
                word_span,
                format!("{word:?} is not a lowercase workbook name"),
            )
        })?;
        self.node(
            ExprKind::Reference {
                reference: Reference::Name { name },
            },
            word_span,
        )
    }

    fn parse_call(
        &mut self,
        name: &str,
        name_span: ByteSpan,
        depth: usize,
    ) -> Result<Expr, FormulaError> {
        self.advance();
        let opening = self.current().clone();
        debug_assert!(matches!(opening.kind, TokenKind::LeftParen));
        self.advance();
        let mut arguments = Vec::new();
        if !matches!(self.current().kind, TokenKind::RightParen) {
            loop {
                if arguments.len() >= self.limits.max_function_arguments {
                    return Err(FormulaError::new(
                        FormulaErrorKind::TooManyArguments,
                        self.current().span,
                        format!(
                            "function exceeds the {} argument limit",
                            self.limits.max_function_arguments
                        ),
                    ));
                }
                arguments.push(self.parse_comparison(depth + 1)?);
                if !matches!(self.current().kind, TokenKind::Comma) {
                    break;
                }
                self.advance();
                if matches!(self.current().kind, TokenKind::RightParen) {
                    return Err(FormulaError::new(
                        FormulaErrorKind::ExpectedExpression,
                        self.current().span,
                        "trailing commas are not permitted in function calls",
                    ));
                }
            }
        }
        let closing = self.expect_right_paren()?;
        self.node(
            ExprKind::Call {
                call: FunctionCall {
                    name: name.to_ascii_uppercase(),
                    arguments,
                },
            },
            joined(name_span, closing.span),
        )
    }

    fn parse_qualified_reference(
        &mut self,
        sheet: &str,
        sheet_span: ByteSpan,
    ) -> Result<Expr, FormulaError> {
        let sheet = SheetId::parse(sheet).map_err(|_| {
            FormulaError::new(
                FormulaErrorKind::InvalidReference,
                sheet_span,
                "sheet qualifiers must be lowercase identifiers",
            )
        })?;
        self.advance();
        let bang = self.current().clone();
        Self::require_adjacent(sheet_span, bang.span)?;
        self.advance();
        let cell = self.current().clone();
        let TokenKind::Cell(address) = cell.kind else {
            return Err(FormulaError::new(
                FormulaErrorKind::InvalidReference,
                cell.span,
                "a sheet qualifier must be followed by an A1 reference",
            ));
        };
        Self::require_adjacent(bang.span, cell.span)?;
        self.advance();
        self.finish_cell_or_range(Some(sheet), address, sheet_span, cell.span)
    }

    fn parse_cell_reference(
        &mut self,
        address: super::ast::A1Reference,
        address_span: ByteSpan,
    ) -> Result<Expr, FormulaError> {
        self.advance();
        self.finish_cell_or_range(None, address, address_span, address_span)
    }

    fn finish_cell_or_range(
        &mut self,
        sheet: Option<SheetId>,
        start: super::ast::A1Reference,
        full_start_span: ByteSpan,
        cell_span: ByteSpan,
    ) -> Result<Expr, FormulaError> {
        if !matches!(self.current().kind, TokenKind::Colon) {
            return self.node(
                ExprKind::Reference {
                    reference: Reference::Cell {
                        sheet,
                        address: start,
                    },
                },
                joined(full_start_span, cell_span),
            );
        }
        let colon = self.current().clone();
        Self::require_adjacent(cell_span, colon.span)?;
        self.advance();
        let endpoint = self.current().clone();
        let TokenKind::Cell(end) = endpoint.kind else {
            return Err(FormulaError::new(
                FormulaErrorKind::InvalidReference,
                endpoint.span,
                "range endpoints must be unqualified A1 references",
            ));
        };
        Self::require_adjacent(colon.span, endpoint.span)?;
        self.advance();
        self.node(
            ExprKind::Reference {
                reference: Reference::Range(RangeReference { sheet, start, end }),
            },
            joined(full_start_span, endpoint.span),
        )
    }

    fn parse_qualified_structured(
        &mut self,
        table: &str,
        table_span: ByteSpan,
    ) -> Result<Expr, FormulaError> {
        let table = TableId::parse(table).map_err(|_| {
            FormulaError::new(
                FormulaErrorKind::InvalidStructuredReference,
                table_span,
                "table qualifiers must be lowercase identifiers",
            )
        })?;
        self.advance();
        let selector = self.current().clone();
        Self::require_adjacent(table_span, selector.span)?;
        let TokenKind::Structured(value) = selector.kind else {
            unreachable!("caller verified structured selector")
        };
        self.advance();
        let reference = parse_structured(Some(table), value, selector.span)?;
        self.node(
            ExprKind::Reference { reference },
            joined(table_span, selector.span),
        )
    }

    fn expect_right_paren(&mut self) -> Result<Token, FormulaError> {
        let token = self.current().clone();
        if !matches!(token.kind, TokenKind::RightParen) {
            return Err(FormulaError::new(
                FormulaErrorKind::ExpectedToken,
                token.span,
                format!("expected ')', found {}", token.kind.description()),
            ));
        }
        self.advance();
        Ok(token)
    }

    fn require_adjacent(left: ByteSpan, right: ByteSpan) -> Result<(), FormulaError> {
        if left.end == right.start {
            Ok(())
        } else {
            Err(FormulaError::new(
                FormulaErrorKind::WhitespaceInReference,
                ByteSpan {
                    start: left.end,
                    end: right.start,
                },
                "whitespace is not permitted inside a reference",
            ))
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), FormulaError> {
        if depth <= self.limits.max_depth {
            return Ok(());
        }
        Err(FormulaError::new(
            FormulaErrorKind::TooDeep,
            self.current().span,
            format!(
                "formula exceeds the {} level depth limit",
                self.limits.max_depth
            ),
        ))
    }

    /// Builds one AST node, enforcing the parser's depth and node-count
    /// limits before it is returned to the caller.
    ///
    /// Depth is purely structural: a unary or binary operator's depth is one
    /// more than its deepest child, and a call's depth is one more than its
    /// deepest argument. This means a left-spine chain of the same operator —
    /// for example `1+2+3+...+257`, which never branches — still nests one
    /// level per additional term, exactly like a genuinely nested expression
    /// of the same depth. A flat chain of more than `max_depth` (256 by
    /// default) terms is therefore rejected as `MS2202`, just as deliberately
    /// nested input would be.
    fn node(&mut self, kind: ExprKind, span: ByteSpan) -> Result<Expr, FormulaError> {
        let depth = match &kind {
            ExprKind::Literal { .. } | ExprKind::Reference { .. } => 1,
            ExprKind::Unary { operand, .. } => self.expression_depth(operand).saturating_add(1),
            ExprKind::Binary { left, right, .. } => self
                .expression_depth(left)
                .max(self.expression_depth(right))
                .saturating_add(1),
            ExprKind::Call { call } => call
                .arguments
                .iter()
                .map(|argument| self.expression_depth(argument))
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        };
        if depth > self.limits.max_depth {
            // At the point of rejection all children are already within the
            // limit. Discard the candidate iteratively so an unusually large
            // caller-supplied limit cannot turn error cleanup into recursion.
            drop_kind_iteratively(kind);
            return Err(FormulaError::new(
                FormulaErrorKind::TooDeep,
                span,
                format!(
                    "formula AST exceeds the {} level depth limit",
                    self.limits.max_depth
                ),
            ));
        }
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes {
            drop_kind_iteratively(kind);
            return Err(FormulaError::new(
                FormulaErrorKind::TooManyNodes,
                span,
                format!(
                    "formula exceeds the {} AST node limit",
                    self.limits.max_nodes
                ),
            ));
        }
        self.depths.insert(span, depth);
        Ok(Expr::new(kind, span))
    }

    fn expression_depth(&self, expression: &Expr) -> usize {
        self.depths
            .get(&expression.span)
            .copied()
            .expect("parser-created expressions have recorded depths")
    }

    fn current(&self) -> &Token {
        &self.tokens[self.cursor]
    }

    fn peek(&self) -> &Token {
        self.tokens
            .get(self.cursor + 1)
            .unwrap_or_else(|| self.tokens.last().expect("lexer emits EOF"))
    }

    fn advance(&mut self) {
        if self.cursor + 1 < self.tokens.len() {
            self.cursor += 1;
        }
    }
}

/// Consumes an unaccepted AST candidate without recursively dropping its
/// boxed children. Accepted formulas remain shallow by construction.
fn drop_kind_iteratively(kind: ExprKind) {
    let mut pending = vec![kind];
    while let Some(kind) = pending.pop() {
        match kind {
            ExprKind::Literal { .. } | ExprKind::Reference { .. } => {}
            ExprKind::Unary { operand, .. } => {
                let Expr { kind, span: _ } = *operand;
                pending.push(kind);
            }
            ExprKind::Binary { left, right, .. } => {
                let Expr {
                    kind: left_kind,
                    span: _,
                } = *left;
                let Expr {
                    kind: right_kind,
                    span: _,
                } = *right;
                pending.push(left_kind);
                pending.push(right_kind);
            }
            ExprKind::Call { call } => {
                pending.extend(
                    call.arguments
                        .into_iter()
                        .map(|Expr { kind, span: _ }| kind),
                );
            }
        }
    }
}

fn parse_structured(
    table: Option<TableId>,
    selector: String,
    span: ByteSpan,
) -> Result<Reference, FormulaError> {
    if let Some(header) = selector.strip_prefix('@') {
        if header.is_empty() {
            return Err(FormulaError::new(
                FormulaErrorKind::InvalidStructuredReference,
                span,
                "a current-row selector requires a header",
            ));
        }
        return Ok(Reference::Structured(StructuredReference::CurrentRow {
            table,
            header: header.to_owned(),
        }));
    }
    let Some(table) = table else {
        return Err(FormulaError::new(
            FormulaErrorKind::InvalidStructuredReference,
            span,
            "only current-row selectors may omit a table identifier",
        ));
    };
    let structured = match selector.as_str() {
        "#Headers" => StructuredReference::Region {
            table,
            region: TableRegion::Headers,
        },
        "#Data" => StructuredReference::Region {
            table,
            region: TableRegion::Data,
        },
        _ => StructuredReference::Column {
            table,
            header: selector,
        },
    };
    Ok(Reference::Structured(structured))
}

fn comparison_operator(kind: &TokenKind) -> Option<BinaryOperator> {
    Some(match kind {
        TokenKind::Equal => BinaryOperator::Equal,
        TokenKind::NotEqual => BinaryOperator::NotEqual,
        TokenKind::Less => BinaryOperator::Less,
        TokenKind::LessEqual => BinaryOperator::LessEqual,
        TokenKind::Greater => BinaryOperator::Greater,
        TokenKind::GreaterEqual => BinaryOperator::GreaterEqual,
        _ => return None,
    })
}

const fn joined(first: ByteSpan, second: ByteSpan) -> ByteSpan {
    ByteSpan {
        start: first.start,
        end: second.end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(source: &str) -> Formula {
        parse(source, &ParseLimits::default()).expect("valid formula")
    }

    #[test]
    fn precedence_matches_portable_profile() {
        let formula = parsed("=-2^2+3*4");
        let ExprKind::Binary { operator, left, .. } = formula.expression.kind else {
            panic!("expected addition");
        };
        assert_eq!(operator, BinaryOperator::Add);
        let ExprKind::Unary { operand, .. } = left.kind else {
            panic!("expected unary negation");
        };
        assert!(matches!(
            operand.kind,
            ExprKind::Binary {
                operator: BinaryOperator::Power,
                ..
            }
        ));
    }

    #[test]
    fn accepts_ascii_whitespace_between_formula_tokens() {
        parse("=sum ( a1 , $b$2 )", &ParseLimits::default())
            .expect("ASCII whitespace is permitted between formula tokens");
    }

    #[test]
    fn parses_all_reference_families() {
        for formula in [
            "=$A1",
            "=inputs!A1:B2",
            "=tax_rate",
            "=costs[Subtotal]",
            "=costs[#Headers]",
            "=costs[@Cost]",
            "=[@Cost]",
            "=costs[Unit Cost]",
            "=[@Unit Cost]",
        ] {
            parsed(formula);
        }
    }

    #[test]
    fn boolean_keywords_are_contextual_before_reference_qualifiers() {
        let formula = parsed("=true!A1+false[Cost]");
        let ExprKind::Binary { left, right, .. } = formula.expression.kind else {
            panic!("expected binary expression");
        };
        assert!(matches!(
            left.kind,
            ExprKind::Reference {
                reference: Reference::Cell { sheet: Some(ref sheet), .. }
            } if sheet.as_str() == "true"
        ));
        assert!(matches!(
            right.kind,
            ExprKind::Reference {
                reference: Reference::Structured(StructuredReference::Column {
                    ref table,
                    ref header,
                })
            } if table.as_str() == "false" && header == "Cost"
        ));

        assert!(matches!(
            parsed("=TrUe").expression.kind,
            ExprKind::Literal {
                value: Literal::Boolean(true)
            }
        ));
    }

    #[test]
    fn a1_shaped_identifiers_are_contextual_before_reference_qualifiers() {
        let formula = parsed("=a1!B2+a1[Cost]");
        let ExprKind::Binary { left, right, .. } = formula.expression.kind else {
            panic!("expected binary expression");
        };
        assert!(matches!(
            left.kind,
            ExprKind::Reference {
                reference: Reference::Cell { sheet: Some(ref sheet), .. }
            } if sheet.as_str() == "a1"
        ));
        assert!(matches!(
            right.kind,
            ExprKind::Reference {
                reference: Reference::Structured(StructuredReference::Column {
                    ref table,
                    ref header,
                })
            } if table.as_str() == "a1" && header == "Cost"
        ));

        assert!(parse("=A1!B2", &ParseLimits::default()).is_err());
        assert!(matches!(
            parsed("=a1").expression.kind,
            ExprKind::Reference {
                reference: Reference::Cell { .. }
            }
        ));
    }

    #[test]
    fn rejects_chained_comparison() {
        let error = parse("=1<2<3", &ParseLimits::default()).expect_err("invalid chain");
        assert_eq!(error.kind, FormulaErrorKind::ChainedComparison);
    }

    #[test]
    fn rejects_malformed_corpus_examples() {
        for formula in [
            "=",
            "=1+",
            "=(1+2",
            "=1 2",
            "=\"unterminated",
            "=\"line\nbreak\"",
            "=A1,B1",
            "=A 1",
            "=inputs!A1:other!B2",
            "=SUM(1,)",
            "=SUM(,1)",
            "=10%",
            "=A0",
            "=1e",
            "=A1:B2:C3",
            "=1\u{a0}+2",
        ] {
            assert!(
                parse(formula, &ParseLimits::default()).is_err(),
                "{formula:?}"
            );
        }
    }

    #[test]
    fn rejects_whitespace_inside_qualified_reference() {
        let error = parse("=inputs !A1", &ParseLimits::default()).expect_err("invalid whitespace");
        assert_eq!(error.kind, FormulaErrorKind::WhitespaceInReference);
    }

    #[test]
    fn enforces_depth_limit() {
        let limits = ParseLimits {
            max_depth: 2,
            ..ParseLimits::default()
        };
        let error = parse("=(((1)))", &limits).expect_err("too deeply nested");
        assert_eq!(error.kind, FormulaErrorKind::TooDeep);
    }

    #[test]
    fn depth_limit_tracks_the_resulting_left_associative_ast() {
        let limits = ParseLimits {
            max_depth: 4,
            ..ParseLimits::default()
        };
        parse("=1+2+3+4", &limits).expect("AST depth exactly at limit");
        let error = parse("=1+2+3+4+5", &limits).expect_err("AST exceeds depth limit");
        assert_eq!(error.kind, FormulaErrorKind::TooDeep);
        assert_eq!(error.diagnostic_code(), "MS2202");
    }

    #[test]
    fn rejecting_a_huge_left_chain_does_not_overflow_the_stack() {
        let mut source = String::from("=1");
        for _ in 0..40_000 {
            source.push_str("+1");
        }
        let error = parse(&source, &ParseLimits::default()).expect_err("AST is too deep");
        assert_eq!(error.kind, FormulaErrorKind::TooDeep);
    }
}
