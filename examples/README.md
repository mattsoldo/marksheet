# Examples

These six small workbooks are curated, portable Marksheet views of real Excel
corpus material. They deliberately favor ordinary working sheets over large or
feature-dense test files. Their source XLSX paths are recorded in each file.

| Example | Corpus source | Delimiter |
| --- | --- | --- |
| [`budget.ms`](budget.ms) | Hand-authored product example | pipe |
| [`invoice-basic.ms`](invoice-basic.ms) | Google Sheets `invoice_basic.xlsx` | pipe |
| [`stamp-catalog.ms`](stamp-catalog.ms) | PhpSpreadsheet `26template.xlsx` | pipe |
| [`census-population.ms`](census-population.ms) | U.S. Census state population workbook | pipe |
| [`excel-tables.ms`](excel-tables.ms) | Apache POI `ExcelTables.xlsx` | csv |
| [`dates-1904.ms`](dates-1904.ms) | calamine `date_1904.xlsx` | csv |

Pipe blocks use `|` only as their field delimiter. A comma is ordinary data in
a pipe block, while a literal pipe is CSV-quoted. CSV blocks retain RFC 4180
comma separation for interoperability with existing tools.
