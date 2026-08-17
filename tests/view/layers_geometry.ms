#!marksheet 0.1
@style base bold=true text-color="#111111" fill="#ffffff"
@style emphasis bold=false fill="#ffff00"
@sheet view "View"
@block A1 csv
Input,Result,Virtual
2,=A2*3,
@end
@fill C2 =B2*2
@apply A1:C2 base
@apply B2:C2 emphasis
@column A:C width=10
@column B width=24
@row 1:2 height=18
@row 2 height=30
