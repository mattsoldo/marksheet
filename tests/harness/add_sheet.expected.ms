#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

@name tax_rate = inputs!F2

@sheet inputs "Inputs"
@table costs A1 csv
Item,Cost,Quantity,Subtotal
Rent,1500,1,
Utilities,200,1,
@end
@fill costs[Subtotal] =[@Cost]*[@Quantity]

@block E1 csv
Setting,Value
Tax rate,0.2
@end

@sheet summary "Summary"
@block A1 csv
Metric,Value
After tax,=SUM(costs[Subtotal])*(1-tax_rate)
@end

@sheet notes "Notes"
@block A1 csv
Note,Score
Reviewed by automation,
@end
@fill B2:B2 =1
