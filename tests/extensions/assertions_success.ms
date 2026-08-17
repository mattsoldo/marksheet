#!marksheet 0.1
@use assertions@1

@sheet inputs "Inputs"
@block A1 csv
Value,Flag,Date
5,true,2026-08-16
@end
@extension assertions@1 "checks"
# typed equality and ordering are deterministic
assert A2 = 5
assert B2 = true
assert C2 >= 2026-08-16
@end
