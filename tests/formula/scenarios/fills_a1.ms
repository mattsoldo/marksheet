#!marksheet 0.1
@book formula-profile="portable-a1@1"

@sheet main "Fills"

@block A1 csv
1,10,,,
2,20,,,
3,30,,,
@end

@fill C1:C3 =A1*B1
@fill D1:D3 =$A$1+B1
@fill E1:E3 =A$1+$B1

