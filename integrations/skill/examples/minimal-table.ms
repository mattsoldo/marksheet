#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

@style header bold=true fill="#e8eef7" align=center
@style money number=currency currency="USD" decimals=2 align=right

@sheet sales "Sales"
@table orders A1 csv
Order,Units,Price,Total
First,2,12.5,
@end

@fill orders[Total] =[@Units]*[@Price]
@apply orders[#Headers] header
@apply orders[Price] money
@apply orders[Total] money
