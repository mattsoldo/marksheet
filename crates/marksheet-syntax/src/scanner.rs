//! Bytewise scanner for outer Marksheet syntax and embedded CSV.

use crate::cst::{
    Cst, CsvBlock, CsvField, CsvKind, CsvRecord, Directive, ExtensionBlock, Line, Node, Span,
};
use crate::diagnostic::{Diagnostic, error};

pub(crate) struct ScanResult {
    pub cst: Cst,
    pub diagnostics: Vec<Diagnostic>,
}

pub(crate) fn scan(source: &[u8]) -> ScanResult {
    let mut scanner = Scanner {
        source,
        offset: 0,
        nodes: Vec::new(),
        diagnostics: Vec::new(),
    };

    if source.starts_with(&[0xef, 0xbb, 0xbf]) {
        scanner.diagnostics.push(error(
            "MS1002",
            "UTF-8 byte-order marks are not permitted",
            Span::new(0, 3),
        ));
    }
    if let Err(utf8_error) = std::str::from_utf8(source) {
        let start = utf8_error.valid_up_to();
        let end = utf8_error
            .error_len()
            .map_or(source.len(), |len| start + len);
        scanner.diagnostics.push(error(
            "MS1003",
            "document is not valid UTF-8",
            Span::new(start, end),
        ));
    }

    scanner.run();
    ScanResult {
        cst: Cst {
            nodes: scanner.nodes,
            span: Span::new(0, source.len()),
        },
        diagnostics: scanner.diagnostics,
    }
}

struct Scanner<'a> {
    source: &'a [u8],
    offset: usize,
    nodes: Vec<Node>,
    diagnostics: Vec<Diagnostic>,
}

impl Scanner<'_> {
    fn run(&mut self) {
        let mut first = true;
        while self.offset < self.source.len() {
            let line = physical_line(self.source, self.offset);
            self.diagnose_line_ending(line);
            let content = &self.source[line.content.range()];

            if first && content.starts_with(b"#!marksheet") {
                self.nodes.push(Node::Header(line));
                self.offset = line.span.end;
            } else if content.is_empty() {
                self.nodes.push(Node::Blank(line));
                self.offset = line.span.end;
            } else if content.starts_with(b"#") {
                self.nodes.push(Node::Comment(line));
                self.offset = line.span.end;
            } else if content.starts_with(b"@") {
                let directive = split_directive(line, self.source);
                let name = &self.source[directive.name.range()];
                if name == b"block" || name == b"table" {
                    self.scan_csv(
                        directive,
                        if name == b"block" {
                            CsvKind::Block
                        } else {
                            CsvKind::Table
                        },
                    );
                } else if name == b"extension" {
                    self.scan_extension(directive);
                } else {
                    self.nodes.push(Node::Directive(directive));
                    self.offset = line.span.end;
                }
            } else {
                self.nodes.push(Node::Recovery(line));
                self.diagnostics.push(error(
                    "MS1101",
                    "expected a directive, comment, or blank line",
                    line.content,
                ));
                self.offset = line.span.end;
            }
            first = false;
        }
    }

    /// Only LF and CRLF are valid line endings, so a lone carriage return is an
    /// error rather than a third spelling that canonical output would silently
    /// rewrite. The CSV layer reports the same problem for records.
    fn diagnose_line_ending(&mut self, line: Line) {
        if &self.source[line.newline.range()] == b"\r" {
            self.diagnostics.push(error(
                "MS1004",
                "bare carriage return is not a valid line ending",
                line.newline,
            ));
        }
    }

    fn scan_csv(&mut self, directive: Directive, kind: CsvKind) {
        let block_start = directive.line.span.start;
        let body_start = directive.line.span.end;
        let (body_end, terminator, _ended_in_quotes) = find_csv_terminator(self.source, body_start);
        let (records, mut csv_diagnostics) =
            parse_csv(self.source, Span::new(body_start, body_end));
        let malformed_csv = !csv_diagnostics.is_empty();
        self.diagnostics.append(&mut csv_diagnostics);
        if terminator.is_none() && !malformed_csv {
            self.diagnostics.push(error(
                "MS1102",
                "CSV block is missing its @end terminator",
                directive.line.content,
            ));
        }
        if let Some(terminator) = terminator {
            self.diagnose_line_ending(terminator);
        }
        let end = terminator.map_or(self.source.len(), |line| line.span.end);
        self.nodes.push(Node::CsvBlock(CsvBlock {
            kind,
            directive,
            body: Span::new(body_start, body_end),
            records,
            terminator,
            span: Span::new(block_start, end),
        }));
        self.offset = end;
    }

    fn scan_extension(&mut self, directive: Directive) {
        let extension_start = directive.line.span.start;
        let payload_start = directive.line.span.end;
        let (payload_end, terminator) = find_exact_terminator(self.source, payload_start);
        if terminator.is_none() {
            self.diagnostics.push(error(
                "MS1101",
                "extension body is missing its @end terminator",
                directive.line.content,
            ));
        }
        if let Some(terminator) = terminator {
            self.diagnose_line_ending(terminator);
        }
        let end = terminator.map_or(self.source.len(), |line| line.span.end);
        self.nodes.push(Node::Extension(ExtensionBlock {
            directive,
            payload: Span::new(payload_start, payload_end),
            terminator,
            span: Span::new(extension_start, end),
        }));
        self.offset = end;
    }
}

fn physical_line(source: &[u8], start: usize) -> Line {
    let mut cursor = start;
    while cursor < source.len() && source[cursor] != b'\n' && source[cursor] != b'\r' {
        cursor += 1;
    }
    let content_end = cursor;
    if cursor < source.len() {
        if source[cursor] == b'\r' && source.get(cursor + 1) == Some(&b'\n') {
            cursor += 2;
        } else {
            cursor += 1;
        }
    }
    Line {
        span: Span::new(start, cursor),
        content: Span::new(start, content_end),
        newline: Span::new(content_end, cursor),
    }
}

fn split_directive(line: Line, source: &[u8]) -> Directive {
    let mut cursor = line.content.start + 1;
    while cursor < line.content.end && source[cursor].is_ascii_lowercase() {
        cursor += 1;
    }
    let name = Span::new(line.content.start + 1, cursor);
    while cursor < line.content.end && source[cursor] == b' ' {
        cursor += 1;
    }
    Directive {
        line,
        name,
        arguments: Span::new(cursor, line.content.end),
    }
}

fn find_exact_terminator(source: &[u8], start: usize) -> (usize, Option<Line>) {
    let mut cursor = start;
    while cursor < source.len() {
        let line = physical_line(source, cursor);
        if &source[line.content.range()] == b"@end" {
            return (line.span.start, Some(line));
        }
        cursor = line.span.end;
    }
    (source.len(), None)
}

/// Locate `@end` while deliberately tracking only quote state needed for the
/// outer boundary. Detailed CSV errors are emitted by `parse_csv`.
fn find_csv_terminator(source: &[u8], start: usize) -> (usize, Option<Line>, bool) {
    let mut cursor = start;
    let mut in_quotes = false;
    let mut at_field_start = true;
    while cursor < source.len() {
        let line = physical_line(source, cursor);
        if !in_quotes && &source[line.content.range()] == b"@end" {
            return (line.span.start, Some(line), false);
        }

        let mut index = line.content.start;
        while index < line.content.end {
            match source[index] {
                b'"' if in_quotes && source.get(index + 1) == Some(&b'"') => index += 1,
                b'"' if in_quotes => in_quotes = false,
                b'"' if at_field_start => {
                    in_quotes = true;
                    at_field_start = false;
                }
                b',' if !in_quotes => at_field_start = true,
                _ if !in_quotes => at_field_start = false,
                _ => {}
            }
            index += 1;
        }
        if !in_quotes {
            at_field_start = true;
        }
        cursor = line.span.end;
    }
    (source.len(), None, in_quotes)
}

// One state machine owns quote transitions and exact spans; splitting it into
// passes would risk disagreement at malformed recovery boundaries.
#[allow(clippy::too_many_lines)]
fn parse_csv(source: &[u8], body: Span) -> (Vec<CsvRecord>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let mut records = Vec::new();
    if body.is_empty() {
        return (records, diagnostics);
    }

    let mut cursor = body.start;
    let mut record_start = cursor;
    let mut fields = Vec::new();
    while cursor < body.end {
        let field_start = cursor;
        let quoted = source[cursor] == b'"';
        let mut decoded = Vec::new();
        let mut closed_quote = !quoted;
        if quoted {
            cursor += 1;
            while cursor < body.end {
                if source[cursor] == b'"' {
                    if cursor + 1 < body.end && source[cursor + 1] == b'"' {
                        decoded.push(b'"');
                        cursor += 2;
                    } else {
                        cursor += 1;
                        closed_quote = true;
                        break;
                    }
                } else if source[cursor] == b'\r' {
                    // An embedded record always decodes to LF, so no raw
                    // carriage return can reach a value or canonical output.
                    let carriage_return = cursor;
                    cursor += 1;
                    if cursor < body.end && source[cursor] == b'\n' {
                        cursor += 1;
                    } else {
                        diagnostics.push(error(
                            "MS1102",
                            "bare carriage return is not a valid line ending",
                            Span::new(carriage_return, cursor),
                        ));
                    }
                    decoded.push(b'\n');
                } else {
                    decoded.push(source[cursor]);
                    cursor += 1;
                }
            }
            if !closed_quote {
                diagnostics.push(error(
                    "MS1102",
                    "CSV body ends inside a quoted field",
                    Span::new(field_start, body.end),
                ));
            }
        } else {
            while cursor < body.end
                && source[cursor] != b','
                && source[cursor] != b'\n'
                && source[cursor] != b'\r'
            {
                if source[cursor] == b'"' {
                    diagnostics.push(error(
                        "MS1102",
                        "a quote in an unquoted CSV field is invalid",
                        Span::new(cursor, cursor + 1),
                    ));
                }
                decoded.push(source[cursor]);
                cursor += 1;
            }
        }

        if quoted
            && closed_quote
            && cursor < body.end
            && !matches!(source[cursor], b',' | b'\n' | b'\r')
        {
            let invalid_start = cursor;
            while cursor < body.end
                && source[cursor] != b','
                && source[cursor] != b'\n'
                && source[cursor] != b'\r'
            {
                cursor += 1;
            }
            diagnostics.push(error(
                "MS1102",
                "unexpected bytes after the closing CSV quote",
                Span::new(invalid_start, cursor),
            ));
        }

        let field_end = cursor;
        let decoded = String::from_utf8_lossy(&decoded).into_owned();
        fields.push(CsvField {
            span: Span::new(field_start, field_end),
            decoded,
            quoted,
        });

        if cursor >= body.end {
            records.push(CsvRecord {
                span: Span::new(record_start, cursor),
                fields,
                newline: Span::new(cursor, cursor),
            });
            break;
        }
        if source[cursor] == b',' {
            cursor += 1;
            // A delimiter at the end of the body denotes a final blank field.
            if cursor == body.end {
                fields.push(CsvField {
                    span: Span::new(cursor, cursor),
                    decoded: String::new(),
                    quoted: false,
                });
                records.push(CsvRecord {
                    span: Span::new(record_start, cursor),
                    fields,
                    newline: Span::new(cursor, cursor),
                });
                break;
            }
            continue;
        }

        let newline_start = cursor;
        if source[cursor] == b'\r' {
            cursor += 1;
            if cursor < body.end && source[cursor] == b'\n' {
                cursor += 1;
            } else {
                diagnostics.push(error(
                    "MS1102",
                    "bare carriage return is not a valid line ending",
                    Span::new(newline_start, cursor),
                ));
            }
        } else {
            cursor += 1;
        }
        records.push(CsvRecord {
            span: Span::new(record_start, newline_start),
            fields,
            newline: Span::new(newline_start, cursor),
        });
        fields = Vec::new();
        record_start = cursor;
        // The newline immediately before @end belongs to the last record and
        // does not manufacture another empty CSV record.
        if cursor == body.end {
            break;
        }
    }

    (records, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_terminator_inside_multiline_quote_is_data() {
        let source = b"#!marksheet 0.1\n@sheet s \"S\"\n@block A1 csv\n\"a\n@end\nb\",x\n@end\n";
        let result = scan(source);
        let Node::CsvBlock(block) = &result.cst.nodes[2] else {
            panic!("expected CSV block");
        };
        assert_eq!(block.records.len(), 1);
        assert_eq!(block.records[0].fields[0].decoded, "a\n@end\nb");
        assert!(block.terminator.is_some());
    }

    #[test]
    fn extension_payload_is_completely_opaque() {
        let source =
            b"#!marksheet 0.1\r\n@extension charts@1 \"x\"\r\n  invalid \xff bytes\r\n@end\r\n";
        let result = scan(source);
        let Node::Extension(extension) = &result.cst.nodes[1] else {
            panic!("expected extension");
        };
        assert_eq!(
            &source[extension.payload.range()],
            b"  invalid \xff bytes\r\n"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "MS1003")
        );
    }

    #[test]
    fn every_byte_belongs_to_exactly_one_top_level_node() {
        let source = b"#!marksheet 0.1\n# hi\n\n@sheet s \"S\"\n@block A1 csv\na,b\n@end\n";
        let result = scan(source);
        let spans: Vec<_> = result.cst.nodes.iter().map(Node::span).collect();
        assert_eq!(spans.first().map(|span| span.start), Some(0));
        assert_eq!(spans.last().map(|span| span.end), Some(source.len()));
        assert!(
            spans
                .windows(2)
                .all(|window| window[0].end == window[1].start)
        );
    }
}
