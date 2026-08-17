# Marksheet Milestone 1 conformance fixtures

Each `invalid/*.ms` fixture has a sibling `.diagnostics` file containing the
stable diagnostic code(s) expected from `marksheet check`, one code per line.
The primary span is implementation-specific, but must point at the offending
construct. `valid/*.diagnostics` files are intentionally empty unless the
fixture explicitly exercises an optional extension warning.

Diagnostic contract for this corpus:

- `MS1001`: version header/version error
- `MS1101`: malformed, unknown, misplaced directive or property
- `MS1102`: malformed or unterminated CSV
- `MS1201`: invalid or reserved identifier
- `MS1202`: invalid coordinate or range
- `MS1204`: non-rectangular CSV block
- `MS1301`: duplicate or conflicting declaration
- `MS1302`: overlapping block/table footprints
- `MS2101`: unresolved named-range definition
- `MS2102`: unresolved sheet, table, style, or directive target
- `MS2201`: invalid scalar, date, style, or geometry value
- `MS3101`: unavailable required extension
- `MS3102`: unavailable optional extension (warning)
- `MS3103`: undeclared opaque extension instance (warning)

`valid/all_core.ms` is the broad core-language fixture. The focused fixtures
cover CSV edge cases (including CRLF input) and sparse coordinates. The invalid
files are deliberately small so a diagnostic regression is easy to localize.

This corpus deliberately carries no lone `CR` source. Every file here is also
projected by the independent Python consumer in `conformance/python`, whose
physical-line model splits on `LF` only; the Rust scanner instead consumes a
lone `CR` as a terminator and reports `MS1004`. Those two recovery models
disagree about how the *rest* of such a document is read, and reconciling them
is a specification decision about lone-`CR` recovery rather than a fixture
change. Lone-`CR` behavior is therefore pinned where only one implementation is
involved: `MS1004` and the quoted-field `MS1102` in `marksheet-syntax`'s unit
tests and CLI tests, and, corpus-independently, by the
`a_lone_carriage_return_is_diagnosed_unless_it_is_opaque_payload_data` guard in
`crates/marksheet-syntax/tests/conformance.rs`, which injects a lone `CR` at
every byte offset of every fixture here. A lone `CR` inside an opaque
`@extension` payload is *not* an error: SPEC sections 17 and 18 item 12 keep
unknown payload bytes and normalize only CRLF there, so that case lives in
`tests/extensions/opaque_payload_carriage_return.json`.
