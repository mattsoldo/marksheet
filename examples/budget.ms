#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

# Reusable presentation styles.
@style header bold=true fill="#e8eef7" align=center
@style money number=currency currency="USD" decimals=2 align=right
@style percent number=percent decimals=0 align=right

# Names are workbook scoped and may refer forward to cells and tables.
@name tax_rate = inputs!G2
@name cost_values = costs[Cost]

@sheet inputs "Inputs"

@table costs A1 csv
Item,Cost,Quantity,Subtotal
Rent,1500,1,
Utilities,200,1,
Groceries,360,1,
@end

@fill costs[Subtotal] =[@Cost]*[@Quantity]
@apply costs[#Headers] header
@apply costs[Cost] money
@apply costs[Subtotal] money
@column A width=18
@column B:D width=12

# This second block demonstrates sparse placement on the same sheet.
@block F1 csv
Setting,Value
Tax rate,0.2
@end

@apply F1:G1 header
@apply G2 percent
@column F width=14
@column G width=10

@sheet summary "Summary"

@block A1 csv
Metric,Value
Total costs,=SUM(costs[Subtotal])
Average line item,=AVERAGE(cost_values)
After tax,=B2*(1-tax_rate)
@end

@apply A1:B1 header
@apply B2:B4 money
@column A width=20
@column B width=14
