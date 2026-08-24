#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

# Curated from test-corpus/real-world/xlsx/phpspreadsheet/26template.xlsx.
@style title bold=true fill="#FFFF00" font-size=18 align=center
@style money number=currency currency="EUR" decimals=0 align=right

@sheet catalog "Stamp Catalog"
@block A1 pipe
"Rare (sjældne) ""Stamps (切手) & Letters"" for sale"||
No|Item|Price (€)
10001|1851 Denmark. 4 Rigsbankskilling Ferslew printing.|300
10002|1855 Denmark. Copenhagen foot-post cover.|3500
10003|1852 Denmark. Thiele 2 Rigsbankskilling type 2.|5800
10004|1871 Japan. 200 mon dragon cover from Yokohama to Tokyo.|4300
@end
@apply A1:C1 title
@apply C3:C6 money
@column A width=10
@column B width=64
@column C width=12
