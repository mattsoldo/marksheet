#!marksheet 0.1
@book locale="en-US" timezone="UTC" formula-profile="portable-a1@1"
@style header bold=true fill="#E8EEF7" align=center
@style money number=currency currency="USD" decimals=2 align=right
@style note italic=true text-color="#666666" wrap=true font-size=10
@name unit_values = inventory[Unit]
@name summary_cell = summary!B2
@use archive@1
@extension archive@1 "workbook_metadata"
  stable=true
  owner=finance
@end

@sheet inputs "Inputs"

@table inventory A1 csv
Item,Unit,Quantity,Total
Widget,12.5,4,
Gadget,2,3,
@end
@fill inventory[Total] =[@Unit]*[@Quantity]
@apply inventory[#Headers] header
@apply inventory[Unit] money
@apply inventory[Total] money note
@column A width=18
@column B:D width=12.5
@row 1 height=24
@row 2:3 height=18

@block A10 csv
blank,forced_text,formula,error,boolean,number,date,datetime,text
,'00123,=A1+B1,#DIV/0!,true,-12.50e+2,2026-08-16,2026-08-16T10:30:00-04:00,hello
,'=literal,"=SUM(A1,B1)",#N/A,false,0,2026-08-17,2026-08-16T14:30:00Z,"comma, quote "" and text"
@end

@extension archive@1 "sheet_metadata"
  source=imports
@end

@sheet summary "Summary"

@block A1 csv
Metric,Value
Total,=SUM(inventory[Total])
Average,=AVERAGE(unit_values)
Linked,=inputs!A1
@end
@apply A1:B1 header
