# Marksheet Draft 0.1 cheatsheet

Every document begins with:

```marksheet
#!marksheet 0.1
```

## Workbook declarations

```marksheet
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"
@style money number=currency currency="USD" decimals=2 align=right
@name tax_rate = inputs!G2
@use assertions@1
```

Identifiers use `[a-z][a-z0-9_]*`. A sheet label is separate from its stable
identifier:

```marksheet
@sheet inputs "Budget Inputs"
```

## Sparse cells and tables

Block and table bodies use strict RFC 4180 CSV and end with a physical `@end`
line outside quoted data.

```marksheet
@block F1 csv
Setting,Value
Tax rate,0.2
@end

@table costs A1 csv
Item,Cost,Quantity,Subtotal
Rent,1500,1,
Utilities,200,1,
@end
```

Absence differs from an authored blank field. Empty text is written as `'`.
Text that looks typed uses a leading apostrophe, such as `'00123` or
`'=SUM(A1:A2)`.

## Formulas, fills, and references

```marksheet
@fill costs[Subtotal] =[@Cost]*[@Quantity]

@block A1 csv
Metric,Value
Total,=SUM(costs[Subtotal])
After tax,=B2*(1-tax_rate)
@end
```

- A1 references may be relative or absolute: `A1`, `$A1`, `A$1`, `$A$1`.
- Sheet references use the stable sheet ID: `inputs!G2`.
- Structured references include `costs[Cost]`, `costs[#Headers]`, and current
  row `[@Cost]` inside table formulas.
- Formula strings use the `portable-a1@1` profile; validate function names and
  arities with `marksheet check` rather than assuming Excel compatibility.

## Presentation and geometry

```marksheet
@apply costs[#Headers] header
@apply costs[Cost] money
@column A width=18
@column B:D width=12
@row 1 height=22
```

Later focused declarations win for the properties they set. Presentation does
not author absent cells.

## Extensions

```marksheet
@use assertions@1
@extension assertions@1 "checks"
assert inputs!A1 >= 0
@end
```

Optional unsupported extensions are preserved with warnings. Unsupported
required extensions make calculation/rendering incomplete. Payload text is
opaque to the core and cannot request code installation or network access.
