//! Ordered, byte-oriented source patches.
//!
//! Patches deliberately operate on bytes rather than `str` character
//! boundaries. Marksheet's parser supplies byte spans, and preserving that
//! coordinate system lets an editor safely preserve files containing arbitrary
//! bytes (for example, a future non-UTF-8 import path). Callers that edit UTF-8
//! text remain responsible for choosing scalar-boundary spans.

use std::{fmt, sync::Arc};

use marksheet_model::ByteSpan;

/// Replaces the bytes in `span` with `replacement`.
///
/// An empty `span` is an insertion. Patches always refer to offsets in the
/// original source, never offsets after an earlier patch has been applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePatch {
    /// Half-open byte span in the original source.
    pub span: ByteSpan,
    /// Bytes that replace `span`.
    pub replacement: Vec<u8>,
}

impl SourcePatch {
    /// Creates a patch from a span and replacement bytes.
    #[must_use]
    pub fn new(span: ByteSpan, replacement: impl Into<Vec<u8>>) -> Self {
        Self {
            span,
            replacement: replacement.into(),
        }
    }
}

/// A validated collection of patches against one exact source snapshot.
///
/// Patches are stored and exposed in ascending source order. At one offset an
/// insertion comes before a replacement beginning at that offset; this makes
/// `insert + replace` deterministic. An insertion at the end of a preceding
/// replacement is also allowed. Multiple insertions at exactly the same offset
/// are rejected rather than relying on incidental input order; callers should
/// coalesce them into one patch first.
///
/// The bound snapshot is reference-counted. Byte identity remains the
/// precondition for applying a set, but successive document versions are shared
/// rather than copied, so an undo history costs one snapshot per version rather
/// than one per retained patch set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchSet {
    base: Arc<Vec<u8>>,
    patches: Vec<SourcePatch>,
}

impl PatchSet {
    /// Validates ordered patches against and binds them to `base`.
    ///
    /// The input order is preserved. This intentionally does not sort patches:
    /// accepting unsorted input would make overlapping edit intent easy to
    /// conceal and would make error reporting less useful to API callers.
    ///
    /// # Errors
    ///
    /// Returns a [`PatchError`] for malformed spans, offsets outside the base,
    /// non-ascending input, overlapping spans, or ambiguous insertions.
    pub fn for_source(base: &[u8], patches: Vec<SourcePatch>) -> Result<Self, PatchError> {
        Self::for_shared_source(Arc::new(base.to_vec()), patches)
    }

    /// Binds patches to a snapshot the caller already holds.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::for_source`].
    pub(crate) fn for_shared_source(
        base: Arc<Vec<u8>>,
        patches: Vec<SourcePatch>,
    ) -> Result<Self, PatchError> {
        validate_patches(source_len(&base), &patches)?;
        Ok(Self { base, patches })
    }

    /// Makes an empty patch set bound to `base`.
    #[must_use]
    pub fn empty(base: &[u8]) -> Self {
        Self {
            base: Arc::new(base.to_vec()),
            patches: Vec::new(),
        }
    }

    /// The shared snapshot these patches are bound to.
    pub(crate) fn shared_base(&self) -> &Arc<Vec<u8>> {
        &self.base
    }

    /// Source length for which this set was validated.
    #[must_use]
    pub fn base_len(&self) -> u64 {
        source_len(&self.base)
    }

    /// Patches in deterministic ascending source order.
    #[must_use]
    pub fn patches(&self) -> &[SourcePatch] {
        &self.patches
    }

    /// Returns whether this set contains no patches.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    /// Applies all patches in one pass and one result-buffer allocation.
    ///
    /// # Errors
    ///
    /// Returns [`PatchError::BaseMismatch`] unless `source` exactly matches the
    /// snapshot used to create this set. This catches same-length external
    /// changes before any bytes are written.
    pub fn apply(&self, source: &[u8]) -> Result<Vec<u8>, PatchError> {
        let (result, _) = self.render(source, false)?;
        Ok(Arc::unwrap_or_clone(result))
    }

    /// Applies patches and returns a patch set that restores the original bytes.
    ///
    /// The returned inverse is validated against the resulting byte length. It
    /// coalesces adjacent deletions that become same-offset insertions in the
    /// inverse, so undo remains representable under this module's unambiguous
    /// insertion rule.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::apply`], including a base-snapshot
    /// mismatch before it creates an inverse.
    pub fn apply_with_inverse(&self, source: &[u8]) -> Result<(Vec<u8>, PatchSet), PatchError> {
        let (result, inverse) = self.render(source, true)?;
        // `capture_inverse` guarantees this is populated. Keep this branch
        // explicit rather than using `expect`, even though a caller cannot
        // control it, so this module never panics while handling source spans.
        let inverse = inverse.ok_or(PatchError::InternalInvariant)?;
        Ok((Arc::unwrap_or_clone(result), inverse))
    }

    /// Builds the undo patch set without retaining the edited bytes.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::apply`].
    pub fn inverse(&self, source: &[u8]) -> Result<PatchSet, PatchError> {
        let (_, inverse) = self.render(source, true)?;
        inverse.ok_or(PatchError::InternalInvariant)
    }

    fn render(
        &self,
        source: &[u8],
        capture_inverse: bool,
    ) -> Result<(Arc<Vec<u8>>, Option<PatchSet>), PatchError> {
        let actual_len = source_len(source);
        if source != self.base.as_slice() {
            return Err(PatchError::BaseMismatch {
                expected: self.base_len(),
                actual: actual_len,
            });
        }

        let result_len = self.result_len()?;
        let capacity = usize::try_from(result_len)
            .map_err(|_| PatchError::ResultTooLarge { length: result_len })?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(capacity)
            .map_err(|_| PatchError::AllocationFailed { length: result_len })?;

        let mut inverse_patches = capture_inverse.then(Vec::new);
        let mut source_cursor = 0_usize;

        for patch in &self.patches {
            let start =
                usize::try_from(patch.span.start).map_err(|_| PatchError::ResultTooLarge {
                    length: patch.span.start,
                })?;
            let end = usize::try_from(patch.span.end).map_err(|_| PatchError::ResultTooLarge {
                length: patch.span.end,
            })?;

            result.extend_from_slice(&source[source_cursor..start]);
            let result_start = u64::try_from(result.len())
                .map_err(|_| PatchError::ResultTooLarge { length: u64::MAX })?;
            result.extend_from_slice(&patch.replacement);

            if let Some(inverse_patches) = &mut inverse_patches {
                let replacement_len = u64::try_from(patch.replacement.len())
                    .map_err(|_| PatchError::ResultTooLarge { length: u64::MAX })?;
                let inverse_end = result_start
                    .checked_add(replacement_len)
                    .ok_or(PatchError::ResultTooLarge { length: u64::MAX })?;
                inverse_patches.push(SourcePatch {
                    span: ByteSpan {
                        start: result_start,
                        end: inverse_end,
                    },
                    replacement: source[start..end].to_vec(),
                });
            }

            source_cursor = end;
        }

        result.extend_from_slice(&source[source_cursor..]);

        // The inverse shares the rendered snapshot it is bound to, so building
        // undo data never copies the resulting document.
        let result = Arc::new(result);
        let inverse = inverse_patches
            .map(|patches| {
                PatchSet::for_shared_source(Arc::clone(&result), coalesce_inverse_patches(patches))
            })
            .transpose()?;
        Ok((result, inverse))
    }

    fn result_len(&self) -> Result<u64, PatchError> {
        let mut length = self.base_len();
        for patch in &self.patches {
            length = length
                .checked_sub(patch.span.len())
                .ok_or(PatchError::InternalInvariant)?;
            let replacement_len = u64::try_from(patch.replacement.len())
                .map_err(|_| PatchError::ResultTooLarge { length: u64::MAX })?;
            length = length
                .checked_add(replacement_len)
                .ok_or(PatchError::ResultTooLarge { length: u64::MAX })?;
        }
        Ok(length)
    }
}

/// Why a [`PatchSet`] could not be created or applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatchError {
    /// A public `ByteSpan` had its end before its start.
    ReversedSpan {
        /// Patch index.
        index: usize,
        /// Invalid span.
        span: ByteSpan,
    },
    /// A patch span was not wholly inside the base source.
    OutOfBounds {
        /// Patch index.
        index: usize,
        /// Invalid span.
        span: ByteSpan,
        /// Valid source length.
        base_len: u64,
    },
    /// Patches were not supplied in deterministic ascending order.
    OutOfOrder {
        /// Earlier patch index.
        previous_index: usize,
        /// Later patch index that should have appeared earlier.
        index: usize,
        /// Earlier patch span.
        previous: ByteSpan,
        /// Later patch span.
        current: ByteSpan,
    },
    /// A patch touches bytes claimed by an earlier patch.
    Overlap {
        /// Earlier patch index.
        first_index: usize,
        /// Later patch index.
        second_index: usize,
        /// Earlier patch span.
        first: ByteSpan,
        /// Later patch span.
        second: ByteSpan,
    },
    /// Two insertions target the same byte offset.
    AmbiguousInsertion {
        /// First insertion index.
        first_index: usize,
        /// Second insertion index.
        second_index: usize,
        /// Shared insertion offset.
        offset: u64,
    },
    /// The source supplied to `apply` is not the snapshot the patches use.
    BaseMismatch {
        /// Length stored by the patch set.
        expected: u64,
        /// Length of the supplied source.
        actual: u64,
    },
    /// The patched source would not fit in the platform's address space.
    ResultTooLarge {
        /// Requested byte length.
        length: u64,
    },
    /// Reserving the single result buffer failed.
    AllocationFailed {
        /// Requested byte length.
        length: u64,
    },
    /// An invariant that validation establishes was unexpectedly violated.
    InternalInvariant,
}

impl fmt::Display for PatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReversedSpan { index, span } => {
                write!(
                    formatter,
                    "patch {index} has reversed span {}..{}",
                    span.start, span.end
                )
            }
            Self::OutOfBounds {
                index,
                span,
                base_len,
            } => write!(
                formatter,
                "patch {index} span {}..{} is outside source length {base_len}",
                span.start, span.end
            ),
            Self::OutOfOrder {
                previous_index,
                index,
                ..
            } => write!(
                formatter,
                "patch {index} must appear before patch {previous_index} in source order"
            ),
            Self::Overlap {
                first_index,
                second_index,
                ..
            } => write!(
                formatter,
                "patch {second_index} overlaps patch {first_index}"
            ),
            Self::AmbiguousInsertion {
                first_index,
                second_index,
                offset,
            } => write!(
                formatter,
                "insertions {first_index} and {second_index} both target byte {offset}"
            ),
            Self::BaseMismatch { expected, actual } => write!(
                formatter,
                "patches require a {expected}-byte source, received {actual} bytes"
            ),
            Self::ResultTooLarge { length } => {
                write!(
                    formatter,
                    "patched result length {length} does not fit this platform"
                )
            }
            Self::AllocationFailed { length } => {
                write!(
                    formatter,
                    "could not allocate {length} bytes for patched result"
                )
            }
            Self::InternalInvariant => {
                formatter.write_str("validated patch invariant was violated")
            }
        }
    }
}

impl std::error::Error for PatchError {}

fn validate_patches(base_len: u64, patches: &[SourcePatch]) -> Result<(), PatchError> {
    let mut previous: Option<(usize, &SourcePatch)> = None;
    let mut active_non_empty: Option<(usize, ByteSpan)> = None;
    let mut last_insertion: Option<(usize, u64)> = None;

    for (index, patch) in patches.iter().enumerate() {
        let span = patch.span;
        if span.start > span.end {
            return Err(PatchError::ReversedSpan { index, span });
        }
        if span.end > base_len {
            return Err(PatchError::OutOfBounds {
                index,
                span,
                base_len,
            });
        }

        if let Some((previous_index, previous_patch)) = previous {
            if patch_order(patch) < patch_order(previous_patch) {
                return Err(PatchError::OutOfOrder {
                    previous_index,
                    index,
                    previous: previous_patch.span,
                    current: span,
                });
            }
        }

        if let Some((first_index, first_span)) = active_non_empty {
            if span.start < first_span.end {
                return Err(PatchError::Overlap {
                    first_index,
                    second_index: index,
                    first: first_span,
                    second: span,
                });
            }
            active_non_empty = None;
        }

        if span.is_empty() {
            if let Some((first_index, offset)) = last_insertion {
                if offset == span.start {
                    return Err(PatchError::AmbiguousInsertion {
                        first_index,
                        second_index: index,
                        offset,
                    });
                }
            }
            last_insertion = Some((index, span.start));
        } else {
            active_non_empty = Some((index, span));
        }

        previous = Some((index, patch));
    }
    Ok(())
}

fn patch_order(patch: &SourcePatch) -> (u64, u8, u64) {
    // Empty spans sort before a replacement at their shared start, producing
    // the documented "insert then replace" behavior without an implicit sort.
    (
        patch.span.start,
        u8::from(!patch.span.is_empty()),
        patch.span.end,
    )
}

fn coalesce_inverse_patches(mut patches: Vec<SourcePatch>) -> Vec<SourcePatch> {
    patches.sort_by_key(patch_order);

    let mut coalesced: Vec<SourcePatch> = Vec::with_capacity(patches.len());
    for patch in patches {
        if let Some(previous) = coalesced.last_mut() {
            if previous.span.is_empty()
                && patch.span.is_empty()
                && previous.span.start == patch.span.start
            {
                previous.replacement.extend_from_slice(&patch.replacement);
                continue;
            }
        }
        coalesced.push(patch);
    }
    coalesced
}

fn source_len(source: &[u8]) -> u64 {
    u64::try_from(source.len()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{PatchError, PatchSet, SourcePatch};
    use marksheet_model::ByteSpan;

    fn span(start: u64, end: u64) -> ByteSpan {
        ByteSpan::try_new(start, end).unwrap()
    }

    fn patch(start: u64, end: u64, replacement: impl Into<Vec<u8>>) -> SourcePatch {
        SourcePatch::new(span(start, end), replacement)
    }

    #[test]
    fn applies_insert_delete_replace_to_arbitrary_bytes() {
        let source = [0xff, b'a', 0, b'b', 0xfe, b'c'];
        let patches = PatchSet::for_source(
            &source,
            vec![patch(1, 1, b"+".to_vec()), patch(2, 4, vec![0x80, b'X'])],
        )
        .unwrap();

        assert_eq!(
            patches.apply(&source).unwrap(),
            [0xff, b'+', b'a', 0x80, b'X', 0xfe, b'c']
        );
    }

    #[test]
    fn permits_no_op_patches() {
        let source = b"source";
        let patches = PatchSet::for_source(source, vec![patch(3, 3, Vec::new())]).unwrap();

        assert_eq!(patches.apply(source).unwrap(), source);
        let (edited, inverse) = patches.apply_with_inverse(source).unwrap();
        assert_eq!(inverse.apply(&edited).unwrap(), source);
    }

    #[test]
    fn validates_bounds_order_overlap_and_insertions() {
        let out_of_bounds = PatchSet::for_source(b"abc", vec![patch(2, 4, b"x".to_vec())]);
        assert!(matches!(out_of_bounds, Err(PatchError::OutOfBounds { .. })));

        let reversed = PatchSet::for_source(
            b"abc",
            vec![SourcePatch::new(
                ByteSpan { start: 2, end: 1 },
                b"x".to_vec(),
            )],
        );
        assert!(matches!(reversed, Err(PatchError::ReversedSpan { .. })));

        let out_of_order = PatchSet::for_source(
            b"abcd",
            vec![patch(2, 3, b"x".to_vec()), patch(0, 1, b"y".to_vec())],
        );
        assert!(matches!(out_of_order, Err(PatchError::OutOfOrder { .. })));

        let overlap = PatchSet::for_source(
            b"abcd",
            vec![patch(0, 3, b"x".to_vec()), patch(2, 4, b"y".to_vec())],
        );
        assert!(matches!(overlap, Err(PatchError::Overlap { .. })));

        let insertion_inside_replace = PatchSet::for_source(
            b"abcd",
            vec![patch(0, 3, b"x".to_vec()), patch(2, 2, b"y".to_vec())],
        );
        assert!(matches!(
            insertion_inside_replace,
            Err(PatchError::Overlap { .. })
        ));

        let duplicate_insertions = PatchSet::for_source(
            b"abcd",
            vec![patch(2, 2, b"x".to_vec()), patch(2, 2, b"y".to_vec())],
        );
        assert!(matches!(
            duplicate_insertions,
            Err(PatchError::AmbiguousInsertion { .. })
        ));
    }

    #[test]
    fn insertion_and_replacement_at_same_boundary_are_deterministic() {
        let patches = PatchSet::for_source(
            b"abcd",
            vec![patch(1, 1, b"+".to_vec()), patch(1, 3, b"X".to_vec())],
        )
        .unwrap();
        assert_eq!(patches.apply(b"abcd").unwrap(), b"a+Xd");

        let noncanonical = PatchSet::for_source(
            b"abcd",
            vec![patch(1, 3, b"X".to_vec()), patch(1, 1, b"+".to_vec())],
        );
        assert!(matches!(noncanonical, Err(PatchError::OutOfOrder { .. })));
    }

    #[test]
    fn refuses_stale_source() {
        let patches = PatchSet::for_source(b"abc", vec![patch(0, 1, b"x".to_vec())]).unwrap();
        assert_eq!(
            patches.apply(b"longer").unwrap_err(),
            PatchError::BaseMismatch {
                expected: 3,
                actual: 6
            }
        );
        assert_eq!(
            patches.apply(b"zbc").unwrap_err(),
            PatchError::BaseMismatch {
                expected: 3,
                actual: 3
            }
        );
    }

    #[test]
    fn inverse_restores_source_and_coalesces_deleted_bytes() {
        let source = b"abcdef";
        let patches = PatchSet::for_source(
            source,
            vec![patch(1, 3, Vec::new()), patch(3, 5, Vec::new())],
        )
        .unwrap();

        let (edited, inverse) = patches.apply_with_inverse(source).unwrap();
        assert_eq!(edited, b"af");
        assert_eq!(inverse.patches(), &[patch(1, 1, b"bcde".to_vec())]);
        assert_eq!(inverse.apply(&edited).unwrap(), source);
    }

    #[test]
    fn generated_patch_sets_round_trip_without_utf8_assumptions() {
        for seed in 0_u64..128 {
            let source = generated_bytes(seed, 97);
            let patches = generated_patches(seed, source.len() as u64);
            let patches = PatchSet::for_source(&source, patches).unwrap();

            let (edited, inverse) = patches.apply_with_inverse(&source).unwrap();
            assert_eq!(inverse.apply(&edited).unwrap(), source, "seed {seed}");
            assert_eq!(patches.inverse(&source).unwrap(), inverse, "seed {seed}");
        }
    }

    fn generated_bytes(mut state: u64, length: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(length);
        for _ in 0..length {
            state = next(state);
            bytes.push(state.to_le_bytes()[0]);
        }
        bytes
    }

    fn generated_patches(mut state: u64, base_len: u64) -> Vec<SourcePatch> {
        let mut patches = Vec::new();
        let mut offset = 0_u64;
        while offset < base_len {
            state = next(state);
            let gap = state % 5;
            offset = offset.saturating_add(gap);
            if offset >= base_len {
                break;
            }

            state = next(state);
            if state & 1 == 0 {
                let replacement = state.to_le_bytes()[..2].to_vec();
                patches.push(patch(offset, offset, replacement));
                offset += 1;
            } else {
                let width = 1 + (state % 4).min(base_len - offset - 1);
                let replacement = if state & 2 == 0 {
                    Vec::new()
                } else {
                    vec![state.to_le_bytes()[2]]
                };
                patches.push(patch(offset, offset + width, replacement));
                offset += width;
            }
        }
        patches
    }

    fn next(value: u64) -> u64 {
        value
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
    }
}
