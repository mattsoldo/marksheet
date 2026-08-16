# Marksheet editing fixtures

These fixtures define the public Milestone 3 editing contract. They are
deliberately source-oriented: each successful case supplies a complete source
file, an ordered byte-patch plan, and the expected resulting source. Rejected
cases supply an empty plan and a stable failure category. `semantic_equivalence`
cases contain two complete documents instead of patches.

`manifest.json` lists all cases. `schema.json` describes the small fixture
format and `validate.sh` validates its structure, applies every successful
patch plan, and checks inverse patches when present. The shell script is
intentionally independent of the Rust implementation so contributors can
review fixture arithmetic before wiring a new editor implementation to the
corpus.

`marksheet-edit/tests/edit_conformance.rs` is the executable behavioral
consumer. It strictly deserializes every manifest fixture, executes committed,
no-op, rejected, and external-rebase-conflict transactions through the public
editing API, compares resulting source and ordered patches exactly, restores
the inverse source, and runs `SemanticDiff` for the equivalence fixture.

Patch offsets are zero-based UTF-8 byte offsets into the `before` file. Ranges
are half-open, sorted, and nonoverlapping. Apply them in descending order. A
replacement is JSON text and is encoded as UTF-8. A no-op and every rejected
transaction must have an empty `patches` array.

The corpus covers the required editing proof:

- one-field scalar replacement with CSV quoting and untouched extension bytes;
- formula-field replacement without canonicalizing surrounding source;
- append immediately before a table's owning `@end`;
- atomic sheet/name identifier updates and label-only rename;
- whole-block movement and its reference rewrite policy;
- focused reuse of an existing style through `@apply`;
- virtual-cell and partial-block refusal;
- no-op, undo/redo inverse patches, external-change conflict detection; and
- semantic equivalence that ignores presentation-only source differences.

The concrete behavior, including rebase preconditions and unsupported
structural operations, is normative in [`SPEC.md`](../../SPEC.md#191-transactional-edit-contract).
