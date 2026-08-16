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
- A name MUST NOT be `true` or `false`. Those spellings are reserved for the
  case-insensitive formula Boolean literals. This restriction applies only to
  bare workbook names: a sheet or table may use either identifier because
  `true!A1` and `true[Header]` are unambiguous contextual references.
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

### 13.2 Lexical grammar

The leading `=` identifies a formula and is not part of its expression AST. An
expression may contain ASCII space, tab, CR, or LF between tokens. Other
Unicode whitespace is not formula whitespace. Whitespace is not permitted
inside an A1 cell or range reference, inside a sheet qualifier, or between a
table identifier and the `[` that begins its structured selector.

The header portion inside a structured selector is data rather than token
whitespace. It may contain ASCII or Unicode whitespace, which is preserved and
matched exactly; implementations MUST NOT trim it. Thus `costs[Unit Cost]`
refers to the header `Unit Cost`. In a current-row selector, `@` is the first
selector character and the complete remainder is the header, so
`[@Unit Cost]` refers to that same header in the current row. The special
selectors are recognized only by the exact bracket contents `#Headers` and
`#Data`.

Formula literals are:

- the finite number grammar from [Numbers](#111-numbers), without a leading
  sign (signs are operators);
- case-insensitive bare `TRUE` and `FALSE`. A following `!` or `[` instead
  makes `true` or `false` a contextual sheet or table identifier;
- the core error tokens from [Errors](#113-errors); and
- double-quoted text. Within formula text, `""` represents one `"` and a bare
  newline or unmatched `"` is invalid. Backslash has no special meaning.

Function names contain ASCII letters, digits, and underscores, begin with a
letter, and are case-insensitive. Canonical spelling is uppercase. Bare
workbook names use the lowercase identifier grammar in section 4.1 and remain
case-sensitive. A function call is distinguished from a name by its following
`(`. Commas separate function arguments; comma unions and array literals are
not part of this profile.

A syntactically malformed formula makes the document invalid and produces an
`MS2202` diagnostic connected to both the formula source span and its cell.
Malformed formulas are not converted into a new runtime error value. Unknown
function or workbook names are syntactically valid and are handled during
resolution and evaluation.

### 13.3 Operators and precedence

From highest to lowest precedence, portable A1 formulas support:

1. Parentheses, literals, references, and function calls
2. Exponentiation: `^`
3. Unary signs: `+`, `-`
4. Multiplication and division: `*`, `/`
5. Addition and subtraction: `+`, `-`
6. String concatenation: `&`
7. Comparisons: `=`, `<>`, `<`, `<=`, `>`, `>=`

Exponentiation associates right-to-left. The other binary arithmetic and
concatenation operators associate left-to-right. Comparisons do not chain: an
unparenthesized expression such as `1<2<3` is invalid. Unary signs may appear on
the right of exponentiation. Consequently, `-2^2` is `-(2^2)`, `(-2)^2` is
`4`, and `2^-2` is `0.25`.

Arithmetic operands use this numeric coercion:

| Input | Numeric value |
| --- | --- |
| Number | Unchanged |
| Blank | `0` |
| Boolean | `TRUE` is `1`; `FALSE` is `0` |
| Text, Date, DateTime | `#VALUE!` |
| Error | The same error |

Division by zero returns `#DIV/0!`. `0^0` is `1`; zero raised to a negative
power returns `#DIV/0!`; and a negative base raised to a non-integral power
returns `#NUM!`. Any arithmetic result that is NaN or infinite returns
`#NUM!`.

Concatenation converts Blank to empty text, Number to its canonical finite
number spelling, Boolean to `TRUE` or `FALSE`, Date to `YYYY-MM-DD`, and
DateTime to its canonical RFC 3339 spelling with its stored offset. Text is
unchanged and Error propagates.

Equality compares values of the same type exactly. Blank equals only Blank,
and text comparison is case-sensitive lexicographic comparison by Unicode
scalar value without normalization. Values of different non-error types are
unequal; `<>` is the negation of `=`. DateTime equality compares both the
represented instant and the stored UTC offset, so two spellings of the same
instant with different offsets are unequal. Ordering comparisons require two
values of the same type and support Number, Text, Boolean (`FALSE` before
`TRUE`), Date (civil chronology), and DateTime (instant chronology, regardless
of stored offset). Other ordering comparisons return `#VALUE!`. Errors
propagate before comparison.

### 13.4 References and ranges

Formulas may reference:

- cells and ranges on the current sheet;
- cells and ranges using stable sheet identifiers;
- workbook named ranges;
- table columns and regions; and
- current-row table cells.

A sheet qualifier applies to an entire reference, as in `inputs!A1:B5`.
Qualifying only one range endpoint is invalid. Absolute markers affect copying
and structural editing but not lookup of the authored formula: `$A$1`, `$A1`,
and `A$1` all read `A1` before a copy adjustment.

A reference's authored syntax determines its kind; resolved cardinality does
not change it. Cell syntax such as `A1` evaluates to one scalar. Any explicit
range or area syntax, including `A1:A1`, a named target authored as
`sheet!A1:A1`, and a table column or region that currently has one row,
evaluates to an internal
rectangular Range traversed in row-major order. There is no implicit
intersection or implicit array result: using a Range where a scalar is required
returns `#VALUE!`. Only functions whose signatures accept ranges may consume
one. This distinction remains stable when a table grows or shrinks.

Structured selectors `#Headers` and `#Data` use that exact spelling. Table and
header names are case-sensitive; `]]` represents `]` within a header. A
current-row reference is resolvable only for a formula evaluated in a data row
of that same table. A qualifier on `table[@Header]` MUST identify the current
table. An unresolved sheet, table, header, or invalid current-row context
produces `MS2103` during validation and `#REF!` if evaluation is requested. An
unresolved bare workbook name produces `MS2103` and evaluates to `#NAME?`.

External workbook references, whole-row or whole-column references, range
unions, intersections, and network references are not part of the core.

### 13.5 Evaluation and error propagation

Formula evaluation has the scalar types Blank, Text, Number, Boolean, Date,
DateTime, and Error, plus the internal Range described above. It does not add a
date serial-number type or equate Blank with empty Text.

Required operands and function arguments are evaluated left-to-right. Ranges
are traversed row-major. Unless a function explicitly inspects or counts an
error, the first evaluated error is returned; errors have no severity ranking.
`IF`, `IFERROR`, `AND`, and `OR` are the lazy exceptions described below.

Runtime failures use these errors:

| Condition | Result |
| --- | --- |
| Division or modulo by zero | `#DIV/0!` |
| Lookup has no match | `#N/A` |
| Unknown function or workbook name | `#NAME?` |
| Non-finite result or numeric/date domain failure | `#NUM!` |
| Unresolved or out-of-bounds reference | `#REF!` |
| Wrong type, arity, or range shape | `#VALUE!` |
| Circular dependency | `#CIRC!` |

A calculator MUST discover dependencies from every syntactic reference,
including references in a branch that may not be evaluated. It MUST evaluate
acyclic dependencies in dependency order. Every formula cell in a strongly
connected component, including a self-loop, returns `#CIRC!`; dependents
outside that component observe and propagate that error normally. Iterative
calculation is an extension.

Formulas are pure and MUST NOT perform I/O. A calculator MUST NOT silently
substitute cached values for unsupported formulas because core files do not
contain authoritative caches.

### 13.6 Required functions in `portable-a1@1`

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

Except where stated otherwise, an arity mismatch returns `#VALUE!`, a required
scalar receiving a range returns `#VALUE!`, and an evaluated Error propagates.

#### 13.6.1 Aggregation

The aggregation functions take one or more scalar or range arguments and
flatten ranges row-major. `SUM`, `AVERAGE`, `MIN`, and `MAX` use Number values,
ignore Blank, Text, Boolean, Date, and DateTime values, and propagate Error.
`SUM` returns `0` if there are no numbers; `AVERAGE` returns `#DIV/0!`; and
`MIN` and `MAX` return `#VALUE!`. Accumulation that becomes non-finite returns
`#NUM!`.

`COUNT` counts Number values, ignores non-error values of every other type, and
propagates Error. `COUNTA` counts every non-Blank value, including empty Text
and Error; it therefore does not propagate an error merely because it counts
one.

#### 13.6.2 Logic

Logical coercion keeps Boolean unchanged, treats Blank and numeric zero as
false, and treats every other Number as true. Text, Date, DateTime, and Range in
a scalar position return `#VALUE!`.

`IF(condition, when_true, when_false)` evaluates the condition and exactly one
branch. `IFERROR(value, fallback)` returns `fallback` only when `value`
evaluates to any Error; its unused expression is not evaluated. `NOT` accepts
exactly one scalar.

`AND` and `OR` accept one or more scalar or range arguments and traverse them
left-to-right and row-major. They apply logical coercion to each value. `AND`
returns immediately on false and `OR` immediately on true, so an error after a
decisive value is not evaluated. An error encountered first propagates.

#### 13.6.3 Numeric

`ABS(value)` and `INT(value)` accept one numerically coercible scalar. `INT`
rounds toward negative infinity. `MOD(number, divisor)` uses
`number - divisor * floor(number / divisor)` and therefore has the divisor's
sign; a zero divisor returns `#DIV/0!`.

`ROUND(number, digits)`, `ROUNDUP(number, digits)`, and
`ROUNDDOWN(number, digits)` require exactly two arguments. The first is
numerically coercible. `digits` MUST be an integral Number from `-308` through
`308`; otherwise the result is `#NUM!`. Positive digits select decimal places
and negative digits select places to the left of the decimal point. `ROUND`
breaks exact halfway cases away from zero, `ROUNDUP` moves away from zero, and
`ROUNDDOWN` moves toward zero. These operations start with the exact binary64
input value and return the nearest representable finite binary64 result; a
non-finite intermediate or result is `#NUM!`.

#### 13.6.4 Text

`CONCAT` takes one or more scalar or range arguments, flattens ranges row-major,
and applies the concatenation conversion from section 13.3. The other text
functions take scalar input and use that same conversion.

`LEFT(text[, count])` and `RIGHT(text[, count])` default `count` to `1`.
`MID(text, start, count)` uses a one-based `start`. Counts MUST be non-negative
integral Numbers and `start` MUST be a positive integral Number; violations
return `#NUM!`. A position beyond the end returns empty Text. Positions and
`LEN` count Unicode scalar values, not bytes, UTF-16 code units, or grapheme
clusters.

`LOWER` and `UPPER` change only ASCII `A` through `Z` or `a` through `z`;
non-ASCII text is unchanged. `TRIM` removes leading and trailing U+0020 SPACE
characters and collapses each internal run of U+0020 to one. It does not alter
tabs, line breaks, or other Unicode whitespace.

#### 13.6.5 Lookup

`INDEX(array, index)` requires a one-row or one-column range and uses one-based
row-major position. `INDEX(array, row, column)` accepts any finite rectangular
range and uses one-based row and column positions. Indices MUST be positive
integral Numbers; an index outside the range returns `#REF!`, while a wrong
shape or type returns `#VALUE!`. The selected cell's calculated typed value is
returned.

`MATCH(value, array[, match_type])` requires a one-row or one-column range and
scans in its natural order. The omitted `match_type` is `0`; `0` is the only
mode in this profile and any other value returns `#VALUE!`. Matching uses the
equality rules in section 13.3 and returns the one-based position of the first
match. An error encountered before a match propagates. No match returns
`#N/A`.

#### 13.6.6 Dates

`DATE(year, month, day)` requires three integral Numbers and constructs a strict
proleptic Gregorian date. Years are `1` through `9999`; month and day MUST form
a valid date and are not normalized across boundaries. Invalid inputs return
`#NUM!`.

`YEAR`, `MONTH`, and `DAY` each require exactly one Date or DateTime. For a
DateTime they inspect the civil components in its stored offset, not the
workbook display timezone. They return a Number.

#### 13.6.7 Inspection

`ISBLANK`, `ISNUMBER`, `ISTEXT`, and `ISERROR` each inspect exactly one scalar
without propagating that scalar if it is an Error. Only the named type returns
true; in particular, empty Text is not Blank. A range argument returns
`#VALUE!`.

Volatile functions such as `NOW`, `TODAY`, `RAND`, and `RANDBETWEEN` are not in
the core profile. They would make identical source produce different results at
different times.

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
11. Normalize directive whitespace and values. Implementations limited to
    Milestone 1 MAY preserve formula-body spelling; a Milestone 2 canonicalizer
    MUST use the formula rules below.
12. Normalize CRLF line endings to LF in opaque extension payloads while
    preserving all other payload bytes.
13. Exclude save times, generator versions, random IDs, and other volatile
    metadata unless explicitly authored as extension data.

For `portable-a1@1`, canonical formula output MUST be produced from the parsed
AST and MUST:

- begin with exactly one `=` and contain no insignificant whitespace;
- use uppercase function names, Boolean literals, and A1 column letters while
  retaining lowercase stable sheet, name, and table identifiers;
- retain authored `$` markers and emit decimal row numbers without leading
  zeroes;
- use the shortest finite decimal spelling that round-trips to the formula's
  binary64 Number value, and use the canonical core error tokens;
- delimit text with `"`, encode an embedded `"` as `""`, and preserve every
  other text scalar exactly;
- emit structured selectors with exact `#Headers`, `#Data`, and `@` spelling,
  preserve header content exactly, and encode `]` within a header as `]]`;
- separate function arguments with `,` and emit binary and unary operators
  without surrounding whitespace; and
- remove redundant parentheses while retaining every parenthesis required to
  preserve the AST under the precedence and associativity rules in section
  13.3. In particular, it MUST preserve distinctions such as `-(2^2)` versus
  `(-2)^2`, right-associative exponentiation, and the operand order of
  non-associative operators.

Canonicalizing a formula MUST NOT resolve names, expand ranges, materialize a
fill, or otherwise replace authored reference syntax with calculated targets.

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

### 19.1 Transactional edit contract

An edit-capable implementation applies an explicit transaction to a parsed,
valid source document. A transaction MUST either commit all of its semantic
operations and source patches or commit none of them. It MUST NOT return a
partly applied workbook after a failed validation, failed precondition, or
resource-limit diagnostic.

A committed edit result MUST contain:

- the semantic operations that were applied;
- a patch plan of byte ranges in the original source;
- the resulting semantic workbook and diagnostics; and
- an inverse transaction suitable for undo.

Each patch is a half-open UTF-8 byte range `[start, end)` plus replacement
bytes. Patch ranges MUST be sorted by increasing `start`, MUST NOT overlap,
and are interpreted against the same original source snapshot. Insertions have
`start = end`. Applying the complete plan in descending range order produces
the result without invalidating a later offset. A no-op transaction MUST return
an empty patch plan.

An editor MUST preserve all bytes outside its patch plan. In particular, an
edit that does not target an opaque extension MUST preserve its payload,
including original line endings, byte-for-byte. A lossless edit MUST NOT use
canonical serialization as an implementation shortcut.

#### 19.1.1 Authored-cell replacement and table append

`SetCell(sheet, coordinate, value)` is defined only for an existing authored
CSV field. It replaces that one field token, including only the quoting needed
for the replacement value under section 9.1. The operation MUST preserve the
rest of the CSV record, surrounding source spelling, and every unrelated
block. An authored blank field is an existing field and may be replaced.

`SetCell` MUST refuse an absent coordinate, a coordinate represented only by an
`@fill` virtual cell, and a coordinate whose containing CSV field cannot be
uniquely identified. It MUST NOT materialize a new block, split or grow an
unnamed block, or replace a formula merely because its calculated display value
was selected. Setting a formula replaces the authored formula field text; it
does not canonicalize adjacent formulas or resolve the formula to a value.

`AppendTableRow(table, fields)` appends exactly one CSV record to an existing
table. `fields` MUST have the table's header width, and each field is encoded
using section 9.1. The record MUST be inserted immediately before that table's
own physical `@end`, using the table body's line-ending convention when one is
available. The operation MUST NOT append to an unnamed block, another table,
or after a sheet boundary.

#### 19.1.2 Stable identifiers, labels, and references

`RenameSheetLabel(sheet, label)` changes only the quoted label in that sheet's
`@sheet` declaration. Labels are presentation text; this operation MUST NOT
rewrite formulas, names, comments, strings, or extension payloads.

`RenameSheetId(old, new)` and `RenameNameId(old, new)` are atomic identifier
transactions. `new` MUST be valid and unused in its namespace. In addition to
the declaration, the editor MUST rewrite every resolved core reference token
whose semantic target is the renamed sheet or name, including formula fields,
`@fill` formulas, and `@name` definitions where applicable. It MUST NOT rewrite
text literals, comments, labels, opaque extension payloads, or an unrelated
identifier that merely has the same byte spelling. A transaction that cannot
locate every required core reference MUST fail without patches rather than
leave stale references.

Changing a named-range target is a declaration edit. It MUST update the named
range definition and MUST NOT expand the name into every formula use.

#### 19.1.3 Block movement and reference policy

`MoveBlock(sheet, source_footprint, destination_anchor)` is defined only when
`source_footprint` exactly matches one declared `@block` or `@table` footprint.
Moving a subset of a block or table, a range crossing two footprints, an absent
range, or a destination that overlaps another footprint MUST be refused. The
source patch changes the owning block or table anchor; it MUST NOT manufacture
blank cells or rewrite its CSV body.

Moving a complete footprint preserves dependency identity as follows:

1. Every A1 reference endpoint that denotes a cell in the moved footprint is
   rewritten to the corresponding destination coordinate, regardless of `$`
   markers. This applies to references outside and inside the moved footprint.
2. For an A1 endpoint outside the moved footprint but written in a formula that
   itself moves with the footprint, the row and column displacement is applied
   only to relative endpoint components. `$` fixes the corresponding component.
3. A range endpoint is handled independently under rules 1 and 2. The editor
   MUST preserve range ordering; if a rewrite would invert an endpoint order or
   produce an invalid coordinate, the move MUST fail atomically.

The same policy applies to direct A1 targets in named-range definitions. Table
and sheet identifiers remain stable when a block moves and therefore do not
need textual rewriting. Draft 0.1 intentionally does not define insertion or
deletion of arbitrary rows, columns, or partial cells; implementations MUST
report those requests as unsupported rather than infer broader spreadsheet
semantics from `MoveBlock`.

#### 19.1.4 Styles

`ApplyStyle(sheet, target, style)` refers to an existing workbook-scoped style
identifier and creates one focused `@apply target style` directive in the
target sheet. It MUST NOT mutate or widen an existing application, because that
could restyle cells outside the requested target. The new directive is placed
after the last authored `@apply` in that sheet, or after the last sheet item if
the sheet has none, preserving nearby line-ending and indentation conventions.

An editor MAY offer `DefineStyle` as a separate transaction. It MUST either
reuse an existing style with exactly the requested declared properties or add a
new valid, unused style declaration before the first sheet. It MUST NOT modify
an existing named style to satisfy a one-range formatting request. Styles are
compared by their declared core property map; order and whitespace are not a
reason to duplicate a style.

#### 19.1.5 Undo, redo, external changes, and rebasing

The inverse returned for a committed transaction MUST restore the immediately
previous semantic source state when its post-transaction preconditions still
hold. Redo reapplies the original transaction only when its pre-transaction
preconditions hold. Undo and redo are transactions and therefore use the same
atomic validation and patch-plan rules as a new edit.

Before applying a patch plan, an editor MUST compare a source fingerprint and
the expected bytes of every affected span with the current file. If either no
longer matches, it MUST reparse the current source before doing anything else.
It MAY rebase the semantic transaction only when every targeted stable object
still resolves uniquely, all operation-specific preconditions hold, and a new
validated patch plan can be calculated from the current source. Otherwise it
MUST return a conflict diagnostic and no patches. It MUST NOT overwrite an
externally changed file with a stale full-document snapshot.

### 19.2 Semantic diff and equivalence

`semantic_diff(left, right)` compares parsed semantic projections and reports
added, removed, and changed core objects by stable sheet, table, name, style,
and coordinate identities. It MUST report a parse or validation failure for an
input that cannot produce a complete semantic projection.

Two valid documents are core-semantically equivalent when they have the same
workbook settings; ordered sheets with the same identifiers and labels; the
same authored cells, table definitions, fills, names, styles, geometry, and
resolved style effects; and formulas with equivalent `portable-a1@1` ASTs.
Comments, blank lines, directive whitespace, CSV quoting choices, source line
endings, and equivalent formula spelling do not make documents semantically
different. Sheet order, table row order, formula versus scalar kind, authored
blank versus absence, and style precedence do make them different.

Opaque extensions are not interpreted by the core. A semantic diff MUST still
report a changed extension declaration, scope, placement, or payload bytes as
an opaque-extension change; it MUST NOT claim the documents are fully
equivalent when those bytes differ. The diff output SHOULD include a concise
human explanation and stable structured object identifiers suitable for tools.

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

### 20.4 Browser session and GUI

An implementation that claims browser-session conformance MUST expose the
workbook through a revisioned session. It MAY choose its own programming
language, UI framework, and public method names, but it MUST provide the
following observable capabilities as bounded, batched operations:

- workbook metadata, including sheets in source order, stable sheet IDs,
  labels, names, and diagnostics;
- a visible-region projection for one bounded rectangular coordinate range;
- calculation for a bounded set of targets or regions; and
- source-aware semantic edits that return the resulting revision, diagnostics,
  and the lossless patch plan described in section 19.1.

The public binding MUST provide generated TypeScript declarations for those
operations and their data records. The declarations MUST represent identifiers,
coordinates, source byte spans, diagnostic codes, values, and optional values
without requiring a JavaScript consumer to know a Rust layout or pointer.

#### 20.4.1 Sparse visible regions

A general browser binding MAY permit callers to select response layers. The
reference `marksheet-worker@1` profile instead accepts a sheet ID and an
inclusive, finite row-and-column rectangle at the session revision, and always
returns the complete standard projection: authored values, virtual fills,
calculated values, resolved presentation, effective geometry, source links,
and diagnostics. The response MUST identify the revision and sheet it
represents. It MUST return only sparse authored cells, virtual fill cells,
calculated results, and style or geometry intervals that intersect the
requested rectangle. It MUST NOT enumerate coordinates absent from both source
and fills merely to describe a blank grid.

An authored blank field is a returned authored cell whose value is Blank; it is
not interchangeable with an absent coordinate. A virtual cell MUST identify
its destination coordinate and its owning `@fill` directive, and MUST NOT
pretend to have an authored CSV-field source span. A response MAY return
compact rectangles or runs for styles and geometry, but their intersection with
the requested rectangle MUST have exactly the same presentation effect as the
underlying directives.

Visible-region work MUST be bounded by the request limits and the sparse items
that intersect it, not by the greatest used row or column of the sheet. A
renderer MAY create default visual rows and columns for the viewport and a
finite overscan margin, but it MUST NOT allocate a matrix covering the sheet
extent. A request exceeding a configured viewport limit MUST fail with a
diagnostic or explicit limit result before such an allocation is attempted.

For each returned cell, a browser session MUST keep these concepts distinct:

1. the authored scalar, Blank, or formula source, when an authored field
   exists;
2. the virtual formula origin, when a fill supplies the cell;
3. the calculated typed value and error state, when calculation was requested;
   and
4. the resolved presentation style and effective row/column geometry.

The resolved style is computed by merging styles within one `@apply`
left-to-right and then applying later overlapping `@apply` directives one
property at a time, as required by section 15. Effective column width and row
height are resolved independently, with later overlapping declarations taking
precedence as required by section 16. An unspecified property or geometry value
MUST remain distinguishable from a renderer's visual default.

Calculated values are display data, not editable source. Editing a formula
cell MUST target its authored formula field; editing a fill-only virtual cell
MUST be refused under the same rule as `SetCell` in section 19.1.1. Number
formatting MAY be a renderer concern, but it MUST NOT replace the typed
calculated value exposed to clients.

#### 20.4.2 Source links, navigation, and edits

Every authored cell and formula reported by a session MUST carry a source link
to its CSV field and, when available, its formula-token span. Every virtual
cell MUST link to the relevant fill directive and destination coordinate.
Every diagnostic shown by the browser MUST retain its primary source span and
related source or cell locations supplied by the core. Selecting a cell,
formula-bar value, name, or diagnostic MUST be able to navigate to the linked
source span when one exists.

Sheet tabs MUST follow workbook source order. A name box MUST resolve an
entered coordinate, finite range, or declared name using the active workbook
revision and report an invalid or ambiguous target without changing selection.
A formula bar MUST show the authored formula source, including its leading
`=`, rather than a calculated replacement. A synchronized source view is
optional, but when present it MUST display the exact session source bytes and
must update only through a reparse or a committed source patch; it MUST NOT
silently canonicalize unrelated source.

Basic style and geometry controls MUST use the same source-aware transaction
boundary as cell and name edits. A successful GUI edit therefore yields a
focused patch plan and a new revision; a failed edit leaves both the visible
session state and source bytes unchanged.

#### 20.4.3 Worker protocol, revisions, and cancellation

Browser implementations MUST run parsing and calculation away from the UI
thread. Worker messages MUST carry a versioned protocol identifier, a client
request ID, and the revision against which the request was made. Replies MUST
echo enough of that identity for a client to discard stale results. A protocol
identifier of `marksheet-worker@1` is reserved for the reference browser
binding.

A source replacement or committed edit creates a new revision. A cancelled,
superseded, or stale parse/calculation request MUST NOT replace the active
workbook, calculation results, diagnostics, source links, or selection state.
Cancellation is best-effort for CPU work, but its observable result MUST be
either an explicit cancellation reply or a reply that the client can prove is
stale from its request ID and revision. A failed parse MAY return recovered
diagnostics, but it MUST NOT be represented as a valid editable workbook.

#### 20.4.4 Local-file external-change protection

When a browser session opens a local file, it MUST retain the exact source
bytes and fingerprint on which its revision is based. Before writing, it MUST
compare the current file bytes with that base snapshot. If they differ, it MUST
reparse the current file before any write and MAY rebase the pending semantic
transaction only under the conditions in section 19.1.5. Otherwise it MUST
report a conflict, perform no write, and make the external change visible to
the user. It MUST NOT overwrite an externally changed file with an in-memory
full-document snapshot.

A successful local save writes only the source produced by the validated patch
plan or an explicit user-requested canonicalization. A no-op save MUST NOT
write a replacement document merely because it was opened in the browser.

### 20.5 Lossless editor

A conforming lossless editor MUST satisfy the preservation requirements in
[Lossless editing and source maps](#19-lossless-editing-and-source-maps).
An editor that implements transactions MUST additionally satisfy section 19.1;
an implementation that exposes semantic diff MUST apply section 19.2.

### 20.6 Converter

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
