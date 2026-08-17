//! Source-preserving formula reference rewrites.
//!
//! This module deliberately operates on the parsed reference nodes and lexer
//! token spans rather than formatting an AST again. Structural editing must not
//! unexpectedly normalize function case, whitespace, parentheses, text
//! literals, or unrelated identifiers. Every edit is therefore an exact byte
//! patch against the original formula source.

use std::{collections::BTreeMap, fmt};

use marksheet_model::{
    ByteSpan, Coordinate, FormulaSource, NameId, Range, ScalarParseError, SheetId, TableId,
};
use serde::{Deserialize, Serialize};

use super::{
    A1Reference, AdjustmentError, CopyOffset, Expr, ExprKind, FormulaError, ParseLimits, Reference,
    StructuredReference, Token, TokenKind, lex, parse,
};

/// A source-aware transformation that can be applied to a formula.
///
/// Renames are matched against typed parsed references, never arbitrary text.
/// [`FormulaRewrite::CopyA1`] implements conventional A1 copying with a fully
/// explicit source and destination cell. Insertion and movement of partial
/// areas deliberately are not represented here: their policies differ between
/// spreadsheet applications and a partial implementation would silently
/// corrupt references.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FormulaRewrite {
    RenameSheet { from: SheetId, to: SheetId },
    RenameName { from: NameId, to: NameId },
    RenameTable { from: TableId, to: TableId },
    CopyA1 { copy: A1Copy },
    MoveA1 { movement: A1Move },
}

/// The source and destination cell for conventional A1 copy adjustment.
///
/// Relative axes move by the coordinate difference; `$`-absolute axes remain
/// unchanged. This is intentionally distinct from inserting or moving an
/// arbitrary grid area, whose reference-adjustment policy is not yet public.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct A1Copy {
    pub origin: Coordinate,
    pub target: Coordinate,
}

impl A1Copy {
    #[must_use]
    pub fn offset(self) -> CopyOffset {
        CopyOffset::between(self.origin, self.target)
    }
}

/// Formula context and geometry needed to apply the complete-footprint move
/// policy in SPEC section 19.1.3.
///
/// `source` is the exact footprint being moved on `moved_sheet`; `destination`
/// is its new top-left cell. `formula_sheet` resolves unqualified A1 syntax.
/// `formula_origin` is the formula cell's authored coordinate when it is a
/// normal cell formula. It is absent for a direct named-range target, where
/// only explicit or context-resolved references into the moved footprint move.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct A1Move {
    pub moved_sheet: SheetId,
    pub source: Range,
    pub destination: Coordinate,
    pub formula_sheet: SheetId,
    pub formula_origin: Option<Coordinate>,
}

impl A1Move {
    fn displacement(&self) -> Result<CopyOffset, FormulaRewriteError> {
        validate_range(self.source)?;
        validate_coordinate(self.destination)?;
        // Validate the whole moved rectangle, not merely individual references
        // encountered in this formula. A structural action is all-or-nothing.
        shift_coordinate(
            self.source.end,
            CopyOffset::between(self.source.start, self.destination),
        )?;
        if let Some(origin) = self.formula_origin {
            validate_coordinate(origin)?;
        }
        Ok(CopyOffset::between(self.source.start, self.destination))
    }

    fn formula_moves(&self) -> bool {
        self.formula_origin.is_some_and(|origin| {
            self.formula_sheet == self.moved_sheet && self.source.contains(origin)
        })
    }
}

/// One exact replacement made against the original complete formula source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FormulaPatch {
    /// Half-open byte span in the original formula source.
    pub span: ByteSpan,
    /// Replacement spelling. It has not been canonicalized.
    pub replacement: String,
}

/// A rewritten source value and its auditable byte patches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FormulaRewriteResult {
    pub source: FormulaSource,
    pub patches: Vec<FormulaPatch>,
}

/// A failure while planning or validating an atomic formula rewrite.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormulaRewriteError {
    /// The original formula is not valid and therefore has no trustworthy
    /// reference token stream to rewrite.
    InvalidSource(FormulaError),
    /// Two rename requests target the same original typed identifier with
    /// different destinations. Applying either would make the batch orderful.
    ConflictingRename {
        kind: RenameKind,
        from: String,
        first_to: String,
        second_to: String,
    },
    /// Copying has a single well-defined displacement. Multiple copy requests
    /// in one batch are ambiguous and rejected rather than compounded.
    MultipleA1Copies,
    /// A relative A1 axis would become zero or exceed the coordinate domain.
    A1Adjustment(AdjustmentError),
    /// The moved footprint, destination, or formula context contains an
    /// invalid public coordinate or cannot fit after translation.
    InvalidMoveGeometry { message: String },
    /// Independently rewritten range endpoints no longer retain their original
    /// ordering on at least one axis, so the move must not be persisted.
    RangeOrderInverted {
        original_start: Coordinate,
        original_end: Coordinate,
        rewritten_start: Coordinate,
        rewritten_end: Coordinate,
    },
    /// A batch cannot combine conventional formula copying with a structural
    /// footprint move because their coordinate policies are not composable.
    MultipleA1Transforms,
    /// Internal span planning found an impossible overlapping edit. Exposing
    /// this distinctly prevents an editor from accepting a partial rewrite.
    OverlappingPatches { first: ByteSpan, second: ByteSpan },
    /// The patch result did not parse. This is defensive: plans are built from
    /// parsed tokens, but the final check makes rewrites transactional.
    InvalidOutput(FormulaError),
    /// Construction of the source model unexpectedly failed after validation.
    /// This protects the transactional boundary without relying on a panic.
    InvalidModelSource(ScalarParseError),
}

/// The identifier namespace for a conflicting rename request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameKind {
    Sheet,
    Name,
    Table,
}

impl fmt::Display for FormulaRewriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSource(error) => {
                write!(formatter, "cannot rewrite invalid formula: {error}")
            }
            Self::ConflictingRename {
                kind,
                from,
                first_to,
                second_to,
            } => write!(
                formatter,
                "conflicting {kind} renames for {from:?}: {first_to:?} and {second_to:?}"
            ),
            Self::MultipleA1Copies => {
                formatter.write_str("a formula rewrite batch may contain at most one A1 copy")
            }
            Self::A1Adjustment(error) => error.fmt(formatter),
            Self::InvalidMoveGeometry { message } => {
                write!(formatter, "invalid A1 move geometry: {message}")
            }
            Self::RangeOrderInverted {
                original_start,
                original_end,
                rewritten_start,
                rewritten_end,
            } => write!(
                formatter,
                "moving range endpoints {original_start}:{original_end} would invert their order as {rewritten_start}:{rewritten_end}"
            ),
            Self::MultipleA1Transforms => formatter.write_str(
                "a formula rewrite batch cannot combine A1 copy and footprint move transforms",
            ),
            Self::OverlappingPatches { first, second } => write!(
                formatter,
                "formula rewrite patches overlap at bytes {}..{} and {}..{}",
                first.start, first.end, second.start, second.end
            ),
            Self::InvalidOutput(error) => write!(
                formatter,
                "formula rewrite produced invalid syntax: {error}"
            ),
            Self::InvalidModelSource(error) => {
                write!(
                    formatter,
                    "formula rewrite produced an invalid source model: {error}"
                )
            }
        }
    }
}

impl fmt::Display for RenameKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Sheet => "sheet",
            Self::Name => "workbook name",
            Self::Table => "table",
        })
    }
}

impl std::error::Error for FormulaRewriteError {}

/// Rewrites a [`FormulaSource`] while preserving every unmodified source byte.
///
/// The input is parsed before patches are planned and the result is parsed
/// again after the patches have been applied. Consequently either the entire
/// set of changes is returned or the caller receives no output to persist.
///
/// # Errors
///
/// Returns [`FormulaRewriteError`] for invalid input, ambiguous transformation
/// batches, out-of-bounds A1 copy adjustments, or a failed output validation.
pub fn rewrite_formula(
    source: &FormulaSource,
    rewrites: &[FormulaRewrite],
) -> Result<FormulaRewriteResult, FormulaRewriteError> {
    rewrite_formula_text(source.as_str(), rewrites)
}

/// Rewrites complete formula text while preserving every unmodified source
/// byte. This is the `&str` counterpart to [`rewrite_formula`].
///
/// # Errors
///
/// Returns [`FormulaRewriteError`] under the same conditions as
/// [`rewrite_formula`].
pub fn rewrite_formula_text(
    source: &str,
    rewrites: &[FormulaRewrite],
) -> Result<FormulaRewriteResult, FormulaRewriteError> {
    let formula =
        parse(source, &ParseLimits::default()).map_err(FormulaRewriteError::InvalidSource)?;
    let tokens = lex(source).map_err(FormulaRewriteError::InvalidSource)?;
    let plan = RewritePlan::from_rewrites(rewrites)?;
    let mut patches = PatchSet::default();
    collect_expression_patches(source, &formula.expression, &tokens, &plan, &mut patches)?;
    let patches = patches.into_sorted();
    let rewritten = apply_patches(source, &patches)?;
    parse(&rewritten, &ParseLimits::default()).map_err(FormulaRewriteError::InvalidOutput)?;
    let source = FormulaSource::new(rewritten).map_err(FormulaRewriteError::InvalidModelSource)?;
    Ok(FormulaRewriteResult { source, patches })
}

#[derive(Default)]
struct RewritePlan {
    sheets: BTreeMap<SheetId, SheetId>,
    names: BTreeMap<NameId, NameId>,
    tables: BTreeMap<TableId, TableId>,
    a1_transform: Option<A1Transform>,
}

#[derive(Clone, Debug)]
enum A1Transform {
    Copy(CopyOffset),
    Move(A1Move),
}

impl RewritePlan {
    fn from_rewrites(rewrites: &[FormulaRewrite]) -> Result<Self, FormulaRewriteError> {
        let mut plan = Self::default();
        for rewrite in rewrites {
            match rewrite {
                FormulaRewrite::RenameSheet { from, to } => {
                    insert_rename(&mut plan.sheets, from, to, RenameKind::Sheet)?;
                }
                FormulaRewrite::RenameName { from, to } => {
                    insert_rename(&mut plan.names, from, to, RenameKind::Name)?;
                }
                FormulaRewrite::RenameTable { from, to } => {
                    insert_rename(&mut plan.tables, from, to, RenameKind::Table)?;
                }
                FormulaRewrite::CopyA1 { copy } => {
                    plan.insert_a1_transform(A1Transform::Copy(copy.offset()))?;
                }
                FormulaRewrite::MoveA1 { movement } => {
                    movement.displacement()?;
                    plan.insert_a1_transform(A1Transform::Move(movement.clone()))?;
                }
            }
        }
        Ok(plan)
    }

    fn insert_a1_transform(&mut self, transform: A1Transform) -> Result<(), FormulaRewriteError> {
        if let Some(existing) = &self.a1_transform {
            return Err(
                if matches!(existing, A1Transform::Copy(_))
                    && matches!(transform, A1Transform::Copy(_))
                {
                    FormulaRewriteError::MultipleA1Copies
                } else {
                    FormulaRewriteError::MultipleA1Transforms
                },
            );
        }
        self.a1_transform = Some(transform);
        Ok(())
    }
}

fn insert_rename<Id>(
    map: &mut BTreeMap<Id, Id>,
    from: &Id,
    to: &Id,
    kind: RenameKind,
) -> Result<(), FormulaRewriteError>
where
    Id: Clone + Ord + fmt::Display,
{
    if let Some(previous) = map.get(from) {
        if previous != to {
            return Err(FormulaRewriteError::ConflictingRename {
                kind,
                from: from.to_string(),
                first_to: previous.to_string(),
                second_to: to.to_string(),
            });
        }
        return Ok(());
    }
    map.insert(from.clone(), to.clone());
    Ok(())
}

#[derive(Default)]
struct PatchSet(BTreeMap<ByteSpan, String>);

impl PatchSet {
    fn insert(&mut self, patch: FormulaPatch) -> Result<(), FormulaRewriteError> {
        if patch.span.is_empty() {
            return Ok(());
        }
        if let Some(existing) = self.0.get(&patch.span) {
            if existing == &patch.replacement {
                return Ok(());
            }
            return Err(FormulaRewriteError::OverlappingPatches {
                first: patch.span,
                second: patch.span,
            });
        }
        if let Some((previous, _)) = self.0.range(..patch.span).next_back() {
            if previous.end > patch.span.start {
                return Err(FormulaRewriteError::OverlappingPatches {
                    first: *previous,
                    second: patch.span,
                });
            }
        }
        if let Some((next, _)) = self.0.range(patch.span..).next() {
            if patch.span.end > next.start {
                return Err(FormulaRewriteError::OverlappingPatches {
                    first: patch.span,
                    second: *next,
                });
            }
        }
        self.0.insert(patch.span, patch.replacement);
        Ok(())
    }

    fn into_sorted(self) -> Vec<FormulaPatch> {
        self.0
            .into_iter()
            .map(|(span, replacement)| FormulaPatch { span, replacement })
            .collect()
    }
}

fn collect_expression_patches(
    source: &str,
    expression: &Expr,
    tokens: &[Token],
    plan: &RewritePlan,
    patches: &mut PatchSet,
) -> Result<(), FormulaRewriteError> {
    match &expression.kind {
        ExprKind::Literal { .. } => {}
        ExprKind::Reference { reference } => {
            collect_reference_patches(source, expression.span, reference, tokens, plan, patches)?;
        }
        ExprKind::Unary { operand, .. } => {
            collect_expression_patches(source, operand, tokens, plan, patches)?;
        }
        ExprKind::Binary { left, right, .. } => {
            collect_expression_patches(source, left, tokens, plan, patches)?;
            collect_expression_patches(source, right, tokens, plan, patches)?;
        }
        ExprKind::Call { call } => {
            for argument in &call.arguments {
                collect_expression_patches(source, argument, tokens, plan, patches)?;
            }
        }
    }
    Ok(())
}

fn collect_reference_patches(
    source: &str,
    span: ByteSpan,
    reference: &Reference,
    tokens: &[Token],
    plan: &RewritePlan,
    patches: &mut PatchSet,
) -> Result<(), FormulaRewriteError> {
    match reference {
        Reference::Cell { sheet, address } => {
            rename_qualifier(source, span, sheet.as_ref(), &plan.sheets, tokens, patches)?;
            if let Some(transform) = &plan.a1_transform {
                rewrite_a1s(
                    source,
                    span,
                    std::slice::from_ref(address),
                    sheet.as_ref(),
                    transform,
                    tokens,
                    patches,
                )?;
            }
        }
        Reference::Range(range) => {
            rename_qualifier(
                source,
                span,
                range.sheet.as_ref(),
                &plan.sheets,
                tokens,
                patches,
            )?;
            if let Some(transform) = &plan.a1_transform {
                rewrite_a1s(
                    source,
                    span,
                    &[range.start.clone(), range.end.clone()],
                    range.sheet.as_ref(),
                    transform,
                    tokens,
                    patches,
                )?;
            }
        }
        Reference::Name { name } => {
            if let Some(replacement) = plan.names.get(name) {
                insert_changed_patch(
                    source,
                    patches,
                    FormulaPatch {
                        span,
                        replacement: replacement.as_str().to_owned(),
                    },
                )?;
            }
        }
        Reference::Structured(structured) => {
            if let Some(table) = structured_table(structured) {
                if let Some(replacement) = plan.tables.get(table) {
                    let token = first_token_at(span.start, tokens)
                        .expect("parsed structured references start at their table token");
                    debug_assert!(matches!(token.kind, TokenKind::Word(_)));
                    insert_changed_patch(
                        source,
                        patches,
                        FormulaPatch {
                            span: token.span,
                            replacement: replacement.as_str().to_owned(),
                        },
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn rename_qualifier(
    source: &str,
    reference_span: ByteSpan,
    sheet: Option<&SheetId>,
    renames: &BTreeMap<SheetId, SheetId>,
    tokens: &[Token],
    patches: &mut PatchSet,
) -> Result<(), FormulaRewriteError> {
    let Some(sheet) = sheet else {
        return Ok(());
    };
    let Some(replacement) = renames.get(sheet) else {
        return Ok(());
    };
    let token = first_token_at(reference_span.start, tokens)
        .expect("parsed qualified references start at their sheet token");
    debug_assert!(matches!(token.kind, TokenKind::Word(_)));
    insert_changed_patch(
        source,
        patches,
        FormulaPatch {
            span: token.span,
            replacement: replacement.as_str().to_owned(),
        },
    )
}

fn structured_table(reference: &StructuredReference) -> Option<&TableId> {
    match reference {
        StructuredReference::Column { table, .. } | StructuredReference::Region { table, .. } => {
            Some(table)
        }
        StructuredReference::CurrentRow { table, .. } => table.as_ref(),
    }
}

fn rewrite_a1s(
    source: &str,
    reference_span: ByteSpan,
    addresses: &[A1Reference],
    reference_sheet: Option<&SheetId>,
    transform: &A1Transform,
    tokens: &[Token],
    patches: &mut PatchSet,
) -> Result<(), FormulaRewriteError> {
    let cells = tokens_in_span(reference_span, tokens)
        .filter_map(|token| match &token.kind {
            TokenKind::Cell(address) => Some((token, address)),
            _ => None,
        })
        .collect::<Vec<_>>();
    debug_assert_eq!(cells.len(), addresses.len());
    let adjusted = addresses
        .iter()
        .map(|address| transform_a1(address, reference_sheet, transform))
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(transform, A1Transform::Move(_)) {
        if let ([original_start, original_end], [rewritten_start, rewritten_end]) =
            (addresses, adjusted.as_slice())
        {
            ensure_range_order(
                original_start.coordinate,
                original_end.coordinate,
                rewritten_start.coordinate,
                rewritten_end.coordinate,
            )?;
        }
    }
    for ((token, _parsed), adjusted) in cells.into_iter().zip(&adjusted) {
        insert_changed_patch(
            source,
            patches,
            FormulaPatch {
                span: token.span,
                replacement: render_source_preserving_a1(adjusted, source, token),
            },
        )?;
    }
    Ok(())
}

fn transform_a1(
    address: &A1Reference,
    reference_sheet: Option<&SheetId>,
    transform: &A1Transform,
) -> Result<A1Reference, FormulaRewriteError> {
    match transform {
        A1Transform::Copy(offset) => {
            adjust_a1(address, *offset).map_err(FormulaRewriteError::A1Adjustment)
        }
        A1Transform::Move(movement) => move_a1(address, reference_sheet, movement),
    }
}

fn move_a1(
    address: &A1Reference,
    reference_sheet: Option<&SheetId>,
    movement: &A1Move,
) -> Result<A1Reference, FormulaRewriteError> {
    let offset = movement.displacement()?;
    let resolved_sheet = reference_sheet.unwrap_or(&movement.formula_sheet);
    let coordinate = if resolved_sheet == &movement.moved_sheet
        && movement.source.contains(address.coordinate)
    {
        // The endpoint denotes an identity that moved. `$` does not pin an
        // identity to its old coordinate, so both axes translate exactly.
        shift_coordinate(address.coordinate, offset)?
    } else if movement.formula_moves() {
        // The formula moved but this endpoint did not. Conventional relative
        // A1 copying applies only to the endpoint axes without `$` markers.
        adjust_a1(address, offset)
            .map_err(FormulaRewriteError::A1Adjustment)?
            .coordinate
    } else {
        address.coordinate
    };
    Ok(A1Reference {
        coordinate,
        column_absolute: address.column_absolute,
        row_absolute: address.row_absolute,
    })
}

fn ensure_range_order(
    original_start: Coordinate,
    original_end: Coordinate,
    rewritten_start: Coordinate,
    rewritten_end: Coordinate,
) -> Result<(), FormulaRewriteError> {
    let preserves_columns = if original_start.column <= original_end.column {
        rewritten_start.column <= rewritten_end.column
    } else {
        rewritten_start.column >= rewritten_end.column
    };
    let preserves_rows = if original_start.row <= original_end.row {
        rewritten_start.row <= rewritten_end.row
    } else {
        rewritten_start.row >= rewritten_end.row
    };
    if preserves_columns && preserves_rows {
        return Ok(());
    }
    Err(FormulaRewriteError::RangeOrderInverted {
        original_start,
        original_end,
        rewritten_start,
        rewritten_end,
    })
}

fn insert_changed_patch(
    source: &str,
    patches: &mut PatchSet,
    patch: FormulaPatch,
) -> Result<(), FormulaRewriteError> {
    let existing = source_at(source, patch.span)
        .expect("lexer token spans always identify valid UTF-8 source boundaries");
    if existing == patch.replacement {
        return Ok(());
    }
    patches.insert(patch)
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
    let coordinate =
        Coordinate::new(column, row).map_err(|_| AdjustmentError::ColumnOutOfBounds)?;
    Ok(A1Reference {
        coordinate,
        column_absolute: reference.column_absolute,
        row_absolute: reference.row_absolute,
    })
}

fn adjust_axis(value: u64, delta: i128) -> Option<u64> {
    if delta >= 0 {
        value.checked_add(u64::try_from(delta).ok()?)
    } else {
        value
            .checked_sub(u64::try_from(delta.unsigned_abs()).ok()?)
            .filter(|adjusted| *adjusted > 0)
    }
}

fn validate_coordinate(coordinate: Coordinate) -> Result<(), FormulaRewriteError> {
    if coordinate.column == 0 || coordinate.row == 0 {
        return Err(FormulaRewriteError::InvalidMoveGeometry {
            message: format!("coordinate {coordinate:?} must use one-based axes"),
        });
    }
    Ok(())
}

fn validate_range(range: Range) -> Result<(), FormulaRewriteError> {
    validate_coordinate(range.start)?;
    validate_coordinate(range.end)?;
    if range.start.column > range.end.column || range.start.row > range.end.row {
        return Err(FormulaRewriteError::InvalidMoveGeometry {
            message: format!("source range {range:?} is inverted"),
        });
    }
    Ok(())
}

fn shift_coordinate(
    coordinate: Coordinate,
    offset: CopyOffset,
) -> Result<Coordinate, FormulaRewriteError> {
    let column = adjust_axis(coordinate.column, offset.columns).ok_or_else(|| {
        FormulaRewriteError::InvalidMoveGeometry {
            message: format!(
                "moving coordinate {coordinate} by {} columns leaves the coordinate domain",
                offset.columns
            ),
        }
    })?;
    let row = adjust_axis(coordinate.row, offset.rows).ok_or_else(|| {
        FormulaRewriteError::InvalidMoveGeometry {
            message: format!(
                "moving coordinate {coordinate} by {} rows leaves the coordinate domain",
                offset.rows
            ),
        }
    })?;
    Coordinate::new(column, row).map_err(|error| FormulaRewriteError::InvalidMoveGeometry {
        message: format!("moving coordinate {coordinate} produced an invalid coordinate: {error}"),
    })
}

fn render_source_preserving_a1(reference: &A1Reference, source: &str, token: &Token) -> String {
    let TokenKind::Cell(_) = token.kind else {
        unreachable!("only A1 cell tokens are rendered as A1 references");
    };
    // Accepted input may use lowercase A1 columns even though canonical output
    // uses uppercase. Retain all-lowercase spelling during a source edit.
    let spelling = source_at(source, token.span)
        .expect("lexer token spans always identify valid UTF-8 source boundaries");
    let column_letters = spelling
        .bytes()
        .skip_while(|byte| *byte == b'$')
        .take_while(u8::is_ascii_alphabetic)
        .collect::<Vec<_>>();
    let has_lowercase_column =
        !column_letters.is_empty() && column_letters.iter().all(u8::is_ascii_lowercase);
    let mut column = reference.coordinate.column_name();
    if has_lowercase_column {
        column.make_ascii_lowercase();
    }
    let mut rendered = String::new();
    if reference.column_absolute {
        rendered.push('$');
    }
    rendered.push_str(&column);
    if reference.row_absolute {
        rendered.push('$');
    }
    rendered.push_str(&reference.coordinate.row.to_string());
    rendered
}

fn source_at(source: &str, span: ByteSpan) -> Option<&str> {
    let start = usize::try_from(span.start).ok()?;
    let end = usize::try_from(span.end).ok()?;
    source.get(start..end)
}

fn first_token_at(offset: u64, tokens: &[Token]) -> Option<&Token> {
    tokens.iter().find(|token| token.span.start == offset)
}

fn tokens_in_span(span: ByteSpan, tokens: &[Token]) -> impl Iterator<Item = &Token> {
    tokens.iter().filter(move |token| {
        span.contains_span(token.span) && !matches!(token.kind, TokenKind::End)
    })
}

fn apply_patches(source: &str, patches: &[FormulaPatch]) -> Result<String, FormulaRewriteError> {
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for patch in patches {
        let start = usize::try_from(patch.span.start).expect("source byte span fits usize");
        let end = usize::try_from(patch.span.end).expect("source byte span fits usize");
        if start < cursor {
            return Err(FormulaRewriteError::OverlappingPatches {
                first: ByteSpan {
                    start: u64::try_from(cursor).expect("source length fits u64"),
                    end: u64::try_from(cursor).expect("source length fits u64"),
                },
                second: patch.span,
            });
        }
        output.push_str(&source[cursor..start]);
        output.push_str(&patch.replacement);
        cursor = end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use marksheet_model::{FormulaSource, NameId, SheetId, TableId};

    use super::*;

    fn source(value: &str) -> FormulaSource {
        FormulaSource::new(value).expect("formula source")
    }

    fn sheet(value: &str) -> SheetId {
        SheetId::parse(value).expect("sheet id")
    }

    fn name(value: &str) -> NameId {
        NameId::parse(value).expect("name id")
    }

    fn table(value: &str) -> TableId {
        TableId::parse(value).expect("table id")
    }

    fn movement(
        source: &str,
        destination: &str,
        formula_sheet: &str,
        formula_origin: Option<&str>,
    ) -> FormulaRewrite {
        FormulaRewrite::MoveA1 {
            movement: A1Move {
                moved_sheet: sheet("data"),
                source: Range::parse(source).expect("source range"),
                destination: Coordinate::parse(destination).expect("destination"),
                formula_sheet: sheet(formula_sheet),
                formula_origin: formula_origin
                    .map(|value| Coordinate::parse(value).expect("origin")),
            },
        }
    }

    #[test]
    fn renames_only_typed_references_and_preserves_source_spelling() {
        let result = rewrite_formula(
            &source("=sum( inputs!A1 , tax_rate , costs[Cost], \"inputs tax_rate costs\" )"),
            &[
                FormulaRewrite::RenameSheet {
                    from: sheet("inputs"),
                    to: sheet("source_data"),
                },
                FormulaRewrite::RenameName {
                    from: name("tax_rate"),
                    to: name("vat_rate"),
                },
                FormulaRewrite::RenameTable {
                    from: table("costs"),
                    to: table("expenses"),
                },
            ],
        )
        .expect("rewritten formula");

        assert_eq!(
            result.source.as_str(),
            "=sum( source_data!A1 , vat_rate , expenses[Cost], \"inputs tax_rate costs\" )"
        );
        assert_eq!(result.patches.len(), 3);
    }

    #[test]
    fn sheet_rename_rewrites_each_qualified_cell_and_range() {
        let result = rewrite_formula_text(
            "=inputs!A1+inputs!B2:C3+unrelated",
            &[FormulaRewrite::RenameSheet {
                from: sheet("inputs"),
                to: sheet("raw"),
            }],
        )
        .expect("rewritten formula");

        assert_eq!(result.source.as_str(), "=raw!A1+raw!B2:C3+unrelated");
        assert_eq!(result.patches.len(), 2);
    }

    #[test]
    fn table_rename_covers_current_row_and_does_not_touch_headers() {
        let result = rewrite_formula_text(
            "=costs[@Cost]+costs[Cost]+costs[#Data]+[@Cost]",
            &[FormulaRewrite::RenameTable {
                from: table("costs"),
                to: table("expenses"),
            }],
        )
        .expect("rewritten formula");

        assert_eq!(
            result.source.as_str(),
            "=expenses[@Cost]+expenses[Cost]+expenses[#Data]+[@Cost]"
        );
        assert_eq!(result.patches.len(), 3);
    }

    #[test]
    fn copy_adjusts_only_a1_references_and_retains_lowercase_columns() {
        let result = rewrite_formula_text(
            "=a1()+a1+$B2+c$3+$D$4+inputs!E5:F6+\"G7\"",
            &[FormulaRewrite::CopyA1 {
                copy: A1Copy {
                    origin: Coordinate::parse("B2").expect("coordinate"),
                    target: Coordinate::parse("D5").expect("coordinate"),
                },
            }],
        )
        .expect("rewritten formula");

        assert_eq!(
            result.source.as_str(),
            "=a1()+c4+$B5+e$3+$D$4+inputs!G8:H9+\"G7\""
        );
        assert_eq!(result.patches.len(), 5);
    }

    #[test]
    fn copy_underflow_is_atomic() {
        let error = rewrite_formula_text(
            "=A1+B2",
            &[FormulaRewrite::CopyA1 {
                copy: A1Copy {
                    origin: Coordinate::parse("B2").expect("coordinate"),
                    target: Coordinate::parse("A1").expect("coordinate"),
                },
            }],
        )
        .expect_err("A1 underflows");
        assert_eq!(
            error,
            FormulaRewriteError::A1Adjustment(AdjustmentError::ColumnOutOfBounds)
        );
    }

    #[test]
    fn copy_uses_uppercase_for_mixed_case_a1_columns() {
        let result = rewrite_formula_text(
            "=aB1",
            &[FormulaRewrite::CopyA1 {
                copy: A1Copy {
                    origin: Coordinate::parse("B2").expect("coordinate"),
                    target: Coordinate::parse("C3").expect("coordinate"),
                },
            }],
        )
        .expect("rewritten formula");

        // A source-preserving edit retains an all-lowercase column, but mixed
        // input has no stable casing policy. It uses the documented uppercase
        // spelling rather than silently lowercasing the entire column.
        assert_eq!(result.source.as_str(), "=AC2");
    }

    #[test]
    fn conflicting_rename_batch_is_rejected_before_any_patch() {
        let error = rewrite_formula_text(
            "=tax_rate",
            &[
                FormulaRewrite::RenameName {
                    from: name("tax_rate"),
                    to: name("vat_rate"),
                },
                FormulaRewrite::RenameName {
                    from: name("tax_rate"),
                    to: name("sales_tax"),
                },
            ],
        )
        .expect_err("conflict");
        assert!(matches!(
            error,
            FormulaRewriteError::ConflictingRename { .. }
        ));
    }

    #[test]
    fn moving_formula_retargets_inside_endpoints_and_adjusts_outside_relative_axes() {
        let result = rewrite_formula_text(
            "=B2+$C$3+A1+$D1",
            &[movement("B2:C3", "E5", "data", Some("B2"))],
        )
        .expect("rewritten formula");

        // B2 and C3 are moved identities, so their `$` markers do not stop
        // retargeting. A1 and D1 are outside the footprint, so only their
        // relative axes follow the formula from B2 to E5.
        assert_eq!(result.source.as_str(), "=E5+$F$6+D4+$D4");
    }

    #[test]
    fn moving_block_retargets_references_from_outside_without_copying_them() {
        let result = rewrite_formula_text(
            "=data!B2+data!A1",
            &[movement("B2:C3", "E5", "report", Some("A1"))],
        )
        .expect("rewritten formula");

        assert_eq!(result.source.as_str(), "=data!E5+data!A1");
    }

    #[test]
    fn move_resolves_unqualified_and_qualified_references_in_named_targets() {
        let result = rewrite_formula_text(
            "=data!B2+B3+other!B2",
            &[movement("B2:C3", "E5", "data", None)],
        )
        .expect("rewritten named target");

        assert_eq!(result.source.as_str(), "=data!E5+E6+other!B2");
    }

    #[test]
    fn move_does_not_double_shift_an_inside_endpoint_in_a_moved_formula() {
        let result = rewrite_formula_text("=B2", &[movement("B2:C3", "E5", "data", Some("B2"))])
            .expect("rewritten formula");

        assert_eq!(result.source.as_str(), "=E5");
    }

    #[test]
    fn move_rewrites_range_endpoints_independently_without_reordering_them() {
        let result = rewrite_formula_text(
            "=data!B2:C3",
            &[movement("B2:C3", "E5", "report", Some("A1"))],
        )
        .expect("rewritten range");

        assert_eq!(result.source.as_str(), "=data!E5:F6");
    }

    #[test]
    fn move_retains_authored_reverse_range_order() {
        let result = rewrite_formula_text(
            "=data!C3:B2",
            &[movement("B2:C3", "E5", "report", Some("A1"))],
        )
        .expect("rewritten reverse range");

        assert_eq!(result.source.as_str(), "=data!F6:E5");
    }

    #[test]
    fn move_rejects_a_range_endpoint_inversion_atomically() {
        let error =
            rewrite_formula_text("=data!A1:B1", &[movement("A1", "C1", "report", Some("A2"))])
                .expect_err("rewritten range would invert");

        assert!(matches!(
            error,
            FormulaRewriteError::RangeOrderInverted { .. }
        ));
    }

    #[test]
    fn move_rejects_destination_overflow_before_emitting_patches() {
        let movement = FormulaRewrite::MoveA1 {
            movement: A1Move {
                moved_sheet: sheet("data"),
                source: Range {
                    start: Coordinate {
                        column: u64::MAX - 1,
                        row: 1,
                    },
                    end: Coordinate {
                        column: u64::MAX,
                        row: 1,
                    },
                },
                destination: Coordinate {
                    column: u64::MAX,
                    row: 1,
                },
                formula_sheet: sheet("report"),
                formula_origin: None,
            },
        };
        let error = rewrite_formula_text("=A1", &[movement]).expect_err("destination overflows");
        assert!(matches!(
            error,
            FormulaRewriteError::InvalidMoveGeometry { .. }
        ));
    }
}
