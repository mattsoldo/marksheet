use marksheet_model::{ByteSpan, CellError, Coordinate, NameId, SheetId, TableId};
use serde::{Deserialize, Serialize};

/// A parsed formula. The leading `=` is syntax and is not represented in
/// [`Expr`], but all expression spans refer to offsets in the complete source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Formula {
    pub expression: Expr,
}

/// An expression annotated with its half-open UTF-8 byte span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: ByteSpan,
}

impl Expr {
    #[must_use]
    pub const fn new(kind: ExprKind, span: ByteSpan) -> Self {
        Self { kind, span }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExprKind {
    Literal {
        value: Literal,
    },
    Reference {
        reference: Reference,
    },
    Unary {
        operator: UnaryOperator,
        operand: Box<Expr>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Call {
        call: FunctionCall,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Literal {
    Number(f64),
    Text(String),
    Boolean(bool),
    Error(CellError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOperator {
    Positive,
    Negative,
}

impl UnaryOperator {
    #[must_use]
    pub const fn symbol(self) -> char {
        match self {
            Self::Positive => '+',
            Self::Negative => '-',
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryOperator {
    Power,
    Multiply,
    Divide,
    Add,
    Subtract,
    Concatenate,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl BinaryOperator {
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Power => "^",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Concatenate => "&",
            Self::Equal => "=",
            Self::NotEqual => "<>",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
        }
    }
}

/// A function call. Names are stored in their canonical uppercase spelling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: Vec<Expr>,
}

/// An A1 address with independently copyable axes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct A1Reference {
    pub coordinate: Coordinate,
    pub column_absolute: bool,
    pub row_absolute: bool,
}

/// A range retains endpoint order. Lookup may normalize it, but preserving the
/// authored direction makes copy adjustment and diagnostics unsurprising.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RangeReference {
    pub sheet: Option<SheetId>,
    pub start: A1Reference,
    pub end: A1Reference,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reference {
    Cell {
        sheet: Option<SheetId>,
        address: A1Reference,
    },
    Range(RangeReference),
    Name {
        name: NameId,
    },
    Structured(StructuredReference),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StructuredReference {
    Column {
        table: TableId,
        header: String,
    },
    Region {
        table: TableId,
        region: TableRegion,
    },
    CurrentRow {
        table: Option<TableId>,
        header: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableRegion {
    Headers,
    Data,
}
