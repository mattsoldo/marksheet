#!marksheet 0.1
@name tax_rate = data!A2
@sheet data "Data"
@block A1 csv
Value,Formula
1,"=IF(tax_rate>0, ""rate text"", tax_rate)"
@end
