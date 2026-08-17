#!marksheet 0.1
@use assertions@1

@sheet inputs "Inputs"
@block A1 csv
Value
5
@end
@extension assertions@1 "checks"
assert A2 =
assert A2 == 5
assert inputs!A2 = 5
@end
