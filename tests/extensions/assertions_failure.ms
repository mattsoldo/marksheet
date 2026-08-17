#!marksheet 0.1
@use assertions@1

@sheet inputs "Inputs"
@block A1 csv
Value,Text
5,hello
@end
@extension assertions@1 "checks"
assert A2 > 9
assert B2 = "goodbye"
@end
