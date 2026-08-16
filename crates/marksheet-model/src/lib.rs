//! Stable, source-aware semantic data types for Marksheet workbooks.
//!
//! This crate deliberately has no parser or calculator dependency.  The syntax
//! crate constructs these types, while editors and calculators can use the same
//! sparse representation without needing to retain a dense grid.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, format_description::well_known::Rfc3339};

/// A half-open byte interval in UTF-8 source text: `[start, end)`.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub struct ByteSpan {
    pub start: u64,
    pub end: u64,
}

impl ByteSpan {
    /// Makes a span when `start <= end`.
    ///
    /// # Errors
    ///
    /// Returns [`SpanError::Reversed`] when the end precedes the start.
    pub fn try_new(start: u64, end: u64) -> Result<Self, SpanError> {
        if start > end {
            return Err(SpanError::Reversed { start, end });
        }
        Ok(Self { start, end })
    }

    /// Makes an empty span at a byte offset.
    #[must_use]
    pub const fn empty(offset: u64) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    #[must_use]
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub const fn contains_offset(self, offset: u64) -> bool {
        self.start <= offset && offset < self.end
    }

    #[must_use]
    pub const fn contains_span(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Returns true when two spans share at least one byte. Adjacent spans do
    /// not overlap.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpanError {
    Reversed { start: u64, end: u64 },
}

impl fmt::Display for SpanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reversed { start, end } => {
                write!(f, "span starts at {start} after it ends at {end}")
            }
        }
    }
}
impl std::error::Error for SpanError {}

/// A one-based source position. Columns count Unicode scalar values, not UTF-8 bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct LineColumn {
    pub line: u64,
    pub column: u64,
}

/// Maps valid UTF-8 byte offsets to displayable line and scalar-column positions.
#[derive(Clone, Debug)]
pub struct LineIndex {
    source: String,
    line_starts: Vec<usize>,
}

impl LineIndex {
    #[must_use]
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (index, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(index + 1);
            }
        }
        Self {
            source: source.to_owned(),
            line_starts,
        }
    }

    #[must_use]
    pub fn source_len(&self) -> u64 {
        u64::try_from(self.source.len()).unwrap_or(u64::MAX)
    }

    /// Converts a byte offset at a Unicode scalar boundary. The EOF offset is valid.
    ///
    /// # Errors
    ///
    /// Returns [`PositionError::OutOfBounds`] outside the source or
    /// [`PositionError::NotScalarBoundary`] inside a UTF-8 scalar.
    pub fn line_column(&self, offset: u64) -> Result<LineColumn, PositionError> {
        let offset = usize::try_from(offset).map_err(|_| PositionError::OutOfBounds {
            offset,
            source_len: self.source_len(),
        })?;
        if offset > self.source.len() {
            return Err(PositionError::OutOfBounds {
                offset: u64::try_from(offset).unwrap_or(u64::MAX),
                source_len: self.source_len(),
            });
        }
        if !self.source.is_char_boundary(offset) {
            return Err(PositionError::NotScalarBoundary {
                offset: u64::try_from(offset).unwrap_or(u64::MAX),
            });
        }
        let line_index = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let line_start = self.line_starts[line_index];
        let column = self.source[line_start..offset].chars().count() as u64 + 1;
        Ok(LineColumn {
            line: u64::try_from(line_index)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
            column,
        })
    }

    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::line_column`] when either endpoint
    /// is not a valid source position.
    pub fn span_positions(
        &self,
        span: ByteSpan,
    ) -> Result<(LineColumn, LineColumn), PositionError> {
        self.line_column(span.start)
            .and_then(|start| self.line_column(span.end).map(|end| (start, end)))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PositionError {
    OutOfBounds { offset: u64, source_len: u64 },
    NotScalarBoundary { offset: u64 },
}
impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { offset, source_len } => write!(
                f,
                "byte offset {offset} is outside source length {source_len}"
            ),
            Self::NotScalarBoundary { offset } => {
                write!(f, "byte offset {offset} is inside a UTF-8 scalar")
            }
        }
    }
}
impl std::error::Error for PositionError {}

/// A source origin for an IR node. More granular token origins remain owned by the syntax tree.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub span: ByteSpan,
}

macro_rules! typed_identifier {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Identifier);
        impl $name {
            /// # Errors
            ///
            /// Returns [`IdentifierError`] when `value` is not a canonical
            /// Marksheet identifier.
            pub fn parse(value: &str) -> Result<Self, IdentifierError> {
                Identifier::parse(value).map(Self)
            }
            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
            #[must_use]
            pub fn into_inner(self) -> Identifier {
                self.0
            }
        }
        impl FromStr for $name {
            type Err = IdentifierError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

/// A canonical Marksheet identifier: `[a-z][a-z0-9_]*`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Identifier(String);

impl Identifier {
    /// # Errors
    ///
    /// Returns [`IdentifierError`] when `value` does not match the identifier grammar.
    pub fn parse(value: &str) -> Result<Self, IdentifierError> {
        let bytes = value.as_bytes();
        if bytes.is_empty() {
            return Err(IdentifierError::Empty);
        }
        if !bytes[0].is_ascii_lowercase() {
            return Err(IdentifierError::Invalid {
                value: value.to_owned(),
            });
        }
        if bytes[1..]
            .iter()
            .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'_')
        {
            return Err(IdentifierError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Identifier {
    type Error = IdentifierError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<Identifier> for String {
    fn from(value: Identifier) -> Self {
        value.0
    }
}
impl FromStr for Identifier {
    type Err = IdentifierError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentifierError {
    Empty,
    Invalid { value: String },
}
impl fmt::Display for IdentifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identifier is empty"),
            Self::Invalid { value } => write!(f, "invalid identifier {value:?}"),
        }
    }
}
impl std::error::Error for IdentifierError {}

typed_identifier!(SheetId, "A stable sheet identifier.");
typed_identifier!(TableId, "A workbook-scoped table identifier.");
typed_identifier!(NameId, "A workbook-scoped named-reference identifier.");
typed_identifier!(StyleId, "A workbook-scoped style identifier.");

/// A concrete one-based spreadsheet coordinate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Coordinate {
    pub column: u64,
    pub row: u64,
}

impl Coordinate {
    /// # Errors
    ///
    /// Returns an error when either one-based axis is zero.
    pub fn new(column: u64, row: u64) -> Result<Self, CoordinateError> {
        if column == 0 {
            return Err(CoordinateError::ZeroColumn);
        }
        if row == 0 {
            return Err(CoordinateError::ZeroRow);
        }
        Ok(Self { column, row })
    }

    /// # Errors
    ///
    /// Returns an error for malformed A1 input or an overflowing axis.
    pub fn parse(value: &str) -> Result<Self, CoordinateError> {
        let split = value
            .as_bytes()
            .iter()
            .position(u8::is_ascii_digit)
            .ok_or_else(|| CoordinateError::Invalid {
                value: value.to_owned(),
            })?;
        let (column, row) = value.split_at(split);
        if column.is_empty()
            || row.is_empty()
            || !column.bytes().all(|byte| byte.is_ascii_alphabetic())
            || !row.bytes().all(|byte| byte.is_ascii_digit())
            || (row.len() > 1 && row.starts_with('0'))
        {
            return Err(CoordinateError::Invalid {
                value: value.to_owned(),
            });
        }
        let mut column_number = 0_u64;
        for byte in column.bytes() {
            let digit = u64::from(byte.to_ascii_uppercase() - b'A' + 1);
            column_number = column_number
                .checked_mul(26)
                .and_then(|current| current.checked_add(digit))
                .ok_or(CoordinateError::Overflow)?;
        }
        let row_number = row.parse::<u64>().map_err(|_| CoordinateError::Overflow)?;
        Self::new(column_number, row_number)
    }

    #[must_use]
    pub fn column_name(self) -> String {
        let mut number = self.column;
        let mut result = String::new();
        while number > 0 {
            let remainder = ((number - 1) % 26) as u8;
            result.push(char::from(b'A' + remainder));
            number = (number - 1) / 26;
        }
        result.chars().rev().collect()
    }

    /// # Errors
    ///
    /// Returns [`CoordinateError::Overflow`] when the translated coordinate
    /// cannot fit in `u64`.
    pub fn offset(self, columns: u64, rows: u64) -> Result<Self, CoordinateError> {
        Self::new(
            self.column
                .checked_add(columns)
                .ok_or(CoordinateError::Overflow)?,
            self.row
                .checked_add(rows)
                .ok_or(CoordinateError::Overflow)?,
        )
    }
}
impl FromStr for Coordinate {
    type Err = CoordinateError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl fmt::Display for Coordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.column_name(), self.row)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoordinateError {
    ZeroColumn,
    ZeroRow,
    Invalid { value: String },
    Overflow,
}
impl fmt::Display for CoordinateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroColumn => f.write_str("column must be at least one"),
            Self::ZeroRow => f.write_str("row must be at least one"),
            Self::Invalid { value } => write!(f, "invalid A1 coordinate {value:?}"),
            Self::Overflow => f.write_str("coordinate exceeds u64 limits"),
        }
    }
}
impl std::error::Error for CoordinateError {}

/// An inclusive rectangular A1 range. Inputs are normalized to top-left/bottom-right.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub start: Coordinate,
    pub end: Coordinate,
}
impl Range {
    #[must_use]
    pub fn new(first: Coordinate, second: Coordinate) -> Self {
        Self {
            start: Coordinate {
                column: first.column.min(second.column),
                row: first.row.min(second.row),
            },
            end: Coordinate {
                column: first.column.max(second.column),
                row: first.row.max(second.row),
            },
        }
    }
    #[must_use]
    pub fn single(cell: Coordinate) -> Self {
        Self {
            start: cell,
            end: cell,
        }
    }
    /// # Errors
    ///
    /// Returns an error for malformed A1 input or an overflowing axis.
    pub fn parse(value: &str) -> Result<Self, CoordinateError> {
        match value.split_once(':') {
            Some((first, second)) if !second.contains(':') => Ok(Self::new(
                Coordinate::parse(first)?,
                Coordinate::parse(second)?,
            )),
            Some(_) => Err(CoordinateError::Invalid {
                value: value.to_owned(),
            }),
            None => Ok(Self::single(Coordinate::parse(value)?)),
        }
    }
    /// # Errors
    ///
    /// Returns an overflow error if manually constructed endpoints cannot
    /// produce an inclusive width.
    pub fn width(self) -> Result<u64, CoordinateError> {
        self.end
            .column
            .checked_sub(self.start.column)
            .and_then(|value| value.checked_add(1))
            .ok_or(CoordinateError::Overflow)
    }
    /// # Errors
    ///
    /// Returns an overflow error if manually constructed endpoints cannot
    /// produce an inclusive height.
    pub fn height(self) -> Result<u64, CoordinateError> {
        self.end
            .row
            .checked_sub(self.start.row)
            .and_then(|value| value.checked_add(1))
            .ok_or(CoordinateError::Overflow)
    }
    /// # Errors
    ///
    /// Returns an error if the inclusive dimensions cannot be represented.
    pub fn footprint(self) -> Result<Footprint, CoordinateError> {
        Footprint::new(self.start, self.width()?, self.height()?)
    }
    #[must_use]
    pub const fn contains(self, coordinate: Coordinate) -> bool {
        self.start.column <= coordinate.column
            && coordinate.column <= self.end.column
            && self.start.row <= coordinate.row
            && coordinate.row <= self.end.row
    }
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start.column <= other.end.column
            && other.start.column <= self.end.column
            && self.start.row <= other.end.row
            && other.start.row <= self.end.row
    }
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        self.overlaps(other).then(|| Self {
            start: Coordinate {
                column: self.start.column.max(other.start.column),
                row: self.start.row.max(other.start.row),
            },
            end: Coordinate {
                column: self.end.column.min(other.end.column),
                row: self.end.row.min(other.end.row),
            },
        })
    }
}
impl FromStr for Range {
    type Err = CoordinateError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start == self.end {
            self.start.fmt(f)
        } else {
            write!(f, "{}:{}", self.start, self.end)
        }
    }
}

/// A sparse rectangular reservation represented by its anchor and dimensions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub anchor: Coordinate,
    pub width: u64,
    pub height: u64,
}
impl Footprint {
    /// # Errors
    ///
    /// Returns an error for zero dimensions or an end coordinate beyond `u64`.
    pub fn new(anchor: Coordinate, width: u64, height: u64) -> Result<Self, CoordinateError> {
        if width == 0 || height == 0 {
            return Err(CoordinateError::Invalid {
                value: "footprint dimensions must be positive".to_owned(),
            });
        }
        anchor.offset(width - 1, height - 1)?;
        Ok(Self {
            anchor,
            width,
            height,
        })
    }
    /// # Errors
    ///
    /// Returns an error if the footprint's computed end coordinate overflows.
    pub fn range(self) -> Result<Range, CoordinateError> {
        Ok(Range {
            start: self.anchor,
            end: self.anchor.offset(self.width - 1, self.height - 1)?,
        })
    }
    /// # Errors
    ///
    /// Returns an error if the range dimensions cannot be represented.
    pub fn from_range(range: Range) -> Result<Self, CoordinateError> {
        range.footprint()
    }
    /// # Errors
    ///
    /// Returns an error if either footprint has an overflowing end coordinate.
    pub fn overlaps(self, other: Self) -> Result<bool, CoordinateError> {
        Ok(self.range()?.overlaps(other.range()?))
    }
}

/// A sheet-qualified coordinate.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SheetCoordinate {
    pub sheet: SheetId,
    pub coordinate: Coordinate,
}
/// A sheet-qualified range.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SheetRange {
    pub sheet: SheetId,
    pub range: Range,
}

/// A formula exactly as authored, including its leading `=`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FormulaSource(String);
impl FormulaSource {
    /// # Errors
    ///
    /// Returns [`ScalarParseError::FormulaMissingEquals`] if `source` does not
    /// begin with a formula marker.
    pub fn new(source: impl Into<String>) -> Result<Self, ScalarParseError> {
        let source = source.into();
        if source.starts_with('=') {
            Ok(Self(source))
        } else {
            Err(ScalarParseError::FormulaMissingEquals)
        }
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for FormulaSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Core error values, kept distinct from arbitrary text.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum CellError {
    #[serde(rename = "#DIV/0!")]
    DivisionByZero,
    #[serde(rename = "#N/A")]
    NotAvailable,
    #[serde(rename = "#NAME?")]
    Name,
    #[serde(rename = "#NUM!")]
    Number,
    #[serde(rename = "#REF!")]
    Reference,
    #[serde(rename = "#VALUE!")]
    Value,
    #[serde(rename = "#CIRC!")]
    Circular,
}
impl CellError {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "#DIV/0!" => Self::DivisionByZero,
            "#N/A" => Self::NotAvailable,
            "#NAME?" => Self::Name,
            "#NUM!" => Self::Number,
            "#REF!" => Self::Reference,
            "#VALUE!" => Self::Value,
            "#CIRC!" => Self::Circular,
            _ => return None,
        })
    }
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::DivisionByZero => "#DIV/0!",
            Self::NotAvailable => "#N/A",
            Self::Name => "#NAME?",
            Self::Number => "#NUM!",
            Self::Reference => "#REF!",
            Self::Value => "#VALUE!",
            Self::Circular => "#CIRC!",
        }
    }
}
impl fmt::Display for CellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// Compatibility name for the core scalar error value.
pub type Error = CellError;

/// The scalar layer after CSV decoding. `Blank` is intentionally distinct from `Text("")`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    Blank,
    Text(String),
    Number(f64),
    Boolean(bool),
    Date(Date),
    DateTime(OffsetDateTime),
    Formula(FormulaSource),
    Error(CellError),
}
impl Value {
    /// Interprets a decoded CSV field using the precedence defined by the format.
    #[must_use]
    pub fn from_csv_field(field: &str) -> Self {
        if field.is_empty() {
            return Self::Blank;
        }
        if let Some(text) = field.strip_prefix('\'') {
            return Self::Text(text.to_owned());
        }
        if field.starts_with('=') {
            return Self::Formula(FormulaSource(field.to_owned()));
        }
        if let Some(error) = CellError::parse(field) {
            return Self::Error(error);
        }
        if field == "true" {
            return Self::Boolean(true);
        }
        if field == "false" {
            return Self::Boolean(false);
        }
        if let Some(number) = parse_number(field) {
            return Self::Number(number);
        }
        if looks_like_date(field) {
            if let Some(date) = parse_date(field) {
                return Self::Date(date);
            }
        }
        if looks_like_datetime(field) {
            if let Some(datetime) = parse_datetime(field) {
                return Self::DateTime(datetime);
            }
        }
        Self::Text(field.to_owned())
    }

    /// Like [`Value::from_csv_field`], but reports malformed ISO-looking dates
    /// instead of silently treating them as text. Validators should use this.
    /// # Errors
    ///
    /// Returns an error for an ISO-shaped but invalid date or datetime.
    pub fn parse_strict(field: &str) -> Result<Self, ScalarParseError> {
        let value = Self::from_csv_field(field);
        if matches!(value, Self::Text(_)) && looks_like_date(field) {
            return Err(ScalarParseError::InvalidDate {
                value: field.to_owned(),
            });
        }
        if matches!(value, Self::Text(_)) && looks_like_datetime(field) {
            return Err(ScalarParseError::InvalidDateTime {
                value: field.to_owned(),
            });
        }
        Ok(value)
    }
}

fn parse_number(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let mut index = 0;
    if bytes.first() == Some(&b'-') {
        index += 1;
    }
    let integer_start = index;
    match bytes.get(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => {
            index += 1;
            while matches!(bytes.get(index), Some(b'0'..=b'9')) {
                index += 1;
            }
        }
        _ => return None,
    }
    if index - integer_start > 1 && bytes[integer_start] == b'0' {
        return None;
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fractional_start = index;
        while matches!(bytes.get(index), Some(b'0'..=b'9')) {
            index += 1;
        }
        if index == fractional_start {
            return None;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while matches!(bytes.get(index), Some(b'0'..=b'9')) {
            index += 1;
        }
        if index == exponent_start {
            return None;
        }
    }
    (index == bytes.len())
        .then(|| {
            value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
        })
        .flatten()
}

/// Returns the canonical decimal spelling for a finite binary64 number.
///
/// The formatter compares Rust's shortest round-trippable display spelling
/// with an equivalent normalized scientific spelling. The shorter byte string
/// wins; ties prefer the display spelling, which avoids an exponent when both
/// spellings are equally compact. Negative zero is emitted as `-0`.
///
/// # Errors
///
/// Returns [`CanonicalNumberError::NonFinite`] for NaN or either infinity,
/// which are not Marksheet source number literals.
pub fn canonical_number(value: f64) -> Result<String, CanonicalNumberError> {
    if !value.is_finite() {
        return Err(CanonicalNumberError::NonFinite);
    }
    if value == 0.0 {
        return Ok(if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        });
    }

    let display = value.to_string();
    let scientific = normalized_scientific(&display);
    Ok(if scientific.len() < display.len() {
        scientific
    } else {
        display
    })
}

/// The error returned by [`canonical_number`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalNumberError {
    NonFinite,
}
impl fmt::Display for CanonicalNumberError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Marksheet numbers must be finite")
    }
}
impl std::error::Error for CanonicalNumberError {}

/// Converts a shortest decimal spelling into a normalized, equal-value
/// scientific spelling. `f64::to_string` only emits ASCII decimal syntax.
fn normalized_scientific(display: &str) -> String {
    let (negative, unsigned) = match display.strip_prefix('-') {
        Some(unsigned) => (true, unsigned),
        None => (false, display),
    };
    let (source_mantissa, explicit_exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap_or(0)),
        None => (unsigned, 0),
    };
    let decimal_position = source_mantissa.find('.').unwrap_or(source_mantissa.len());
    let digits: String = source_mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let leading_zeroes = digits.bytes().take_while(|digit| *digit == b'0').count();
    let significant = digits[leading_zeroes..].trim_end_matches('0');

    // `canonical_number` handles zero before reaching this helper.
    debug_assert!(!significant.is_empty());
    let exponent = explicit_exponent + i32::try_from(decimal_position).unwrap_or(i32::MAX)
        - 1
        - i32::try_from(leading_zeroes).unwrap_or(i32::MAX);
    let mut result = String::new();
    if negative {
        result.push('-');
    }
    result.push(char::from(significant.as_bytes()[0]));
    if significant.len() > 1 {
        result.push('.');
        result.push_str(&significant[1..]);
    }
    result.push('e');
    result.push_str(&exponent.to_string());
    result
}
fn looks_like_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10 && has_date_prefix(bytes) && bytes[8..].iter().all(u8::is_ascii_digit)
}
fn looks_like_datetime(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 19
        || !has_date_prefix(bytes)
        || bytes[10] != b'T'
        || !bytes[11..13].iter().all(u8::is_ascii_digit)
        || bytes[13] != b':'
        || !bytes[14..16].iter().all(u8::is_ascii_digit)
        || bytes[16] != b':'
        || !bytes[17..19].iter().all(u8::is_ascii_digit)
    {
        return false;
    }

    let mut offset = 19;
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let fractional_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == fractional_start {
            return false;
        }
    }

    let tail = &bytes[offset..];
    // A complete date/time with no offset is an invalid datetime candidate,
    // rather than ordinary text, because the format requires an offset.
    tail.is_empty()
        || tail == b"Z"
        || (tail.len() == 6
            && matches!(tail[0], b'+' | b'-')
            && tail[3] == b':'
            && tail[1..3].iter().all(u8::is_ascii_digit)
            && tail[4..6].iter().all(u8::is_ascii_digit))
}

fn has_date_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}
fn parse_date(value: &str) -> Option<Date> {
    time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
        .ok()
        .and_then(|format| Date::parse(value, &format).ok())
}
fn parse_datetime(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScalarParseError {
    FormulaMissingEquals,
    InvalidDate { value: String },
    InvalidDateTime { value: String },
}
impl fmt::Display for ScalarParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FormulaMissingEquals => f.write_str("formula source must begin with '='"),
            Self::InvalidDate { value } => write!(f, "invalid ISO date {value:?}"),
            Self::InvalidDateTime { value } => write!(f, "invalid ISO datetime {value:?}"),
        }
    }
}
impl std::error::Error for ScalarParseError {}

/// A value together with its field source origin, if it came from source text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub value: Value,
    pub origin: Option<Origin>,
}
impl Cell {
    #[must_use]
    pub fn new(value: Value) -> Self {
        Self {
            value,
            origin: None,
        }
    }
}

/// A sparse rectangular CSV block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub anchor: Coordinate,
    pub cells: Vec<Vec<Cell>>,
    pub origin: Option<Origin>,
}
impl Block {
    /// # Errors
    ///
    /// Returns an error for an empty or non-rectangular cell matrix.
    pub fn new(anchor: Coordinate, cells: Vec<Vec<Cell>>) -> Result<Self, BlockError> {
        let width = cells.first().ok_or(BlockError::Empty)?.len();
        if width == 0 {
            return Err(BlockError::Empty);
        }
        if cells.iter().any(|row| row.len() != width) {
            return Err(BlockError::NonRectangular);
        }
        Ok(Self {
            anchor,
            cells,
            origin: None,
        })
    }
    /// # Errors
    ///
    /// Returns an error if the block's dimensions overflow its anchor.
    pub fn footprint(&self) -> Result<Footprint, CoordinateError> {
        Footprint::new(
            self.anchor,
            u64::try_from(self.cells.first().map_or(0, Vec::len))
                .map_err(|_| CoordinateError::Overflow)?,
            u64::try_from(self.cells.len()).map_err(|_| CoordinateError::Overflow)?,
        )
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockError {
    Empty,
    NonRectangular,
}
impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("block requires at least one row and field"),
            Self::NonRectangular => f.write_str("block rows must have equal field counts"),
        }
    }
}
impl std::error::Error for BlockError {}

/// A named block whose first row is its header row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub id: TableId,
    pub block: Block,
    pub origin: Option<Origin>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum TableRegion {
    Headers,
    Data,
    Column { header: String },
}
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum NameTarget {
    /// A name that resolves to exactly one sheet-qualified cell.
    ///
    /// This is intentionally distinct from [`Self::Range`]: a source target
    /// such as `sheet!A1:A1` is still range-shaped and therefore retains range
    /// behavior in the calculation layer.
    Cell(SheetCoordinate),
    Range(SheetRange),
    TableColumn {
        table: TableId,
        header: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Name {
    pub id: NameId,
    pub target: NameTarget,
    pub origin: Option<Origin>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum FillTarget {
    Range(Range),
    TableColumn { table: TableId, header: String },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub target: FillTarget,
    pub formula: FormulaSource,
    pub origin: Option<Origin>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum HorizontalAlignment {
    Left,
    Center,
    Right,
    General,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum VerticalAlignment {
    Top,
    Middle,
    Bottom,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum NumberFormat {
    General,
    Integer,
    Decimal,
    Percent,
    Currency,
    Date,
    DateTime,
}
/// A CSS-style RGB or RGBA color using `#RRGGBB` or `#RRGGBBAA` spelling.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Color(String);
impl Color {
    /// # Errors
    ///
    /// Returns an error unless `value` is a six- or eight-digit hexadecimal color.
    pub fn parse(value: &str) -> Result<Self, ColorError> {
        let valid = matches!(value.len(), 7 | 9)
            && value.starts_with('#')
            && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit);
        valid
            .then(|| Self(value.to_owned()))
            .ok_or_else(|| ColorError {
                value: value.to_owned(),
            })
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColorError {
    pub value: String,
}
impl fmt::Display for ColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid color {:?}", self.value)
    }
}
impl std::error::Error for ColorError {}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct StyleProperties {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub wrap: Option<bool>,
    pub text_color: Option<Color>,
    pub fill: Option<Color>,
    pub font_size: Option<f64>,
    pub align: Option<HorizontalAlignment>,
    pub valign: Option<VerticalAlignment>,
    pub number: Option<NumberFormat>,
    pub decimals: Option<u8>,
    pub currency: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub id: StyleId,
    pub properties: StyleProperties,
    pub origin: Option<Origin>,
}
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ApplyTarget {
    Range(Range),
    Table { table: TableId, region: TableRegion },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Apply {
    pub target: ApplyTarget,
    pub styles: Vec<StyleId>,
    pub origin: Option<Origin>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ColumnRange {
    pub start: u64,
    pub end: u64,
}
impl ColumnRange {
    /// # Errors
    ///
    /// Returns an error when either one-based column is zero.
    pub fn new(first: u64, second: u64) -> Result<Self, CoordinateError> {
        if first == 0 || second == 0 {
            return Err(CoordinateError::ZeroColumn);
        }
        Ok(Self {
            start: first.min(second),
            end: first.max(second),
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct RowRange {
    pub start: u64,
    pub end: u64,
}
impl RowRange {
    /// # Errors
    ///
    /// Returns an error when either one-based row is zero.
    pub fn new(first: u64, second: u64) -> Result<Self, CoordinateError> {
        if first == 0 || second == 0 {
            return Err(CoordinateError::ZeroRow);
        }
        Ok(Self {
            start: first.min(second),
            end: first.max(second),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnGeometry {
    pub columns: ColumnRange,
    pub width: f64,
    pub origin: Option<Origin>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RowGeometry {
    pub rows: RowRange,
    pub height: f64,
    pub origin: Option<Origin>,
}

/// A declared extension capability such as `charts@1`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ExtensionId {
    pub id: Identifier,
    pub major: u64,
}
impl ExtensionId {
    /// # Errors
    ///
    /// Returns an error for an invalid identifier, missing separator, or zero/non-numeric major version.
    pub fn parse(value: &str) -> Result<Self, ExtensionIdError> {
        let (id, major) = value
            .rsplit_once('@')
            .ok_or_else(|| ExtensionIdError::Invalid {
                value: value.to_owned(),
            })?;
        let id = Identifier::parse(id).map_err(ExtensionIdError::Identifier)?;
        let major = major.parse().map_err(|_| ExtensionIdError::Invalid {
            value: value.to_owned(),
        })?;
        if major == 0 {
            return Err(ExtensionIdError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self { id, major })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionIdError {
    Identifier(IdentifierError),
    Invalid { value: String },
}
impl fmt::Display for ExtensionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identifier(error) => error.fmt(f),
            Self::Invalid { value } => write!(f, "invalid extension id {value:?}"),
        }
    }
}
impl std::error::Error for ExtensionIdError {}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDeclaration {
    pub capability: ExtensionId,
    pub required: bool,
    pub origin: Option<Origin>,
}
/// Opaque source payload retained even when its capability is unsupported.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Extension {
    pub capability: ExtensionId,
    pub name: String,
    pub payload: String,
    pub origin: Option<Origin>,
    pub payload_origin: Option<Origin>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SheetItem {
    Block(Block),
    Table(Table),
    Fill(Fill),
    Apply(Apply),
    ColumnGeometry(ColumnGeometry),
    RowGeometry(RowGeometry),
    Extension(Extension),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub id: SheetId,
    pub label: String,
    pub items: Vec<SheetItem>,
    pub origin: Option<Origin>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkbookSettings {
    pub locale: String,
    pub timezone: String,
    pub formula_profile: String,
}
impl Default for WorkbookSettings {
    fn default() -> Self {
        Self {
            locale: "en-US".to_owned(),
            timezone: "UTC".to_owned(),
            formula_profile: "portable-a1@1".to_owned(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workbook {
    pub settings: WorkbookSettings,
    pub styles: Vec<Style>,
    pub names: Vec<Name>,
    pub extensions: Vec<ExtensionDeclaration>,
    /// Opaque extension instances declared before the first sheet.
    pub extension_instances: Vec<Extension>,
    pub sheets: Vec<Sheet>,
    pub origin: Option<Origin>,
}

/// A stable Marksheet diagnostic code in the form `MS` followed by four digits.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DiagnosticCode(String);
impl DiagnosticCode {
    /// # Errors
    ///
    /// Returns an error unless `code` is exactly `MS` followed by four ASCII digits.
    pub fn new(code: impl Into<String>) -> Result<Self, DiagnosticCodeError> {
        let code = code.into();
        let bytes = code.as_bytes();
        if bytes.len() != 6
            || !bytes.starts_with(b"MS")
            || !bytes[2..].iter().all(u8::is_ascii_digit)
        {
            return Err(DiagnosticCodeError { code });
        }
        Ok(Self(code))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for DiagnosticCode {
    type Error = DiagnosticCodeError;
    fn try_from(code: String) -> Result<Self, Self::Error> {
        Self::new(code)
    }
}
impl From<DiagnosticCode> for String {
    fn from(code: DiagnosticCode) -> Self {
        code.0
    }
}
impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticCodeError {
    pub code: String,
}
impl fmt::Display for DiagnosticCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "diagnostic code {:?} must match MS followed by four digits",
            self.code
        )
    }
}
impl std::error::Error for DiagnosticCodeError {}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LabeledSpan {
    pub span: ByteSpan,
    pub label: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelatedDiagnostic {
    pub message: String,
    pub span: LabeledSpan,
}
/// Backwards-compatible descriptive name for a diagnostic's secondary source span.
pub type RelatedSpan = RelatedDiagnostic;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticContext {
    pub sheet: Option<SheetId>,
    pub cell: Option<Coordinate>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub message: String,
    pub replacement: String,
    pub span: ByteSpan,
}
/// Structured, stable parser/validator feedback with source and optional grid context.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub message: String,
    pub primary: LabeledSpan,
    pub related: Vec<RelatedDiagnostic>,
    pub context: Option<DiagnosticContext>,
    pub suggestion: Option<Suggestion>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans_are_half_open() {
        let span = ByteSpan::try_new(2, 5).unwrap();
        assert!(span.contains_offset(2));
        assert!(!span.contains_offset(5));
        assert!(span.overlaps(ByteSpan::try_new(4, 7).unwrap()));
        assert!(!span.overlaps(ByteSpan::try_new(5, 8).unwrap()));
    }
    #[test]
    fn line_index_counts_unicode_scalars() {
        let index = LineIndex::new("aé\n🙂z");
        assert_eq!(
            index.line_column(3).unwrap(),
            LineColumn { line: 1, column: 3 }
        );
        assert_eq!(
            index.line_column(4).unwrap(),
            LineColumn { line: 2, column: 1 }
        );
        assert!(matches!(
            index.line_column(2),
            Err(PositionError::NotScalarBoundary { .. })
        ));
        assert_eq!(
            index.line_column(index.source_len()).unwrap(),
            LineColumn { line: 2, column: 3 }
        );
    }
    #[test]
    fn coordinates_round_trip_and_check_limits() {
        for (source, column, row) in [
            ("A1", 1, 1),
            ("z42", 26, 42),
            ("AA2", 27, 2),
            ("XFD1048576", 16384, 1_048_576),
        ] {
            let coordinate = Coordinate::parse(source).unwrap();
            assert_eq!((coordinate.column, coordinate.row), (column, row));
            assert_eq!(coordinate.to_string(), source.to_uppercase());
        }
        for invalid in ["A0", "A01", "0", "$A1", "A-1", "A1B"] {
            assert!(Coordinate::parse(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn ranges_normalize_and_overlap() {
        let first = Range::parse("D20:B7").unwrap();
        assert_eq!(first.to_string(), "B7:D20");
        assert_eq!(first.width().unwrap(), 3);
        assert!(first.overlaps(Range::parse("D20:E22").unwrap()));
        assert!(!first.overlaps(Range::parse("E7:F20").unwrap()));
    }
    #[test]
    fn named_cells_remain_distinct_from_single_cell_ranges() {
        let sheet = SheetId::parse("summary").unwrap();
        let cell = Coordinate::parse("B2").unwrap();
        let named_cell = NameTarget::Cell(SheetCoordinate {
            sheet: sheet.clone(),
            coordinate: cell,
        });
        let named_range = NameTarget::Range(SheetRange {
            sheet,
            range: Range::single(cell),
        });

        assert_ne!(named_cell, named_range);
    }
    #[test]
    fn footprints_are_checked_for_distant_cells() {
        let anchor = Coordinate::parse("A1").unwrap();
        let footprint = Footprint::new(anchor, u64::MAX, 1);
        assert!(footprint.is_ok());
        let final_column = Coordinate::new(u64::MAX, 1).unwrap();
        assert!(Footprint::new(final_column, 2, 1).is_err());
        let distant = Footprint::new(Coordinate::parse("A1000000000").unwrap(), 2, 2).unwrap();
        assert_eq!(
            distant.range().unwrap().to_string(),
            "A1000000000:B1000000001"
        );
    }
    #[test]
    fn scalar_precedence_keeps_blank_distinct_from_empty_text() {
        assert_eq!(Value::from_csv_field(""), Value::Blank);
        assert_eq!(Value::from_csv_field("'"), Value::Text(String::new()));
        assert!(matches!(Value::from_csv_field("=true"), Value::Formula(_)));
        assert_eq!(Value::from_csv_field("'true"), Value::Text("true".into()));
        assert_eq!(Value::from_csv_field("true"), Value::Boolean(true));
        assert_eq!(Value::from_csv_field("001"), Value::Text("001".into()));
        assert_eq!(
            Value::from_csv_field("#REF!"),
            Value::Error(CellError::Reference)
        );
    }
    #[test]
    fn numbers_and_dates_are_strict() {
        assert_eq!(Value::from_csv_field("-12.5e+2"), Value::Number(-1250.0));
        assert!(matches!(Value::from_csv_field("1."), Value::Text(_)));
        assert!(matches!(
            Value::from_csv_field("2024-02-29"),
            Value::Date(_)
        ));
        assert_eq!(
            Value::from_csv_field("2023-02-29"),
            Value::Text("2023-02-29".into())
        );
        assert!(matches!(
            Value::parse_strict("2023-02-29"),
            Err(ScalarParseError::InvalidDate { .. })
        ));
        assert!(matches!(
            Value::from_csv_field("2026-08-16T14:30:00Z"),
            Value::DateTime(_)
        ));
        assert!(matches!(
            Value::parse_strict("2026-08-16T14:30:00"),
            Err(ScalarParseError::InvalidDateTime { .. })
        ));
        assert!(matches!(
            Value::from_csv_field("2026-08-16T14:30:00.125-04:00"),
            Value::DateTime(_)
        ));
        assert_eq!(
            Value::parse_strict("abcd-ef-gh").unwrap(),
            Value::Text("abcd-ef-gh".into())
        );
        assert_eq!(
            Value::parse_strict("abcdefghijTrest").unwrap(),
            Value::Text("abcdefghijTrest".into())
        );
    }
    #[test]
    fn blocks_remain_sparse_at_distant_coordinates() {
        let block = Block::new(
            Coordinate::parse("A1000000000").unwrap(),
            vec![vec![Cell::new(Value::Number(1.0))]],
        )
        .unwrap();
        assert_eq!(
            block.footprint().unwrap().range().unwrap().to_string(),
            "A1000000000"
        );
        assert_eq!(
            std::mem::size_of_val(&block.cells),
            std::mem::size_of::<Vec<Vec<Cell>>>()
        );
    }
    #[test]
    fn diagnostic_codes_are_stable_and_constrained() {
        assert_eq!(DiagnosticCode::new("MS1001").unwrap().as_str(), "MS1001");
        assert!(DiagnosticCode::new("MS1").is_err());
        assert!(DiagnosticCode::new("syntax-error").is_err());
    }
    #[test]
    fn canonical_numbers_are_shortest_and_round_trip() {
        for (value, expected) in [
            (1e20, "1e20"),
            (1e-7, "1e-7"),
            (12.5, "12.5"),
            (-0.0, "-0"),
            (1.23e-10, "1.23e-10"),
        ] {
            let actual = canonical_number(value).unwrap();
            assert_eq!(actual, expected);
            assert_eq!(actual.parse::<f64>().unwrap().to_bits(), value.to_bits());
        }
        assert!(matches!(
            canonical_number(f64::INFINITY),
            Err(CanonicalNumberError::NonFinite)
        ));
    }
}
