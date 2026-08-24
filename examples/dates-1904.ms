#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

# Curated from test-corpus/real-world/xlsx/calamine/date_1904.xlsx.
@style header bold=true fill="#E8EEF7" align=center
@style date number=date

@sheet dates "1904 Date System"
@block A1 csv
Date,Measurement,Elapsed days
2021-01-01,15,0
2021-01-02,16,=A3-A2
2021-01-03,17,=A4-A2
@end
@apply A1:C1 header
@apply A2:A4 date
@column A width=14
@column B:C width=14
