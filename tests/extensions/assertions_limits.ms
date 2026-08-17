#!marksheet 0.1
@use assertions@1

@sheet inputs "Inputs"
@block A1 csv
Value
1
@end
@extension assertions@1 "checks"
assert A2 = 1
assert A2 = 1
assert A2 = 1
@end
