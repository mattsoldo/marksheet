# Marksheet Format Specification

**Format version:** 0.1

**Document revision:** 0.1.0-draft

**Status:** Draft

This document defines the Marksheet core interchange format. The examples and
requirements here are expected to change during the `0.x` design period.

## 1. Conventions

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**,
and **MAY** are to be interpreted as described by
[BCP 14](https://www.rfc-editor.org/info/bcp14) when, and only when, they appear
in all capitals.

An implementation can conform as a parser, calculator, editor, or renderer; see
[Conformance](#20-conformance).

## 2. File identity and encoding

- The recommended filename extension is `.ms`.
- A Marksheet document MUST be UTF-8.
- A document MUST begin at byte zero with a version header. A UTF-8 byte-order
  mark is not permitted.
- Parsers MUST accept LF and CRLF input. Canonical output MUST use LF.
- Canonical output MUST end with exactly one newline.

The first line has this form:

```text
#!marksheet 0.1
```

The version is part of the file contents so that file identity does not depend
on an extension or media type.

## 3. Versioning

The header contains a `major.minor` format version.

- Before 1.0, a new minor version MAY contain breaking changes.
- Beginning with 1.0, a major version denotes breaking syntax or semantic
  changes and a minor version denotes backward-compatible additions.
- Editorial corrections do not change the format version.
- A processor MUST reject an unsupported major version.
- A processor MAY open a higher minor version only if it preserves unknown
  content and clearly reports any semantics it cannot implement.

Formula semantics are independently versioned using a formula profile:

```text
@book formula-profile="portable-a1@1"
```

This separation allows the container language and formula library to evolve at
different rates.

## 4. Lexical structure

Marksheet is line-oriented outside CSV and extension bodies.

- A directive begins with `@` in column one.
- A comment begins with `#` in column one, except for the version header.
- A blank line has no semantic meaning.
- Spaces may be used for indentation only inside opaque extension bodies.
- Core directive names and identifier syntax are ASCII.
- User text and CSV data may contain any valid Unicode scalar value.

Comments and blank lines MAY appear between directives. They MUST NOT appear
inside a CSV body unless they are intended as CSV data.

```text
# This comment is preserved by lossless editors.
```

### 4.1 Identifiers

Sheet, table, name, and style identifiers use:

```text
[a-z][a-z0-9_]*
```

Identifiers are case-sensitive because canonical identifiers are lowercase.
They are stable machine references and are not display labels.

The following namespaces are independent:

- sheets;
- styles; and
- workbook values, comprising tables and named ranges.

A table and a named range MUST NOT share an identifier. A name MUST NOT match an
A1 or R1C1 address when compared case-insensitively.

### 4.2 Quoted strings

Directive strings use JSON string syntax, including its escaping rules:

```text
"A human-facing label"
"A quote: \"hello\""
```

### 4.3 Directive properties

Some directives contain space-separated `key=value` properties. A value is a
JSON string, a JSON number, `true`, `false`, or a lowercase bare identifier.
Whitespace is not permitted around `=`.

Property keys use:

```text
[a-z][a-z0-9-]*
```

A property key MUST occur at most once in a directive. Unless a directive
explicitly permits another form, unknown properties, duplicate properties,
missing required arguments, and trailing content are invalid. Core directives
therefore have only the exact forms specified in this document.

## 5. Document structure

A document contains workbook-level declarations followed by one or more sheet
sections.

```text
#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

@style header bold=true fill="#e8eef7"
@name tax_rate = inputs!G2

@sheet inputs "Inputs"
# Sheet content

@sheet summary "Summary"
# Sheet content
```

The version header MUST occur exactly once. `@book` MAY occur once and MUST
precede the first `@sheet`. At least one `@sheet` is REQUIRED.

Styles and names are workbook-scoped and MUST precede the first sheet in
canonical output. `@use` and `@require` are workbook-scoped; `@extension` may
be workbook- or sheet-scoped. Forward references are permitted.

## 6. Workbook settings

The optional `@book` directive has this form:

```text
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"
```

Draft 0.1 defines these properties:

| Property | Meaning | Default |
| --- | --- | --- |
| `locale` | BCP 47 locale used for display | `"en-US"` |
| `timezone` | IANA time-zone identifier used for display and conversion | `"UTC"` |
| `formula-profile` | Formula syntax and function profile | `"portable-a1@1"` |

Locale MUST NOT change source syntax. Numbers always use `.` as the decimal
separator, function argument separators are commas, and core function names are
English identifiers.

Unknown `@book` properties are invalid in 0.1 rather than silently ignored.

## 7. Sheets

A sheet section begins with:

```text
@sheet inputs "Inputs"
```

The first argument is the stable sheet identifier. The quoted second argument
is its human-facing label. The label MAY be changed without breaking formulas,
because formulas reference the identifier.

- Sheet identifiers MUST be unique.
- Sheet order is document order.
- A sheet continues until the next `@sheet` or end of file.
- Labels SHOULD be unique, but uniqueness is not required.
- A sheet has no fixed row or column limit in the format.

An implementation MAY impose resource limits, but it MUST report them and MUST
NOT silently truncate cells outside those limits.

## 8. Coordinates and ranges

Marksheet uses A1 coordinates.

- Columns are one or more ASCII letters: `A`, `Z`, `AA`, and so on.
- Rows are positive decimal integers beginning at 1.
- A cell is a column followed by a row, such as `B7`.
- A range is two cells separated by `:`, such as `B7:D20`.
- Absolute markers are permitted in formulas: `$B$7`, `B$7`, and `$B7`.
- Directive targets MUST NOT contain `$`; directives describe concrete ranges.

Coordinates are case-insensitive on input. Canonical output uses uppercase
column letters.

Cross-sheet references use a sheet identifier and `!`:

```text
inputs!B7
inputs!B7:D20
```

Because sheet identifiers are restricted identifiers, quoted sheet names are
not part of the core grammar.

## 9. Blocks

An unnamed block places a rectangular CSV payload on the current sheet:

```text
@block A1 csv
Name,Quantity,Price
Widget,4,12.50
Gadget,2,8.00
@end
```

`A1` is the upper-left cell. The first CSV field maps to `A1`; subsequent fields
advance across columns and subsequent records advance down rows.

- A block MUST contain at least one record and one field.
- Every record in a block MUST contain the same number of fields.
- The `@end` terminator MUST appear alone on a physical line outside a quoted
  CSV field.
- To store a one-field row whose value is `@end`, the field MUST be CSV-quoted.
- A block reserves its complete rectangular footprint, including blank fields.
- Block footprints on the same sheet MUST NOT overlap.

Sheets are sparse because any number of blocks can be anchored at distant
coordinates without representing the intervening cells.

### 9.1 CSV dialect

The core `csv` body follows the field, quote, and record rules of
[RFC 4180](https://www.rfc-editor.org/rfc/rfc4180), with these requirements:

- the delimiter is comma;
- the quote character is `"`;
- a quote inside a quoted field is written `""`;
- quoted fields MAY contain commas and newlines;
- LF and CRLF records are accepted;
- canonical output uses LF; and
- canonical output quotes a field only when required by CSV syntax or by the
  `@end` terminator rule.

CSV quoting does not make a value textual. Scalar interpretation happens after
CSV decoding.

## 10. Tables

A table is a named block whose first record contains column headers:

```text
@table costs A1 csv
Item,Cost,Quantity,Subtotal
Rent,1500,1,
Utilities,200,1,
@end
```

The arguments are the table identifier, top-left cell, and payload encoding.

- Table identifiers are workbook-scoped and MUST be unique.
- A table header MUST contain nonblank text values.
- Header values MUST be unique within the table.
- A table MUST contain at least a header record. It MAY contain zero data rows.
- A table occupies and reserves the full block footprint.
- Table rows and columns are contiguous.

Every table is physically a block, but an unnamed block is not a semantic
table. Unnamed blocks are appropriate for reports, labels, and irregular grid
layouts. Tables are appropriate when rows share a schema and formulas benefit
from structured references.

### 10.1 Structured references

Draft 0.1 defines:

```text
costs[Cost]          # Data cells in the Cost column
costs[#Headers]      # Header row
costs[#Data]         # Entire data body
costs[@Cost]         # Cost cell in the current table row
[@Cost]              # Same-table shorthand inside a table formula
```

A `]` in a header is escaped as `]]` in a structured reference. Current-row
references are valid only while evaluating a formula in a table data row.

## 11. Cell values

After CSV decoding, fields are interpreted in this order:

1. An empty field is a blank cell.
2. A leading apostrophe forces text and is removed. A field containing only
   `'` is an empty string.
3. A leading `=` is a formula.
4. A core error token is an error value.
5. `true` or `false` is a boolean.
6. A canonical number is a number.
7. An ISO date is a date.
8. An ISO datetime with an offset is a datetime.
9. Anything else is text.

Interpretation uses the complete decoded field. Leading or trailing whitespace
therefore causes a field to be text unless it was forced text already.

### 11.1 Numbers

The number grammar is:

```text
-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?
```

Core calculation uses IEEE 754 binary64 semantics. A canonical serializer emits
the shortest decimal representation that round-trips to the same finite value.
NaN and infinity are not source number literals; calculations that would
produce them return an error.

### 11.2 Dates and datetimes

A date uses `YYYY-MM-DD`. A datetime uses ISO 8601 and MUST include `Z` or a
numeric UTC offset:

```text
2026-08-16
2026-08-16T14:30:00Z
2026-08-16T10:30:00-04:00
```

Invalid calendar dates are text in a tolerant parser and errors in a validating
parser. Canonical output uses uppercase `T` and `Z`.

### 11.3 Errors

Core error values are:

```text
#DIV/0!
#N/A
#NAME?
#NUM!
#REF!
#VALUE!
#CIRC!
```

### 11.4 Text escapes

The apostrophe prefix allows values that resemble another type:

```text
'00123
'=not a formula
'true
'2026-08-16
'
```

The stored strings are `00123`, `=not a formula`, `true`, `2026-08-16`, and the
empty string respectively.

## 12. Named ranges

A workbook-scoped named cell or range is declared before the first sheet:

```text
@name tax_rate = inputs!G2
@name monthly_revenue = inputs!B5:B16
@name cost_values = costs[Cost]
```

- A name target MUST be one cell, one contiguous range, or one table column.
- A cell or range name target MUST include its sheet identifier; bare targets
  such as `A1` or `A1:B2` are invalid. Table-column targets use their
  workbook-scoped table identifier.
- A name MUST resolve after the entire workbook is parsed.
- Forward references are allowed.
- Duplicate or unresolved names are invalid.
- Formula constants and computed named expressions are not part of the 0.1
  core.
- Structural edits SHOULD update a name definition rather than rewriting each
  formula that uses the name.

## 13. Formulas

A formula is a cell value beginning with `=`. The formula is stored, not a
cached result.

```text
=A1+B1
=SUM(inputs!B2:B12)
=SUM(monthly_revenue)*(1-tax_rate)
=SUM(costs[Subtotal])
```

Formula syntax is locale-independent. Function names are case-insensitive and
canonical output uses uppercase names. Workbook identifiers and table headers
retain their defined spelling.

### 13.1 Milestone 1 representation

Milestone 1 stores every formula as an opaque `FormulaSource`: the complete
decoded field, including its leading `=`. It validates only that the field is a
formula value. Formula grammar parsing, reference validation, and formula-body
canonicalization begin in Milestone 2. In particular, Milestone 1 canonical
formatting MUST preserve formula-body spelling exactly. This implementation
milestone does not relax the final formula requirements in this section or the
calculator conformance requirements in [20.2](#202-calculator).

### 13.2 Operators

From highest to lowest precedence, portable A1 formulas support:

1. Parentheses and function calls
2. Exponentiation: `^`
3. Unary signs: `+`, `-`
4. Multiplication and division: `*`, `/`
5. Addition and subtraction: `+`, `-`
6. String concatenation: `&`
7. Comparisons: `=`, `<>`, `<`, `<=`, `>`, `>=`

Exponentiation associates right-to-left. Other binary arithmetic operators
associate left-to-right.

### 13.3 References

Formulas may reference:

- cells and ranges on the current sheet;
- cells and ranges using stable sheet identifiers;
- workbook named ranges;
- table columns and regions; and
- current-row table cells.

External workbook and network references are not part of the core.

### 13.4 Required functions in `portable-a1@1`

The initial function set is deliberately compact:

| Category | Functions |
| --- | --- |
| Aggregation | `SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT`, `COUNTA` |
| Logic | `IF`, `AND`, `OR`, `NOT`, `IFERROR` |
| Numeric | `ABS`, `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `INT`, `MOD` |
| Text | `CONCAT`, `LEFT`, `RIGHT`, `MID`, `LEN`, `LOWER`, `UPPER`, `TRIM` |
| Lookup | `INDEX`, `MATCH` |
| Date | `DATE`, `YEAR`, `MONTH`, `DAY` |
| Inspection | `ISBLANK`, `ISNUMBER`, `ISTEXT`, `ISERROR` |

Exact argument coercion, error propagation, and edge cases will be captured in
the formula conformance corpus before the profile reaches stable status. Until
then, this table defines required surface area but not final edge semantics.

Volatile functions such as `NOW`, `TODAY`, `RAND`, and `RANDBETWEEN` are not in
the core profile. They would make identical source produce different results at
different times.

### 13.5 Calculation

- Formulas are pure and MUST NOT perform I/O.
- A calculator MUST construct dependencies and evaluate in dependency order.
- A circular dependency returns `#CIRC!` for every cell in the cycle.
- Iterative calculation is an extension.
- Blank arithmetic coercion and function-specific coercion are defined by the
  formula profile, not by the container grammar.
- A calculator MUST NOT silently substitute cached values for unsupported
  formulas because core files do not contain authoritative caches.

## 14. Formula fills

`@fill` concisely applies a formula over a finite target range:

```text
@fill C2:C100 =A2*B2
@fill costs[Subtotal] =[@Cost]*[@Quantity]
```

- `@fill` is sheet-scoped and MUST follow the block or table that owns its
  target.
- The target MUST resolve to a finite contiguous range or table column.
- The target's entire concrete footprint MUST be contained in exactly one
  preceding block or table footprint on the current sheet. A table-column
  target is owned by that table and denotes only its data cells, never its
  header. A fill MUST NOT straddle, extend beyond, or be contained by more than
  one source footprint.
- Every target cell MUST be blank in its source block.
- For A1 formulas, the formula is interpreted at the target's top-left cell and
  copied using conventional relative and absolute reference adjustment.
- A table formula is evaluated separately in each data row.
- A fill that conflicts with a nonblank source cell is invalid.

The fill directive is part of core formula support because it avoids repeating
the same formula across thousands of CSV records.

## 15. Styles

Styles are reusable workbook-scoped declarations:

```text
@style header bold=true fill="#e8eef7" align=center
@style money number=currency currency="USD" decimals=2 align=right
@style note italic=true text-color="#666666" wrap=true
```

Draft 0.1 style properties are:

| Property | Values |
| --- | --- |
| `bold`, `italic`, `wrap` | Boolean |
| `text-color`, `fill` | `#RRGGBB` or `#RRGGBBAA` string |
| `font-size` | Positive number in points |
| `align` | `left`, `center`, `right`, `general` |
| `valign` | `top`, `middle`, `bottom` |
| `number` | `general`, `integer`, `decimal`, `percent`, `currency`, `date`, `datetime` |
| `decimals` | Integer from 0 through 15 |
| `currency` | Three-letter ISO 4217 code string |

`decimals` is meaningful with `decimal`, `percent`, and `currency`. `currency`
is required when `number=currency`.

A style is applied within the current sheet:

```text
@apply A1:D1 header
@apply costs[#Headers] header
@apply costs[Cost] money
@apply B2:B10 money note
```

Styles in one `@apply` are merged left-to-right. If multiple `@apply`
directives affect a cell, later directives override earlier directives one
property at a time. An application with no renderer MUST preserve styles even
if it cannot display them.

Conditional formatting, borders, fonts by family, merged cells, and themes are
extensions in 0.1.

## 16. Row and column geometry

The current sheet may define column width and row height:

```text
@column A width=18
@column B:D width=12
@row 1 height=24
@row 2:10 height=18
```

- Column width is measured in approximate `0` character units, consistent with
  common spreadsheet interfaces.
- Row height is measured in points.
- Width and height MUST be positive finite numbers.
- Later declarations override earlier declarations for overlapping rows or
  columns.

Hidden rows, hidden columns, grouping, and automatic-fit state are extensions.

## 17. Extensions

Extensions add declarative semantics without expanding the core grammar.

An optional capability is declared with `@use`:

```text
@use charts@1
```

A capability required to interpret the workbook is declared with `@require`:

```text
@require actuarial_functions@1
```

An extension instance is an opaque block:

```text
@extension charts@1 "revenue"
type=bar
source=summary!A1:B13
@end
```

- Extension IDs use an identifier followed by `@` and a positive major version.
- `@use` and `@require` are permitted only before the first `@sheet` and are
  workbook-scoped. `@extension` is permitted either before the first `@sheet`
  (workbook scope) or within a sheet (that sheet's scope); parsers and editors
  MUST retain that scope.
- A capability base identifier MUST have at most one declaration across
  `@use` and `@require`. Repeating a declaration, declaring multiple major
  versions, or declaring the same capability as both optional and required is
  invalid.
- The quoted instance name is locally meaningful to the extension.
- Within one scope, an `(extension ID, instance name)` pair MUST be unique.
- The payload is every byte after the directive newline through the newline
  before the first physical line equal to `@end`.
- A payload that needs a literal `@end` line MUST encode or escape it according
  to its extension's rules.
- A lossless editor MUST preserve an unknown payload byte-for-byte.
- An unsupported `@use` MAY be skipped with a visible warning.
- An unsupported `@require` MUST prevent calculation or rendering from being
  presented as complete.
- A parser MAY still expose the core workbook model when a required extension
  is unsupported.

Extension support is evaluated against the application's extension registry.
An empty registry supports no extension IDs: every `@use` produces a visible
warning and every `@require` produces an error. Unsupported extension instances
remain syntactically valid opaque data and MUST be retained; their presence does
not change the warning/error behavior of the corresponding capability
declaration.

Workbooks do not contain plugin URLs or executable plugin code. They MUST NOT
cause an application to install, download, or run a plugin automatically.

### 17.1 Implementation plugin surface

A runtime MAY expose hooks equivalent to:

```text
registerExtensionParser(id, parser)
registerFormulaFunctions(namespace, functions)
registerRenderer(id, renderer)
registerConverter(id, converter)
```

The API names and host language are not normative. The security boundary is:
the user or host application installs implementation code; the workbook only
contains declarative content referring to it.

## 18. Canonical serialization

Canonical serialization exists to make generated files reproducible and Git
diffs stable. It is an explicit operation; editors SHOULD preserve local source
spelling during ordinary edits.

Canonical output MUST:

1. Use UTF-8 without a byte-order mark.
2. Use LF line endings and one final newline.
3. Write the normalized version header first.
4. Use one blank line between top-level declarations and sheet sections.
5. Use lowercase directive names and identifiers.
6. Use uppercase A1 column letters.
7. Use JSON escaping for directive strings.
8. Use canonical scalar spellings and minimal required CSV quoting.
9. Preserve authored directive order, property order, comments in their
   attached positions, sheet order, block order, table row order, and style
   application order.
10. Preserve explicitly authored `@book` properties, including properties set
    to their Draft 0.1 defaults, and do not synthesize properties that were
    omitted from the source.
11. Normalize directive whitespace and values, but in Milestone 1 preserve
    formula-body spelling exactly rather than canonicalizing it.
12. Normalize CRLF line endings to LF in opaque extension payloads while
    preserving all other payload bytes.
13. Exclude save times, generator versions, random IDs, and other volatile
    metadata unless explicitly authored as extension data.

A canonicalizer MUST NOT reorder directives, properties, table rows, sheets,
style applications, comments, or other constructs for which order or placement
has meaning. A lossless no-op open-save cycle is not canonical serialization
and MUST preserve opaque extension payload bytes, including their original line
endings, exactly.

## 19. Lossless editing and source maps

A conforming editor maintains both a semantic workbook model and source
locations.

At minimum, source mappings SHOULD identify:

- each directive;
- every CSV field;
- each formula token stream;
- each comment and blank-line region; and
- each opaque extension payload.

When a user changes one cell, the editor SHOULD replace only that field and any
CSV quoting required for its record. It SHOULD NOT regenerate unaffected blocks
or reorder declarations.

Unknown optional extensions and comments MUST survive a lossless open-save
cycle. A tool that cannot meet this requirement MUST identify itself as a
canonicalizing or lossy writer before overwriting the source.

Structural spreadsheet actions require source-aware behavior:

- inserting rows into a table changes that table body;
- moving a block changes its anchor rather than manufacturing empty cells;
- renaming a sheet label does not rewrite formulas;
- changing a sheet identifier rewrites every identifier reference atomically;
- inserting cells adjusts A1 references according to spreadsheet semantics;
  and
- a named range is updated at its declaration rather than expanded in formulas.

## 20. Conformance

### 20.1 Core parser

A conforming core parser MUST:

- accept every valid 0.1 core document;
- reject invalid core syntax with a line and column or byte range;
- build the workbook, sheets, sparse blocks, tables, values, names, formulas,
  styles, and extension declarations;
- detect duplicate identifiers, overlaps, invalid targets, and unresolved
  references; and
- retain opaque extension payloads.

### 20.2 Calculator

A conforming `portable-a1@1` calculator MUST:

- implement the formula grammar and required function surface;
- produce the defined errors rather than host-language exceptions;
- detect circular references; and
- pass the formula profile's conformance corpus once that corpus is published.

### 20.3 Renderer

A conforming renderer MUST:

- preserve sheet order and cell coordinates;
- display core scalar and calculated values;
- apply core styles, row heights, and column widths materially as specified;
- distinguish blanks from empty strings; and
- visibly report unsupported required extensions.

Pixel-identical output is not required.

### 20.4 Lossless editor

A conforming lossless editor MUST satisfy the preservation requirements in
[Lossless editing and source maps](#19-lossless-editing-and-source-maps).

### 20.5 Converter

A converter SHOULD emit a report containing:

- source and destination formats and versions;
- features converted exactly;
- features approximated;
- features omitted;
- formulas translated or replaced; and
- cell or source locations for every warning.

It MUST NOT describe a conversion as lossless when the report contains an
approximation or omission.

## 21. Security and resource handling

- Core parsing and formula evaluation require no network access.
- Core formulas MUST NOT access files, environment variables, clocks, random
  sources, processes, or the network.
- Applications MUST treat extension payloads as untrusted data.
- Applications MUST NOT automatically install plugins requested by a workbook.
- Parsers and calculators SHOULD provide configurable limits for input bytes,
  CSV field size, formula depth, dependency count, and evaluation work.
- Exceeding a limit MUST produce a diagnostic rather than truncation or a
  partially trusted result.
- Renderers MUST escape text appropriately for their output environment.

## 22. Informative grammar sketch

This sketch describes the outer language; it does not replace the normative
rules above or the CSV grammar.

```ebnf
document          = header, newline, { pre_sheet_item }, sheet, { sheet } ;
header            = "#!marksheet", space, version ;
version           = digits, ".", digits ;

pre_sheet_item    = blank | comment | book | style | name |
                    use | require | extension ;
sheet             = sheet_header, newline, { sheet_item } ;
sheet_item        = blank | comment | block | table | fill |
                    apply | column | row | extension ;

block             = "@block", space, cell, space, "csv", newline,
                    csv_body, "@end", newline ;
table             = "@table", space, identifier, space, cell, space,
                    "csv", newline, csv_body, "@end", newline ;

identifier        = lower, { lower | digit | "_" } ;
comment           = "#", { unicode_scalar }, newline ;
blank             = newline ;
```

## 23. Complete example

See [`examples/budget.ms`](examples/budget.ms) for a complete Draft 0.1
workbook.
