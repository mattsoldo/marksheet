#!marksheet 0.1
@book formula-profile="portable-a1@1"
@name prices = inventory[Price]

@sheet data "Data"

@table inventory A1 csv
Item,Price,Qty,Total
Pens,2,10,
Paper,5,3,
@end

@fill inventory[Total] =[@Price]*[@Qty]

@sheet summary "Summary"

@block A1 csv
Metric,Value
Total,=SUM(inventory[Total])
Average price,=AVERAGE(prices)
Header count,=COUNTA(inventory[#Headers])
Second item,"=INDEX(inventory[Item],2)"
@end
