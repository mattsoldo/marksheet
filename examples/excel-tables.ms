#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

# Curated from test-corpus/real-world/xlsx/poi/ExcelTables.xlsx.
@style header bold=true fill="#E8EEF7" align=center

@sheet exceltable "Excel Table"
@table people A1 csv
Name,Age,Age group
Anton,44,
Bertha,33,
Cäsar,21,
@end
@fill people[Age group] =INT([@Age]/10)
@apply people[#Headers] header
@column A width=18
@column B:C width=12
