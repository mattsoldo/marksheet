#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

# Curated from test-corpus/real-world/xlsx/gsheets/invoice_basic.xlsx.
@style header bold=true fill="#93C47D" align=center
@style money number=currency currency="USD" decimals=2 align=right

@sheet invoice "Invoice"
@block A1 pipe
Invoice|Invoice Number:|123456
Company Name|Invoice Date:|2022-03-31
Example Street|Payment Due:|2022-04-30
Miami, Florida, USA|Invoice To:|Company XYZ
@end

@block A7 pipe
Description|Quantity|Rate|Amount
Example product 1|3|125|=B8*C8
Example product 2|6|75|=B9*C9
Service 1|4|150|=B10*C10
@end
@apply A7:D7 header
@apply C8:D10 money
@column A width=28
@column B:D width=14
