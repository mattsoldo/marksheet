//! Lossless concrete syntax for the Marksheet outer language.

use std::ops::Range;

/// A half-open byte span into the original document.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    #[must_use]
    pub const fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

/// One physical source line, including its exact line-ending spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Line {
    pub span: Span,
    pub content: Span,
    pub newline: Span,
}

/// A directive line split only at the outer-language level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directive {
    pub line: Line,
    /// Span of the name without the leading `@`.
    pub name: Span,
    /// Everything after the name and before the physical newline.
    pub arguments: Span,
}

/// Whether a CSV-bearing directive is a block or a table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CsvKind {
    Block,
    Table,
}

/// An exact CSV field and its decoded content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsvField {
    /// Exact field spelling, including surrounding quotes when present.
    pub span: Span,
    pub decoded: String,
    pub quoted: bool,
}

/// One decoded CSV record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsvRecord {
    /// Covers fields and delimiters, but not the record-ending newline.
    pub span: Span,
    pub fields: Vec<CsvField>,
    /// Exact CRLF, LF, bare CR, or empty span at EOF/recovery.
    pub newline: Span,
}

/// A complete `@block` or `@table`, including recovery when `@end` is absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsvBlock {
    pub kind: CsvKind,
    pub directive: Directive,
    /// All bytes after the directive newline and before the terminator.
    pub body: Span,
    pub records: Vec<CsvRecord>,
    pub terminator: Option<Line>,
    pub span: Span,
}

/// A complete opaque extension instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionBlock {
    pub directive: Directive,
    /// Payload bytes exactly as authored, including a trailing newline when one
    /// precedes `@end`.
    pub payload: Span,
    pub terminator: Option<Line>,
    pub span: Span,
}

/// Every node owns a disjoint, ordered portion of the source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Node {
    Header(Line),
    Comment(Line),
    Blank(Line),
    Directive(Directive),
    CsvBlock(CsvBlock),
    Extension(ExtensionBlock),
    /// Bytes that could not be classified as a valid outer construct.
    Recovery(Line),
}

impl Node {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Header(line) | Self::Comment(line) | Self::Blank(line) | Self::Recovery(line) => {
                line.span
            }
            Self::Directive(directive) => directive.line.span,
            Self::CsvBlock(block) => block.span,
            Self::Extension(extension) => extension.span,
        }
    }
}

/// The lossless outer syntax tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cst {
    pub nodes: Vec<Node>,
    pub span: Span,
}
