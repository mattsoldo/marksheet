#!marksheet 0.1
# Preserve this comment exactly.
@use vendor_data@3

@sheet raw "Raw data"
@extension vendor_data@3 "opaque"
  key = value
  nested:
    - preserve spaces
@end

@block B2 csv
value
"text, with comma"
@end
