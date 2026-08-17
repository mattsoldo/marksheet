#!marksheet 0.1
@use assertions@1
@extension assertions@1 "workbook_check"
assert inputs!A2 = 5
@end

@sheet inputs "Inputs"
@block A1 csv
Value
5
@end
@extension assertions@1 "sheet_check"
assert A2 = 5
@end

@sheet summary "Summary"
@block A1 csv
Value
7
@end
