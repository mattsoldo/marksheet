//! Lossless parsing and canonical serialization for Marksheet source.
//!
//! Parsing accepts bytes, rather than `&str`, because invalid UTF-8 and a BOM
//! must be diagnosed without discarding the original source needed by a
//! lossless editor.

mod diagnostic;
mod format;
mod lower;
mod scanner;

pub mod cst;

use marksheet_model::{Diagnostic, Severity, Workbook};

pub use cst::Cst;
pub use format::canonicalize;
pub use lower::ParseOptions;

/// The complete result of one recoverable parse.
#[derive(Clone, Debug)]
pub struct ParsedDocument {
    source: Vec<u8>,
    pub cst: Cst,
    pub workbook: Option<Workbook>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ParsedDocument {
    /// Returns the exact input, including invalid UTF-8, BOMs, and original
    /// line-ending spelling.
    #[must_use]
    pub fn source_bytes(&self) -> &[u8] {
        &self.source
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

/// Scans source losslessly. Semantic lowering is added after the syntax tree is
/// complete; malformed input still returns a usable CST and exact source.
#[must_use]
pub fn parse(source: &[u8]) -> ParsedDocument {
    parse_with_options(source, &ParseOptions::default())
}

/// Parses with host capability information used for extension diagnostics.
#[must_use]
pub fn parse_with_options(source: &[u8], options: &ParseOptions) -> ParsedDocument {
    let scanned = scanner::scan(source);
    let (workbook, mut semantic_diagnostics) = lower::lower(source, &scanned.cst.nodes, options);
    let mut diagnostics = scanned.diagnostics;
    diagnostics.append(&mut semantic_diagnostics);
    ParsedDocument {
        source: source.to_vec(),
        cst: scanned.cst,
        workbook,
        diagnostics,
    }
}

/// Returns the exact source bytes without performing an implicit rewrite.
#[must_use]
pub fn lossless_bytes(document: &ParsedDocument) -> &[u8] {
    document.source_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use marksheet_model::{Coordinate, NameTarget, SheetId};

    #[test]
    fn complete_example_lowers_and_is_already_canonical() {
        let source = include_bytes!("../../../examples/budget.ms");
        let document = parse(source);
        assert!(
            !document.has_errors(),
            "{:?}",
            document
                .diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.code.as_str(), &diagnostic.message))
                .collect::<Vec<_>>()
        );
        let workbook = document.workbook.as_ref().expect("valid workbook");
        assert_eq!(workbook.sheets.len(), 2);
        assert_eq!(workbook.styles.len(), 3);
        assert_eq!(workbook.names.len(), 2);
        assert_eq!(canonicalize(&document).unwrap(), source);
    }

    #[test]
    fn canonicalization_is_idempotent_and_normalizes_crlf() {
        let source = b"#!marksheet 0.1\r\n@sheet s \"Sheet\"\r\n\r\n@block a1 csv\r\n\"a,b\",true\r\n@end\r\n";
        let document = parse(source);
        assert!(!document.has_errors());
        let once = canonicalize(&document).unwrap();
        assert!(!once.windows(2).any(|window| window == b"\r\n"));
        let twice = canonicalize(&parse(&once)).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn lossless_access_is_byte_identical_even_for_invalid_input() {
        let source = b"\xef\xbb\xbf#!marksheet 0.1\r\n\xff";
        let document = parse(source);
        assert_eq!(document.source_bytes(), source);
        assert!(document.has_errors());
    }

    #[test]
    fn table_column_fill_requires_at_least_one_data_row() {
        let source = b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@table empty A1 csv\nValue\n@end\n@fill empty[Value] =1\n";
        let document = parse(source);
        let codes: Vec<_> = document
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert_eq!(codes, ["MS2102"]);
    }

    #[test]
    fn structured_header_closing_brackets_must_be_escaped() {
        let valid = b"#!marksheet 0.1\n@name selected = costs[A]]B]\n@sheet s \"Sheet\"\n@table costs A1 csv\nA]B\nvalue\n@end\n";
        let valid_document = parse(valid);
        assert!(
            !valid_document.has_errors(),
            "{:?}",
            valid_document.diagnostics
        );
        assert_eq!(
            valid_document
                .workbook
                .as_ref()
                .expect("valid workbook")
                .names
                .len(),
            1
        );

        let invalid = b"#!marksheet 0.1\n@name selected = costs[A]B]\n@sheet s \"Sheet\"\n@table costs A1 csv\nA]B\nvalue\n@end\n";
        let invalid_document = parse(invalid);
        let codes: Vec<_> = invalid_document
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert_eq!(codes, ["MS2101"]);
    }

    #[test]
    fn table_apply_resolves_forward_only_within_its_sheet() {
        let forward = b"#!marksheet 0.1\n@style header bold=true\n@sheet s \"Sheet\"\n@apply future[Value] header\n@table future A1 csv\nValue\nitem\n@end\n";
        let forward_document = parse(forward);
        assert!(
            !forward_document.has_errors(),
            "{:?}",
            forward_document.diagnostics
        );

        let cross_sheet = b"#!marksheet 0.1\n@style header bold=true\n@sheet first \"First\"\n@table owned A1 csv\nValue\nitem\n@end\n@sheet second \"Second\"\n@apply owned[Value] header\n";
        let cross_sheet_document = parse(cross_sheet);
        let codes: Vec<_> = cross_sheet_document
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert_eq!(codes, ["MS2102"]);
    }

    #[test]
    fn canonical_geometry_normalizes_column_case_and_range_direction() {
        let source =
            b"#!marksheet 0.1\n\n@sheet s \"Sheet\"\n@column a:d width=12\n@row 3:1 height=18\n";
        let once = canonicalize(&parse(source)).expect("valid geometry");
        assert!(String::from_utf8_lossy(&once).contains("@column A:D width=12\n"));
        assert!(String::from_utf8_lossy(&once).contains("@row 1:3 height=18\n"));
        assert_eq!(canonicalize(&parse(&once)).unwrap(), once);
    }

    #[test]
    fn row_geometry_rejects_leading_zeroes() {
        let document = parse(b"#!marksheet 0.1\n@sheet s \"Sheet\"\n@row 01:2 height=10\n");
        assert_eq!(document.diagnostics[0].code.as_str(), "MS1202");
    }

    #[test]
    fn directive_numbers_require_json_number_grammar() {
        for invalid in [".5", "1.", "+1", "01"] {
            let source =
                format!("#!marksheet 0.1\n@style bad font-size={invalid}\n@sheet s \"Sheet\"\n");
            assert!(
                parse(source.as_bytes()).has_errors(),
                "accepted invalid numeric spelling {invalid}"
            );
        }

        for directive in [
            "@column A width=.5",
            "@row 1 height=1.",
            "@row 1 height=+1",
            "@column A width=01",
        ] {
            let source = format!("#!marksheet 0.1\n@sheet s \"Sheet\"\n{directive}\n");
            let document = parse(source.as_bytes());
            assert_eq!(
                document.diagnostics[0].code.as_str(),
                "MS2201",
                "accepted invalid directive {directive}"
            );
        }
    }

    #[test]
    fn boolean_literals_are_reserved_name_identifiers() {
        for reserved in ["true", "false"] {
            let source = format!(
                "#!marksheet 0.1\n@name {reserved} = sheet!A1\n@sheet sheet \"Sheet\"\n@block A1 csv\nvalue\n@end\n"
            );
            let document = parse(source.as_bytes());
            let codes: Vec<_> = document
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect();
            assert_eq!(codes, ["MS1201"], "accepted reserved name {reserved}");
            assert!(
                document
                    .workbook
                    .as_ref()
                    .is_some_and(|workbook| workbook.names.is_empty()),
                "reserved name escaped into the workbook model"
            );
        }
    }

    #[test]
    fn named_cells_remain_distinct_from_single_cell_ranges() {
        let source = b"#!marksheet 0.1\n@name scalar = summary!B2\n@name range = summary!B2:B2\n@sheet summary \"Summary\"\n@block A1 csv\nvalue\n@end\n";
        let document = parse(source);
        assert!(!document.has_errors(), "{:?}", document.diagnostics);

        let workbook = document.workbook.expect("valid workbook");
        assert_eq!(workbook.names.len(), 2);
        assert!(matches!(
            &workbook.names[0].target,
            NameTarget::Cell(cell)
                if cell.sheet == SheetId::parse("summary").unwrap()
                    && cell.coordinate == Coordinate::parse("B2").unwrap()
        ));
        assert!(matches!(
            &workbook.names[1].target,
            NameTarget::Range(range)
                if range.sheet == SheetId::parse("summary").unwrap()
                    && range.range.start == Coordinate::parse("B2").unwrap()
                    && range.range.end == Coordinate::parse("B2").unwrap()
        ));
    }

    #[test]
    fn outer_scalar_numbers_use_canonical_exponents_and_signed_zero() {
        let source =
            b"#!marksheet 0.1\n\n@sheet s \"Sheet\"\n@block A1 csv\n1e+20,0.0000001,-0.0\n@end\n";
        let once = canonicalize(&parse(source)).expect("valid numeric scalars");
        assert!(String::from_utf8_lossy(&once).contains("1e20,1e-7,-0\n"));
        assert_eq!(canonicalize(&parse(&once)).unwrap(), once);
    }
}
