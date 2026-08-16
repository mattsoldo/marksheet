#!marksheet 0.1
@name tax_rate = data!A2
@sheet data "Data"
@block A1 csv
Rate,Double
0.2,=tax_rate*2
@end
