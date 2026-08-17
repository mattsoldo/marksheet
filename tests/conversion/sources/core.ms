#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"
@style money number=currency currency="USD" decimals=2 align=right
@name tax_rate = inputs!G2

@sheet inputs "Inputs"
@table sales A1 csv
Item,Amount,Quantity,Total
Widget,12.5,2,
Gadget,3,4,
@end
@fill sales[Total] =[@Amount]*[@Quantity]
@apply sales[Amount] money
@apply sales[Total] money
@column A width=20
@column B:D width=12
@row 1 height=24

@block F1 csv
Setting,Value
Tax,0.2
@end

@sheet summary "Summary"
@block A1 csv
Metric,Value
Total,=SUM(sales[Total])
AfterTax,=B2*(1-tax_rate)
@end
