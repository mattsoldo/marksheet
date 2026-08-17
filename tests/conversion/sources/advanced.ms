#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"
@use charts@1

@sheet report "Report"
@block A1 csv
Month,Revenue
Jan,100
Feb,125
@end
@extension charts@1 "revenue"
type=bar
source=report!A1:B3
@end
