use std::fmt;

use marksheet_model::{ByteSpan, CellError, Coordinate};
use serde::{Deserialize, Serialize};

use super::FORMULA_SYNTAX_DIAGNOSTIC;
use super::ast::A1Reference;
use super::parser::ParseLimits;

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: ByteSpan,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Number(f64),
    Text(String),
    Error(CellError),
    Word(String),
    Cell(A1Reference),
    Structured(String),
    LeftParen,
    RightParen,
    Comma,
    Bang,
    Colon,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Ampersand,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    End,
}

impl TokenKind {
    #[must_use]
    pub fn description(&self) -> &'static str {
        match self {
            Self::Number(_) => "number",
            Self::Text(_) => "text literal",
            Self::Error(_) => "error literal",
            Self::Word(_) => "name",
            Self::Cell(_) => "cell reference",
            Self::Structured(_) => "structured selector",
            Self::LeftParen => "'('",
            Self::RightParen => "')'",
            Self::Comma => "','",
            Self::Bang => "'!'",
            Self::Colon => "':'",
            Self::Plus => "'+'",
            Self::Minus => "'-'",
            Self::Star => "'*'",
            Self::Slash => "'/'",
            Self::Caret => "'^'",
            Self::Ampersand => "'&'",
            Self::Equal => "'='",
            Self::NotEqual => "'<>'",
            Self::Less => "'<'",
            Self::LessEqual => "'<='",
            Self::Greater => "'>'",
            Self::GreaterEqual => "'>='",
            Self::End => "end of formula",
        }
    }
}

/// Machine-stable categories for all formula syntax failures.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormulaErrorKind {
    MissingEquals,
    EmptyExpression,
    SourceTooLong,
    TooManyTokens,
    TooDeep,
    TooManyNodes,
    TooManyArguments,
    InvalidNumber,
    NonFiniteNumber,
    UnterminatedText,
    NewlineInText,
    UnterminatedStructuredReference,
    InvalidStructuredReference,
    InvalidReference,
    WhitespaceInReference,
    ChainedComparison,
    ExpectedExpression,
    ExpectedToken,
    UnexpectedToken,
    UnexpectedCharacter,
}

/// A parse failure whose code and category are stable. `message` is intended
/// for humans and may be made clearer without changing conformance behavior.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FormulaError {
    pub kind: FormulaErrorKind,
    pub span: ByteSpan,
    pub message: String,
}

impl FormulaError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        FORMULA_SYNTAX_DIAGNOSTIC
    }

    pub(crate) fn new(kind: FormulaErrorKind, span: ByteSpan, message: impl Into<String>) -> Self {
        Self {
            kind,
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for FormulaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at bytes {}..{}: {}",
            self.diagnostic_code(),
            self.span.start,
            self.span.end,
            self.message
        )
    }
}

impl std::error::Error for FormulaError {}

/// Tokenizes a complete formula using default resource limits.
///
/// # Errors
///
/// Returns [`FormulaError`] for malformed syntax or when a default resource
/// limit is exceeded.
pub fn lex(source: &str) -> Result<Vec<Token>, FormulaError> {
    lex_with_limits(source, &ParseLimits::default())
}

pub(crate) fn lex_with_limits(
    source: &str,
    limits: &ParseLimits,
) -> Result<Vec<Token>, FormulaError> {
    if source.len() > limits.max_source_bytes {
        return Err(FormulaError::new(
            FormulaErrorKind::SourceTooLong,
            span(0, source.len()),
            format!(
                "formula is {} bytes; the configured limit is {}",
                source.len(),
                limits.max_source_bytes
            ),
        ));
    }
    if !source.starts_with('=') {
        return Err(FormulaError::new(
            FormulaErrorKind::MissingEquals,
            ByteSpan::empty(0),
            "formula must begin with '='",
        ));
    }

    let bytes = source.as_bytes();
    let mut cursor = 1;
    let mut tokens = Vec::new();
    while cursor < bytes.len() {
        if is_formula_space(bytes[cursor]) {
            cursor += 1;
            continue;
        }

        let start = cursor;
        let kind = match bytes[cursor] {
            b'0'..=b'9' => scan_number(source, &mut cursor)?,
            b'"' => scan_text(source, &mut cursor)?,
            b'#' => scan_error(source, &mut cursor)?,
            b'[' => scan_structured(source, &mut cursor)?,
            b'$' => scan_absolute_cell(source, &mut cursor)?,
            byte if byte.is_ascii_alphabetic() => scan_word_or_cell(source, &mut cursor)?,
            b'(' => single(&mut cursor, TokenKind::LeftParen),
            b')' => single(&mut cursor, TokenKind::RightParen),
            b',' => single(&mut cursor, TokenKind::Comma),
            b'!' => single(&mut cursor, TokenKind::Bang),
            b':' => single(&mut cursor, TokenKind::Colon),
            b'+' => single(&mut cursor, TokenKind::Plus),
            b'-' => single(&mut cursor, TokenKind::Minus),
            b'*' => single(&mut cursor, TokenKind::Star),
            b'/' => single(&mut cursor, TokenKind::Slash),
            b'^' => single(&mut cursor, TokenKind::Caret),
            b'&' => single(&mut cursor, TokenKind::Ampersand),
            b'=' => single(&mut cursor, TokenKind::Equal),
            b'<' if bytes.get(cursor + 1) == Some(&b'>') => {
                cursor += 2;
                TokenKind::NotEqual
            }
            b'<' if bytes.get(cursor + 1) == Some(&b'=') => {
                cursor += 2;
                TokenKind::LessEqual
            }
            b'>' if bytes.get(cursor + 1) == Some(&b'=') => {
                cursor += 2;
                TokenKind::GreaterEqual
            }
            b'<' => single(&mut cursor, TokenKind::Less),
            b'>' => single(&mut cursor, TokenKind::Greater),
            _ => {
                let character = source[cursor..]
                    .chars()
                    .next()
                    .expect("cursor is in bounds");
                cursor += character.len_utf8();
                return Err(FormulaError::new(
                    FormulaErrorKind::UnexpectedCharacter,
                    span(start, cursor),
                    format!("character {character:?} is not valid formula syntax"),
                ));
            }
        };
        tokens.push(Token {
            kind,
            span: span(start, cursor),
        });
        if tokens.len() > limits.max_tokens {
            return Err(FormulaError::new(
                FormulaErrorKind::TooManyTokens,
                span(start, cursor),
                format!("formula exceeds the {} token limit", limits.max_tokens),
            ));
        }
    }
    tokens.push(Token {
        kind: TokenKind::End,
        span: ByteSpan::empty(to_u64(source.len())),
    });
    Ok(tokens)
}

const fn is_formula_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn single(cursor: &mut usize, kind: TokenKind) -> TokenKind {
    *cursor += 1;
    kind
}

fn scan_number(source: &str, cursor: &mut usize) -> Result<TokenKind, FormulaError> {
    let bytes = source.as_bytes();
    let start = *cursor;
    if bytes[*cursor] == b'0' {
        *cursor += 1;
        if bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
            while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
                *cursor += 1;
            }
            return Err(invalid_number(source, start, *cursor));
        }
    } else {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
            *cursor += 1;
        }
    }
    if bytes.get(*cursor) == Some(&b'.') {
        *cursor += 1;
        let fraction_start = *cursor;
        while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
            *cursor += 1;
        }
        if *cursor == fraction_start {
            return Err(invalid_number(source, start, *cursor));
        }
    }
    if matches!(bytes.get(*cursor), Some(b'e' | b'E')) {
        *cursor += 1;
        if matches!(bytes.get(*cursor), Some(b'+' | b'-')) {
            *cursor += 1;
        }
        let exponent_start = *cursor;
        while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
            *cursor += 1;
        }
        if *cursor == exponent_start {
            return Err(invalid_number(source, start, *cursor));
        }
    }
    let spelling = &source[start..*cursor];
    let value = spelling
        .parse::<f64>()
        .map_err(|_| invalid_number(source, start, *cursor))?;
    if !value.is_finite() {
        return Err(FormulaError::new(
            FormulaErrorKind::NonFiniteNumber,
            span(start, *cursor),
            format!("number {spelling:?} is outside the finite binary64 range"),
        ));
    }
    Ok(TokenKind::Number(value))
}

fn invalid_number(source: &str, start: usize, end: usize) -> FormulaError {
    FormulaError::new(
        FormulaErrorKind::InvalidNumber,
        span(start, end),
        format!("invalid number literal {:?}", &source[start..end]),
    )
}

fn scan_text(source: &str, cursor: &mut usize) -> Result<TokenKind, FormulaError> {
    let start = *cursor;
    *cursor += 1;
    let mut value = String::new();
    while *cursor < source.len() {
        let rest = &source[*cursor..];
        let character = rest.chars().next().expect("cursor is in bounds");
        if character == '"' {
            if source.as_bytes().get(*cursor + 1) == Some(&b'"') {
                value.push('"');
                *cursor += 2;
                continue;
            }
            *cursor += 1;
            return Ok(TokenKind::Text(value));
        }
        if matches!(character, '\r' | '\n') {
            let end = *cursor + character.len_utf8();
            return Err(FormulaError::new(
                FormulaErrorKind::NewlineInText,
                span(*cursor, end),
                "formula text cannot contain a bare newline",
            ));
        }
        value.push(character);
        *cursor += character.len_utf8();
    }
    Err(FormulaError::new(
        FormulaErrorKind::UnterminatedText,
        span(start, source.len()),
        "unterminated formula text literal",
    ))
}

fn scan_error(source: &str, cursor: &mut usize) -> Result<TokenKind, FormulaError> {
    const ERRORS: [CellError; 7] = [
        CellError::DivisionByZero,
        CellError::NotAvailable,
        CellError::Name,
        CellError::Number,
        CellError::Reference,
        CellError::Value,
        CellError::Circular,
    ];
    let start = *cursor;
    for error in ERRORS {
        if source[start..].starts_with(error.token()) {
            *cursor += error.token().len();
            return Ok(TokenKind::Error(error));
        }
    }
    let character = source[start..].chars().next().expect("cursor is in bounds");
    *cursor += character.len_utf8();
    Err(FormulaError::new(
        FormulaErrorKind::UnexpectedCharacter,
        span(start, *cursor),
        "unknown error literal",
    ))
}

fn scan_structured(source: &str, cursor: &mut usize) -> Result<TokenKind, FormulaError> {
    let start = *cursor;
    *cursor += 1;
    let mut selector = String::new();
    while *cursor < source.len() {
        let character = source[*cursor..]
            .chars()
            .next()
            .expect("cursor is in bounds");
        if character == ']' {
            if source.as_bytes().get(*cursor + 1) == Some(&b']') {
                selector.push(']');
                *cursor += 2;
                continue;
            }
            *cursor += 1;
            if selector.is_empty() {
                return Err(FormulaError::new(
                    FormulaErrorKind::InvalidStructuredReference,
                    span(start, *cursor),
                    "structured selectors must be nonempty",
                ));
            }
            return Ok(TokenKind::Structured(selector));
        }
        selector.push(character);
        *cursor += character.len_utf8();
    }
    Err(FormulaError::new(
        FormulaErrorKind::UnterminatedStructuredReference,
        span(start, source.len()),
        "unterminated structured reference",
    ))
}

fn scan_absolute_cell(source: &str, cursor: &mut usize) -> Result<TokenKind, FormulaError> {
    let start = *cursor;
    *cursor += 1;
    let bytes = source.as_bytes();
    let column_start = *cursor;
    while bytes.get(*cursor).is_some_and(u8::is_ascii_alphabetic) {
        *cursor += 1;
    }
    if *cursor == column_start {
        return Err(invalid_reference(start, *cursor));
    }
    if bytes.get(*cursor) == Some(&b'$') {
        *cursor += 1;
    }
    let row_start = *cursor;
    while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
        *cursor += 1;
    }
    if *cursor == row_start {
        return Err(invalid_reference(start, *cursor));
    }
    parse_a1(&source[start..*cursor])
        .map(TokenKind::Cell)
        .map_err(|_| invalid_reference(start, *cursor))
}

fn scan_word_or_cell(source: &str, cursor: &mut usize) -> Result<TokenKind, FormulaError> {
    let start = *cursor;
    let bytes = source.as_bytes();
    while bytes.get(*cursor).is_some_and(u8::is_ascii_alphabetic) {
        *cursor += 1;
    }
    // A row-absolute marker can only occur after a column name, so it
    // unambiguously selects the mixed A1 form (for example `C$7`).
    if bytes.get(*cursor) == Some(&b'$') {
        *cursor += 1;
        let row_start = *cursor;
        while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
            *cursor += 1;
        }
        if *cursor == row_start {
            return Err(invalid_reference(start, *cursor));
        }
        return parse_a1(&source[start..*cursor])
            .map(TokenKind::Cell)
            .map_err(|_| invalid_reference(start, *cursor));
    }
    while bytes
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        *cursor += 1;
    }
    let word = &source[start..*cursor];
    // Sheet and table identifiers may resemble A1 coordinates. Qualifiers are
    // adjacency-sensitive, so resolve that ambiguity here while the original
    // casing remains available: lowercase `a1!`/`a1[` starts an identifier,
    // whereas uppercase `A1!` remains an invalid attempted qualifier.
    if matches!(bytes.get(*cursor), Some(b'!' | b'['))
        && word
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && word
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Ok(TokenKind::Word(word.to_owned()));
    }
    if looks_like_a1(word) {
        return parse_a1(word)
            .map(TokenKind::Cell)
            .map_err(|_| invalid_reference(start, *cursor));
    }
    Ok(TokenKind::Word(word.to_owned()))
}

fn looks_like_a1(value: &str) -> bool {
    let bytes = value.as_bytes();
    let split = bytes.iter().position(u8::is_ascii_digit);
    split.is_some_and(|index| {
        index > 0
            && bytes[..index].iter().all(u8::is_ascii_alphabetic)
            && bytes[index..].iter().all(u8::is_ascii_digit)
    })
}

fn parse_a1(value: &str) -> Result<A1Reference, FormulaError> {
    let mut spelling = value;
    let column_absolute = spelling.starts_with('$');
    if column_absolute {
        spelling = &spelling[1..];
    }
    let split = spelling
        .bytes()
        .position(|byte| byte == b'$' || byte.is_ascii_digit())
        .ok_or_else(|| invalid_reference(0, value.len()))?;
    let (column, row) = spelling.split_at(split);
    let row_absolute = row.starts_with('$');
    let row = row.strip_prefix('$').unwrap_or(row);
    let canonical = format!("{}{}", column.to_ascii_uppercase(), row);
    let coordinate =
        Coordinate::parse(&canonical).map_err(|_| invalid_reference(0, value.len()))?;
    Ok(A1Reference {
        coordinate,
        column_absolute,
        row_absolute,
    })
}

fn invalid_reference(start: usize, end: usize) -> FormulaError {
    FormulaError::new(
        FormulaErrorKind::InvalidReference,
        span(start, end),
        "invalid A1 reference",
    )
}

fn span(start: usize, end: usize) -> ByteSpan {
    ByteSpan {
        start: to_u64(start),
        end: to_u64(end),
    }
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_decodes_literals_and_references() {
        let tokens = lex("=\"a\"\"b\"+$a$1+#N/A").expect("valid formula");
        assert!(matches!(&tokens[0].kind, TokenKind::Text(value) if value == "a\"b"));
        assert!(matches!(tokens[1].kind, TokenKind::Plus));
        assert!(matches!(tokens[2].kind, TokenKind::Cell(_)));
        assert!(matches!(
            tokens[4].kind,
            TokenKind::Error(CellError::NotAvailable)
        ));
    }

    #[test]
    fn lexer_rejects_non_ascii_formula_whitespace() {
        let error = lex("=1\u{a0}+2").expect_err("NBSP is not formula whitespace");
        assert_eq!(error.kind, FormulaErrorKind::UnexpectedCharacter);
    }

    #[test]
    fn lexer_retains_escaped_structured_bracket() {
        let tokens = lex("=fees[Fee]]]").expect("valid formula");
        assert!(matches!(&tokens[1].kind, TokenKind::Structured(value) if value == "Fee]"));
    }
}
