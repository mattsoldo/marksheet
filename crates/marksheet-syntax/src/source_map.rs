//! Deterministic, syntax-owned source locations for lossless editing.
//!
//! The map is intentionally derived from the CST rather than the lowered
//! workbook.  That keeps useful locations available when semantic validation
//! fails, while the keyed lookup APIs only return unambiguous, parseable keys.
//! A missing location is therefore meaningful: callers must not manufacture a
//! replacement span for a malformed or recovered construct.

use std::collections::{BTreeMap, BTreeSet};

use marksheet_model::{ByteSpan, Coordinate, NameId, SheetId, StyleId, TableId};

use crate::cst::{Cst, CsvBlock, CsvKind, Directive, ExtensionBlock, Line, Node, Span};

/// A directive's exact outer-language source spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectiveLocation {
    /// The complete physical line, including its newline when present.
    pub line: ByteSpan,
    /// The directive name without its leading `@`.
    pub name: ByteSpan,
    /// The arguments after the directive name and separator spaces.
    pub arguments: ByteSpan,
}

/// The exact CSV field that authored a cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellLocation {
    /// Field spelling, including CSV quotes when present.
    pub field: ByteSpan,
    /// Record spelling, excluding its record-ending newline.
    pub record: ByteSpan,
    /// The enclosing `@block` or `@table` construct.
    pub container: ByteSpan,
}

/// A source location for one `@block` or `@table`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsvBlockLocation {
    /// Whether this construct is an unnamed block or a named table.
    pub kind: CsvKind,
    /// The directive line.
    pub directive: DirectiveLocation,
    /// Table identifier token for `@table`; absent for `@block` or malformed
    /// argument lists.
    pub table_id: Option<ByteSpan>,
    /// Anchor token, when the directive has enough lexically valid arguments.
    pub anchor: Option<ByteSpan>,
    /// Exact CSV body, excluding the `@end` line.
    pub body: ByteSpan,
    /// The complete `@end` line when it was found.
    pub terminator: Option<ByteSpan>,
    /// A safe append point for a new CSV record.  It is absent if recovery did
    /// not find an `@end` terminator.
    pub insertion: Option<ByteSpan>,
    /// The complete CSV construct.
    pub span: ByteSpan,
}

/// Source spans for an `@sheet` declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SheetLocation {
    /// The directive line.
    pub directive: DirectiveLocation,
    /// Stable sheet identifier token, if present.
    pub id: Option<ByteSpan>,
    /// JSON label token, if present.
    pub label: Option<ByteSpan>,
    /// A safe point at which another sheet-scoped item may be inserted.
    pub insertion: Option<ByteSpan>,
}

/// Source spans for an `@name` declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NameLocation {
    /// The directive line.
    pub directive: DirectiveLocation,
    /// Name identifier token, if the required separator was found.
    pub id: Option<ByteSpan>,
    /// Named target source, excluding the required ` = ` separator.
    pub target: Option<ByteSpan>,
}

/// Source spans for an `@style` declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StyleLocation {
    /// The directive line.
    pub directive: DirectiveLocation,
    /// Style identifier token, if present.
    pub id: Option<ByteSpan>,
}

/// Source spans for an `@fill` declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FillLocation {
    /// The directive line.
    pub directive: DirectiveLocation,
    /// Target source, excluding the separator before the formula.
    pub target: Option<ByteSpan>,
    /// Formula source, including its leading `=`.
    pub formula: Option<ByteSpan>,
}

/// Source spans for an `@apply` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyLocation {
    /// The directive line.
    pub directive: DirectiveLocation,
    /// Target source, excluding the following separator.
    pub target: Option<ByteSpan>,
    /// Individual style identifier tokens, in authored order.
    pub styles: Vec<ByteSpan>,
}

/// Exact source locations for an opaque `@extension` instance.
///
/// The payload is only located, never decoded or interpreted by this map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionLocation {
    /// The opening `@extension` directive line.
    pub directive: DirectiveLocation,
    /// Opaque payload bytes between the opening directive and `@end` (or EOF
    /// during recovery).
    pub payload: ByteSpan,
    /// The complete `@end` line when the scanner found one.
    pub terminator: Option<ByteSpan>,
    /// The complete extension construct.
    pub span: ByteSpan,
}

/// The concrete kind of non-semantic source trivia.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriviaKind {
    /// A line whose content starts with `#`.
    Comment,
    /// An empty physical line.
    Blank,
}

/// Exact source location for a comment or blank physical line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TriviaLocation {
    /// Whether this location is a comment or blank line.
    pub kind: TriviaKind,
    /// The complete physical line, including its original line ending.
    pub line: ByteSpan,
    /// The line content, excluding its original line ending.
    pub content: ByteSpan,
    /// The original CRLF, LF, bare CR, or empty EOF newline span.
    pub newline: ByteSpan,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct CellKey {
    sheet: SheetId,
    coordinate: Coordinate,
}

/// Source locations derived directly from a parsed CST.
///
/// Recovered or invalid documents may have only a partial map.  In particular,
/// a keyed lookup returns `None` for duplicate authored keys, because choosing
/// one declaration would be a guess.  The ordered location collections still
/// expose every syntactically recognized occurrence for diagnostics and repair
/// tooling.
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    directives: Vec<DirectiveLocation>,
    csv_blocks: Vec<CsvBlockLocation>,
    sheets: Vec<SheetLocation>,
    names: Vec<NameLocation>,
    styles: Vec<StyleLocation>,
    fills: Vec<FillLocation>,
    applies: Vec<ApplyLocation>,
    extensions: Vec<ExtensionLocation>,
    trivia: Vec<TriviaLocation>,
    cells: BTreeMap<CellKey, CellLocation>,
    sheets_by_id: BTreeMap<SheetId, SheetLocation>,
    names_by_id: BTreeMap<NameId, NameLocation>,
    styles_by_id: BTreeMap<StyleId, StyleLocation>,
    tables_by_id: BTreeMap<TableId, CsvBlockLocation>,
    top_level_insertion: Option<ByteSpan>,
}

impl SourceMap {
    /// Builds a map from the lossless CST.  This never requires a successfully
    /// lowered workbook and accepts source containing unrelated invalid UTF-8.
    #[must_use]
    pub fn from_cst(source: &[u8], cst: &Cst) -> Self {
        let mut builder = Builder::new(source, cst);
        builder.build();
        builder.map
    }

    /// Every recognized directive in source order, including CSV and extension
    /// directives.
    #[must_use]
    pub fn directives(&self) -> &[DirectiveLocation] {
        &self.directives
    }

    /// Every recognized CSV block or table in source order.
    #[must_use]
    pub fn csv_blocks(&self) -> &[CsvBlockLocation] {
        &self.csv_blocks
    }

    /// Every recognized sheet declaration in source order.
    #[must_use]
    pub fn sheets(&self) -> &[SheetLocation] {
        &self.sheets
    }

    /// Every recognized named-range declaration in source order.
    #[must_use]
    pub fn names(&self) -> &[NameLocation] {
        &self.names
    }

    /// Every recognized style declaration in source order.
    #[must_use]
    pub fn styles(&self) -> &[StyleLocation] {
        &self.styles
    }

    /// Every recognized fill declaration in source order.
    #[must_use]
    pub fn fills(&self) -> &[FillLocation] {
        &self.fills
    }

    /// Every recognized style application in source order.
    #[must_use]
    pub fn applies(&self) -> &[ApplyLocation] {
        &self.applies
    }

    /// Every opaque extension instance in source order.
    #[must_use]
    pub fn extensions(&self) -> &[ExtensionLocation] {
        &self.extensions
    }

    /// Every comment and blank physical line in source order.
    #[must_use]
    pub fn trivia(&self) -> &[TriviaLocation] {
        &self.trivia
    }

    /// Looks up an unambiguous authored CSV cell.
    #[must_use]
    pub fn cell(&self, sheet: &SheetId, coordinate: Coordinate) -> Option<CellLocation> {
        self.cells
            .get(&CellKey {
                sheet: sheet.clone(),
                coordinate,
            })
            .copied()
    }

    /// Looks up an unambiguous sheet declaration by stable identifier.
    #[must_use]
    pub fn sheet(&self, id: &SheetId) -> Option<SheetLocation> {
        self.sheets_by_id.get(id).copied()
    }

    /// Looks up an unambiguous named-range declaration by identifier.
    #[must_use]
    pub fn name(&self, id: &NameId) -> Option<NameLocation> {
        self.names_by_id.get(id).copied()
    }

    /// Looks up an unambiguous style declaration by identifier.
    #[must_use]
    pub fn style(&self, id: &StyleId) -> Option<StyleLocation> {
        self.styles_by_id.get(id).copied()
    }

    /// Looks up an unambiguous table declaration by identifier.
    #[must_use]
    pub fn table(&self, id: &TableId) -> Option<&CsvBlockLocation> {
        self.tables_by_id.get(id)
    }

    /// Returns a safe top-level insertion point, if the current CST proves one.
    #[must_use]
    pub fn top_level_insertion(&self) -> Option<ByteSpan> {
        self.top_level_insertion
    }

    /// Returns a safe sheet-scoped insertion point for an unambiguous sheet.
    #[must_use]
    pub fn sheet_insertion(&self, id: &SheetId) -> Option<ByteSpan> {
        self.sheet(id).and_then(|location| location.insertion)
    }
}

struct Builder<'a> {
    source: &'a [u8],
    cst: &'a Cst,
    map: SourceMap,
    current_sheet: Option<SheetId>,
    sheet_indexes: Vec<(SheetId, usize)>,
    ambiguous_cells: BTreeSet<CellKey>,
    ambiguous_sheets: BTreeSet<SheetId>,
    ambiguous_names: BTreeSet<NameId>,
    ambiguous_styles: BTreeSet<StyleId>,
    ambiguous_tables: BTreeSet<TableId>,
}

impl<'a> Builder<'a> {
    fn new(source: &'a [u8], cst: &'a Cst) -> Self {
        Self {
            source,
            cst,
            map: SourceMap::default(),
            current_sheet: None,
            sheet_indexes: Vec::new(),
            ambiguous_cells: BTreeSet::new(),
            ambiguous_sheets: BTreeSet::new(),
            ambiguous_names: BTreeSet::new(),
            ambiguous_styles: BTreeSet::new(),
            ambiguous_tables: BTreeSet::new(),
        }
    }

    fn build(&mut self) {
        for (index, node) in self.cst.nodes.iter().enumerate() {
            match node {
                Node::Directive(directive) => self.visit_directive(index, directive),
                Node::CsvBlock(block) => self.visit_csv_block(block),
                Node::Extension(extension) => self.visit_extension(extension),
                Node::Comment(line) => self.visit_trivia(TriviaKind::Comment, *line),
                Node::Blank(line) => self.visit_trivia(TriviaKind::Blank, *line),
                Node::Header(_) | Node::Recovery(_) => {}
            }
        }
        self.finish_insertions();
    }

    fn visit_directive(&mut self, index: usize, directive: &Directive) {
        let location = directive_location(directive);
        self.map.directives.push(location);
        match self.text(directive.name) {
            Some("sheet") => self.visit_sheet(index, directive, location),
            Some("name") => self.visit_name(directive, location),
            Some("style") => self.visit_style(directive, location),
            Some("fill") => self.visit_fill(directive, location),
            Some("apply") => self.visit_apply(directive, location),
            _ => {}
        }
    }

    fn visit_sheet(
        &mut self,
        index: usize,
        directive: &Directive,
        directive_location: DirectiveLocation,
    ) {
        let tokens = token_spans(self.source, directive.arguments);
        let location = SheetLocation {
            directive: directive_location,
            id: tokens.first().copied(),
            label: tokens.get(1).copied(),
            insertion: None,
        };
        self.map.sheets.push(location);

        let Some(id_span) = location.id else {
            self.current_sheet = None;
            return;
        };
        let Some(id) = self
            .text_byte_span(id_span)
            .and_then(|value| SheetId::parse(value).ok())
        else {
            self.current_sheet = None;
            return;
        };
        insert_unambiguous(
            &mut self.map.sheets_by_id,
            &mut self.ambiguous_sheets,
            id.clone(),
            location,
        );
        self.current_sheet = Some(id.clone());
        self.sheet_indexes.push((id, self.map.sheets.len() - 1));
        // `index` records the syntactic boundary, not the semantic validity of
        // the declaration.  The next recognized @sheet is always a safe point
        // to insert a sheet-scoped directive before it.
        debug_assert_eq!(
            self.cst.nodes[index].span().start,
            directive.line.span.start
        );
    }

    fn visit_name(&mut self, directive: &Directive, directive_location: DirectiveLocation) {
        let (id, target) = self.name_parts(directive.arguments);
        let location = NameLocation {
            directive: directive_location,
            id,
            target,
        };
        self.map.names.push(location);
        let Some(id) = id
            .and_then(|span| self.text_byte_span(span))
            .and_then(|value| NameId::parse(value).ok())
        else {
            return;
        };
        insert_unambiguous(
            &mut self.map.names_by_id,
            &mut self.ambiguous_names,
            id,
            location,
        );
    }

    fn visit_style(&mut self, directive: &Directive, directive_location: DirectiveLocation) {
        let location = StyleLocation {
            directive: directive_location,
            id: token_spans(self.source, directive.arguments)
                .first()
                .copied(),
        };
        self.map.styles.push(location);
        let Some(id) = location
            .id
            .and_then(|span| self.text_byte_span(span))
            .and_then(|value| StyleId::parse(value).ok())
        else {
            return;
        };
        insert_unambiguous(
            &mut self.map.styles_by_id,
            &mut self.ambiguous_styles,
            id,
            location,
        );
    }

    fn visit_fill(&mut self, directive: &Directive, directive_location: DirectiveLocation) {
        let (target, formula) = self.target_and_rest(directive.arguments);
        self.map.fills.push(FillLocation {
            directive: directive_location,
            target,
            formula,
        });
    }

    fn visit_apply(&mut self, directive: &Directive, directive_location: DirectiveLocation) {
        let (target, rest) = self.target_and_rest(directive.arguments);
        let styles = rest.map_or_else(Vec::new, |span| {
            span_from_byte_span(span).map_or_else(Vec::new, |span| token_spans(self.source, span))
        });
        self.map.applies.push(ApplyLocation {
            directive: directive_location,
            target,
            styles,
        });
    }

    fn visit_csv_block(&mut self, block: &CsvBlock) {
        let directive = directive_location(&block.directive);
        self.map.directives.push(directive);
        let tokens = token_spans(self.source, block.directive.arguments);
        let (table_id, anchor) = match block.kind {
            CsvKind::Block => (None, tokens.first().copied()),
            CsvKind::Table => (tokens.first().copied(), tokens.get(1).copied()),
        };
        let location = CsvBlockLocation {
            kind: block.kind,
            directive,
            table_id,
            anchor,
            body: byte_span(block.body),
            terminator: block.terminator.map(|line| byte_span(line.span)),
            insertion: block
                .terminator
                .map(|line| ByteSpan::empty(line.span.start as u64)),
            span: byte_span(block.span),
        };
        self.map.csv_blocks.push(location.clone());

        if let Some(table) = table_id
            .and_then(|span| self.text_byte_span(span))
            .and_then(|value| TableId::parse(value).ok())
        {
            insert_unambiguous(
                &mut self.map.tables_by_id,
                &mut self.ambiguous_tables,
                table,
                location,
            );
        }

        let Some(sheet) = self.current_sheet.clone() else {
            return;
        };
        let Some(anchor) = anchor
            .and_then(|span| self.text_byte_span(span))
            .and_then(|value| Coordinate::parse(value).ok())
        else {
            return;
        };

        for (row_offset, record) in block.records.iter().enumerate() {
            let Some(row_offset) = u64::try_from(row_offset).ok() else {
                break;
            };
            for (column_offset, field) in record.fields.iter().enumerate() {
                let Some(column_offset) = u64::try_from(column_offset).ok() else {
                    break;
                };
                let Ok(coordinate) = anchor.offset(column_offset, row_offset) else {
                    continue;
                };
                insert_unambiguous(
                    &mut self.map.cells,
                    &mut self.ambiguous_cells,
                    CellKey {
                        sheet: sheet.clone(),
                        coordinate,
                    },
                    CellLocation {
                        field: byte_span(field.span),
                        record: byte_span(record.span),
                        container: byte_span(block.span),
                    },
                );
            }
        }
    }

    fn visit_extension(&mut self, extension: &ExtensionBlock) {
        let directive = directive_location(&extension.directive);
        self.map.directives.push(directive);
        self.map.extensions.push(ExtensionLocation {
            directive,
            payload: byte_span(extension.payload),
            terminator: extension.terminator.map(|line| byte_span(line.span)),
            span: byte_span(extension.span),
        });
    }

    fn visit_trivia(&mut self, kind: TriviaKind, line: Line) {
        self.map.trivia.push(TriviaLocation {
            kind,
            line: byte_span(line.span),
            content: byte_span(line.content),
            newline: byte_span(line.newline),
        });
    }

    fn finish_insertions(&mut self) {
        let header_is_valid = self.cst.nodes.first().is_some_and(|node| match node {
            Node::Header(line) => self.text(line.content) == Some("#!marksheet 0.1"),
            _ => false,
        });
        if header_is_valid {
            self.map.top_level_insertion = self
                .map
                .sheets
                .first()
                .map(|location| ByteSpan::empty(location.directive.line.start))
                .or_else(|| {
                    self.safe_eof()
                        .then(|| ByteSpan::empty(self.source.len() as u64))
                });
        }

        for position in 0..self.sheet_indexes.len() {
            let (_, sheet_index) = self.sheet_indexes[position];
            let insertion = self
                .sheet_indexes
                .get(position + 1)
                .and_then(|(_, next_index)| {
                    self.map
                        .sheets
                        .get(*next_index)
                        .map(|location| ByteSpan::empty(location.directive.line.start))
                })
                .or_else(|| {
                    self.safe_eof()
                        .then(|| ByteSpan::empty(self.source.len() as u64))
                });
            if let Some(location) = self.map.sheets.get_mut(sheet_index) {
                location.insertion = insertion;
            }
            let id = self.sheet_indexes[position].0.clone();
            if !self.ambiguous_sheets.contains(&id) {
                if let Some(location) = self.map.sheets.get(sheet_index).copied() {
                    self.map.sheets_by_id.insert(id, location);
                }
            }
        }
    }

    fn safe_eof(&self) -> bool {
        self.source
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
            && !self.cst.nodes.iter().any(|node| {
                matches!(node, Node::CsvBlock(block) if block.terminator.is_none())
                    || matches!(node, Node::Extension(extension) if extension.terminator.is_none())
            })
    }

    fn text(&self, span: Span) -> Option<&str> {
        std::str::from_utf8(self.source.get(span.range())?).ok()
    }

    fn text_byte_span(&self, span: ByteSpan) -> Option<&str> {
        self.text(span_from_byte_span(span)?)
    }

    fn name_parts(&self, arguments: Span) -> (Option<ByteSpan>, Option<ByteSpan>) {
        let Some(value) = self.text(arguments) else {
            return (None, None);
        };
        let Some(separator) = value.find(" = ") else {
            return (None, None);
        };
        let id = (!value[..separator].is_empty()).then(|| ByteSpan {
            start: arguments.start as u64,
            end: (arguments.start + separator) as u64,
        });
        let target_start = arguments.start + separator + 3;
        let target = (target_start < arguments.end).then_some(ByteSpan {
            start: target_start as u64,
            end: arguments.end as u64,
        });
        (id, target)
    }

    fn target_and_rest(&self, arguments: Span) -> (Option<ByteSpan>, Option<ByteSpan>) {
        let Some(bytes) = self.source.get(arguments.range()) else {
            return (None, None);
        };
        let mut offset = 0;
        let mut in_brackets = false;
        while offset < bytes.len() {
            match bytes[offset] {
                b'[' => in_brackets = true,
                b']' if in_brackets && bytes.get(offset + 1) == Some(&b']') => offset += 1,
                b']' => in_brackets = false,
                b' ' if !in_brackets => {
                    let target = (offset > 0).then(|| ByteSpan {
                        start: arguments.start as u64,
                        end: (arguments.start + offset) as u64,
                    });
                    let mut rest_start = arguments.start + offset;
                    while rest_start < arguments.end && self.source[rest_start] == b' ' {
                        rest_start += 1;
                    }
                    let rest = (rest_start < arguments.end).then_some(ByteSpan {
                        start: rest_start as u64,
                        end: arguments.end as u64,
                    });
                    return (target, rest);
                }
                _ => {}
            }
            offset += 1;
        }
        (None, None)
    }
}

fn directive_location(directive: &Directive) -> DirectiveLocation {
    DirectiveLocation {
        line: byte_span(directive.line.span),
        name: byte_span(directive.name),
        arguments: byte_span(directive.arguments),
    }
}

fn byte_span(span: Span) -> ByteSpan {
    ByteSpan {
        start: span.start as u64,
        end: span.end as u64,
    }
}

fn span_from_byte_span(span: ByteSpan) -> Option<Span> {
    Some(Span::new(
        usize::try_from(span.start).ok()?,
        usize::try_from(span.end).ok()?,
    ))
}

/// Lexes outer directive tokens enough to preserve exact token spans.  It
/// deliberately mirrors the scanner/lowerer boundary rules without assigning
/// meaning to malformed token content.
fn token_spans(source: &[u8], span: Span) -> Vec<ByteSpan> {
    let Some(bytes) = source.get(span.range()) else {
        return Vec::new();
    };
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor] == b' ' {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let start = cursor;
        let mut in_string = false;
        let mut escaped = false;
        let mut brackets = 0_u32;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'"' if !escaped => in_string = !in_string,
                b'\\' if in_string => escaped = !escaped,
                b'[' if !in_string => brackets = brackets.saturating_add(1),
                b']' if !in_string && brackets > 0 => brackets -= 1,
                b' ' if !in_string && brackets == 0 => break,
                _ => escaped = false,
            }
            cursor += 1;
        }
        // Unterminated JSON text has no trustworthy token boundary.  Keep the
        // directive location but intentionally omit the partial argument map.
        if in_string {
            return Vec::new();
        }
        tokens.push(ByteSpan {
            start: (span.start + start) as u64,
            end: (span.start + cursor) as u64,
        });
    }
    tokens
}

fn insert_unambiguous<K, V>(map: &mut BTreeMap<K, V>, ambiguous: &mut BTreeSet<K>, key: K, value: V)
where
    K: Clone + Ord,
{
    if ambiguous.contains(&key) {
        return;
    }
    if map.insert(key.clone(), value).is_some() {
        map.remove(&key);
        ambiguous.insert(key);
    }
}
