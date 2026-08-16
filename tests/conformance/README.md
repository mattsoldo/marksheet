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

`valid/all_core.ms` is the broad core-language fixture. The focused fixtures
cover CSV edge cases (including CRLF input) and sparse coordinates. The invalid
files are deliberately small so a diagnostic regression is easy to localize.
