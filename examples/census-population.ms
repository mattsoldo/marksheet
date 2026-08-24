#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"

# Curated from test-corpus/real-world/xlsx/govdata/us_census_state_population.xlsx.
@style header bold=true fill="#E8EEF7" align=center
@style integer number=integer align=right

@sheet population "2023 Population Estimates"
@block A1 pipe
Geographic Area|2020 Estimates Base|2020 Estimate
United States|331464948|331526933
Northeast|57614141|57430477
Midwest|68987296|68969794
South|126268529|126465281
West|78594982|78661381
@end
@apply A1:C1 header
@apply B2:C6 integer
@column A width=20
@column B:C width=20
