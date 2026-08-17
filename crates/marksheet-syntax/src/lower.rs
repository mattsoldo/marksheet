//! Semantic lowering and validation from the lossless CST.

use std::collections::{HashMap, HashSet};

use marksheet_model::{
    Apply, ApplyTarget, Block, Cell, Color, ColumnGeometry, ColumnRange, Coordinate, Extension,
    ExtensionDeclaration, ExtensionId, Fill, FillTarget, FormulaSource, HorizontalAlignment, Name,
    NameId, NameTarget, NumberFormat, Origin, Range, RowGeometry, RowRange, Sheet, SheetCoordinate,
    SheetId, SheetItem, SheetRange, Style, StyleId, StyleProperties, Table, TableId, TableRegion,
    Value, VerticalAlignment, Workbook,
};

use crate::cst::{CsvBlock, CsvKind, Directive, ExtensionBlock, Node, Span};
use crate::diagnostic::{Diagnostic, error, warning};

#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// Capabilities implemented by the host, spelled as `identifier@major`.
    pub supported_extensions: Vec<String>,
}

pub(crate) fn lower(
    source: &[u8],
    nodes: &[Node],
    options: &ParseOptions,
) -> (Option<Workbook>, Vec<Diagnostic>) {
    let Ok(text) = std::str::from_utf8(source) else {
        return (None, Vec::new());
    };
    let mut lowerer = Lowerer {
        source: text,
        options,
        workbook: Workbook::default(),
        current_sheet: None,
        diagnostics: Vec::new(),
        book_span: None,
        sheet_ids: HashMap::new(),
        style_ids: HashMap::new(),
        value_ids: HashMap::new(),
        table_headers: HashMap::new(),
        reservations: HashMap::new(),
        pending_names: Vec::new(),
    };
    lowerer.workbook.origin = Some(origin(Span::new(0, source.len())));
    lowerer.validate_header(nodes);
    for node in nodes {
        lowerer.lower_node(node);
    }
    lowerer.finish_sheet();
    lowerer.validate_apply_targets();
    lowerer.resolve_names();
    let saw_sheet_directive = nodes.iter().any(|node| match node {
        Node::Directive(directive) => lowerer.slice(directive.name) == "sheet",
        _ => false,
    });
    if lowerer.workbook.sheets.is_empty() && !saw_sheet_directive {
        lowerer.diagnostics.push(error(
            "MS1101",
            "a workbook requires at least one @sheet",
            Span::new(source.len(), source.len()),
        ));
    }
    (Some(lowerer.workbook), lowerer.diagnostics)
}

struct Lowerer<'a> {
    source: &'a str,
    options: &'a ParseOptions,
    workbook: Workbook,
    current_sheet: Option<Sheet>,
    diagnostics: Vec<Diagnostic>,
    book_span: Option<Span>,
    sheet_ids: HashMap<String, Span>,
    style_ids: HashMap<String, Span>,
    /// Shared namespace for names and tables.
    value_ids: HashMap<String, Span>,
    table_headers: HashMap<String, Vec<String>>,
    reservations: HashMap<String, Vec<(marksheet_model::Footprint, Span)>>,
    pending_names: Vec<(NameId, String, Span)>,
}

impl Lowerer<'_> {
    fn validate_header(&mut self, nodes: &[Node]) {
        let headers: Vec<_> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Header(line) => Some(*line),
                Node::Comment(line) if self.slice(line.content).starts_with("#!marksheet") => {
                    Some(*line)
                }
                _ => None,
            })
            .collect();
        if headers.len() != 1 || headers[0].span.start != 0 {
            self.diagnostics.push(error(
                "MS1001",
                "document must begin with exactly one version header",
                headers.first().map_or(Span::new(0, 0), |line| line.content),
            ));
            return;
        }
        if self.slice(headers[0].content) != "#!marksheet 0.1" {
            self.diagnostics.push(error(
                "MS1001",
                "supported version header is exactly #!marksheet 0.1",
                headers[0].content,
            ));
        }
    }

    fn lower_node(&mut self, node: &Node) {
        match node {
            Node::Directive(directive) => self.lower_directive(directive),
            Node::CsvBlock(block) => self.lower_csv_block(block),
            Node::Extension(extension) => self.lower_extension(extension),
            Node::Header(_) | Node::Comment(_) | Node::Blank(_) | Node::Recovery(_) => {}
        }
    }

    fn lower_directive(&mut self, directive: &Directive) {
        match self.slice(directive.name) {
            "book" => self.lower_book(directive),
            "style" => self.lower_style(directive),
            "name" => self.lower_name(directive),
            "use" => self.lower_extension_declaration(directive, false),
            "require" => self.lower_extension_declaration(directive, true),
            "sheet" => self.lower_sheet(directive),
            "fill" => self.lower_fill(directive),
            "apply" => self.lower_apply(directive),
            "column" => self.lower_column(directive),
            "row" => self.lower_row(directive),
            "end" => self.invalid(directive.line.content, "unexpected @end"),
            _ => self.invalid(directive.name, "unknown directive"),
        }
    }

    fn lower_book(&mut self, directive: &Directive) {
        if self.current_sheet.is_some() || !self.workbook.sheets.is_empty() {
            self.invalid(directive.line.content, "@book must precede every @sheet");
            return;
        }
        if let Some(first) = self.book_span.replace(directive.line.content) {
            self.duplicate(directive.line.content, first, "duplicate @book directive");
            return;
        }
        // Keep the directive line (including its original line ending) so an
        // editor can replace an explicit declaration without inventing one
        // for workbooks that only use default settings.
        self.workbook.book_origin = Some(origin(directive.line.span));
        let Some(properties) = self.properties(directive.arguments) else {
            return;
        };
        let mut seen = HashSet::new();
        for property in properties {
            if !seen.insert(property.key.clone()) {
                self.invalid(property.span, "duplicate @book property");
                continue;
            }
            let Some(value) = property.string_value() else {
                self.invalid(property.span, "@book properties require JSON string values");
                continue;
            };
            match property.key.as_str() {
                "locale" => self.workbook.settings.locale = value,
                "timezone" => self.workbook.settings.timezone = value,
                "formula-profile" => self.workbook.settings.formula_profile = value,
                _ => self.invalid(property.span, "unknown @book property"),
            }
        }
    }

    fn lower_sheet(&mut self, directive: &Directive) {
        let Some(tokens) = self.exact_tokens(directive.arguments, 2) else {
            return;
        };
        if tokens[0].quoted {
            self.diagnostics.push(error(
                "MS1201",
                "sheet identifier must not be a JSON string",
                tokens[0].span,
            ));
            return;
        }
        let Ok(id) = SheetId::parse(&tokens[0].text) else {
            self.diagnostics
                .push(error("MS1201", "invalid sheet identifier", tokens[0].span));
            return;
        };
        if !tokens[1].quoted {
            self.invalid(tokens[1].span, "sheet label must be a JSON string");
            return;
        }
        if let Some(first) = self.sheet_ids.insert(id.to_string(), tokens[0].span) {
            self.duplicate(tokens[0].span, first, "duplicate sheet identifier");
            return;
        }
        self.finish_sheet();
        self.current_sheet = Some(Sheet {
            id,
            label: tokens[1].text.clone(),
            items: Vec::new(),
            origin: Some(origin(directive.line.span)),
        });
    }

    fn finish_sheet(&mut self) {
        if let Some(sheet) = self.current_sheet.take() {
            self.workbook.sheets.push(sheet);
        }
    }

    // Keeping the validation steps linear yields stable source-order
    // diagnostics and prevents partial semantic items from escaping.
    #[allow(clippy::too_many_lines)]
    fn lower_csv_block(&mut self, csv: &CsvBlock) {
        let expected = if csv.kind == CsvKind::Block { 2 } else { 3 };
        let Some(tokens) = self.exact_tokens(csv.directive.arguments, expected) else {
            return;
        };
        // The grammar spells the encoding as a bare `csv` literal, so a JSON
        // string that decodes to `csv` must not be accepted here.
        let encoding = &tokens[expected - 1];
        if encoding.quoted {
            let span = encoding.span;
            self.invalid(span, "block encoding must not be a JSON string");
            return;
        }
        if encoding.text != "csv" {
            self.invalid(
                csv.directive.arguments,
                "only the csv block encoding is supported",
            );
            return;
        }
        let anchor_index = usize::from(csv.kind == CsvKind::Table);
        if tokens[anchor_index].quoted {
            self.diagnostics.push(error(
                "MS1202",
                "block anchor must not be a JSON string",
                tokens[anchor_index].span,
            ));
            return;
        }
        let Ok(anchor) = Coordinate::parse(&tokens[anchor_index].text) else {
            self.diagnostics.push(error(
                "MS1202",
                "invalid block anchor",
                tokens[anchor_index].span,
            ));
            return;
        };
        if csv.records.is_empty() {
            self.invalid(csv.body, "CSV block requires at least one record");
            return;
        }
        let width = csv.records[0].fields.len();
        if width == 0
            || csv
                .records
                .iter()
                .any(|record| record.fields.len() != width)
        {
            self.diagnostics
                .push(error("MS1204", "CSV records must be rectangular", csv.body));
            return;
        }
        let mut cells = Vec::with_capacity(csv.records.len());
        for record in &csv.records {
            let mut row = Vec::with_capacity(record.fields.len());
            for field in &record.fields {
                let value = match Value::parse_strict(&field.decoded) {
                    Ok(value) => value,
                    Err(parse_error) => {
                        self.diagnostics
                            .push(error("MS2201", parse_error.to_string(), field.span));
                        Value::from_csv_field(&field.decoded)
                    }
                };
                row.push(Cell {
                    value,
                    origin: Some(origin(field.span)),
                });
            }
            cells.push(row);
        }
        let Ok(mut block) = Block::new(anchor, cells) else {
            self.diagnostics
                .push(error("MS1204", "invalid rectangular block", csv.body));
            return;
        };
        block.origin = Some(origin(csv.span));
        let Ok(footprint) = block.footprint() else {
            self.diagnostics.push(error(
                "MS1202",
                "block footprint exceeds coordinate limits",
                csv.directive.line.content,
            ));
            return;
        };
        if !self.reserve(footprint, csv.directive.line.content) {
            return;
        }

        if csv.kind == CsvKind::Table {
            if tokens[0].quoted {
                self.diagnostics.push(error(
                    "MS1201",
                    "table identifier must not be a JSON string",
                    tokens[0].span,
                ));
                return;
            }
            let Ok(table_id) = TableId::parse(&tokens[0].text) else {
                self.diagnostics
                    .push(error("MS1201", "invalid table identifier", tokens[0].span));
                return;
            };
            if let Some(first) = self.value_ids.insert(table_id.to_string(), tokens[0].span) {
                self.duplicate(
                    tokens[0].span,
                    first,
                    "table conflicts with an existing table or name",
                );
                return;
            }
            let mut headers = Vec::new();
            let mut header_spans = HashMap::new();
            for (field, cell) in csv.records[0].fields.iter().zip(&block.cells[0]) {
                let Value::Text(header) = &cell.value else {
                    self.diagnostics.push(error(
                        "MS2201",
                        "table headers must be text values",
                        field.span,
                    ));
                    continue;
                };
                if header.is_empty() {
                    self.diagnostics.push(error(
                        "MS2201",
                        "table headers must be nonblank text",
                        field.span,
                    ));
                }
                if let Some(first) = header_spans.insert(header.clone(), field.span) {
                    self.duplicate_with_code(
                        "MS2201",
                        field.span,
                        first,
                        "table headers must be unique",
                    );
                }
                headers.push(header.clone());
            }
            self.table_headers.insert(table_id.to_string(), headers);
            let table = Table {
                id: table_id,
                block,
                origin: Some(origin(csv.span)),
            };
            self.push_sheet_item(SheetItem::Table(table), csv.directive.line.content);
        } else {
            self.push_sheet_item(SheetItem::Block(block), csv.directive.line.content);
        }
    }

    fn reserve(&mut self, footprint: marksheet_model::Footprint, span: Span) -> bool {
        let Some(sheet) = self.current_sheet.as_ref() else {
            self.invalid(span, "blocks and tables are sheet-scoped");
            return false;
        };
        let reservations = self.reservations.entry(sheet.id.to_string()).or_default();
        for (existing, existing_span) in reservations.iter() {
            if footprint.overlaps(*existing).unwrap_or(true) {
                let first = *existing_span;
                self.duplicate_with_code("MS1302", span, first, "block footprints overlap");
                return false;
            }
        }
        reservations.push((footprint, span));
        true
    }

    fn push_sheet_item(&mut self, item: SheetItem, span: Span) {
        if let Some(sheet) = self.current_sheet.as_mut() {
            sheet.items.push(item);
        } else {
            self.invalid(span, "directive is only valid inside a sheet");
        }
    }

    fn lower_style(&mut self, directive: &Directive) {
        if self.current_sheet.is_some() || !self.workbook.sheets.is_empty() {
            self.invalid(directive.line.content, "@style must precede every @sheet");
            return;
        }
        let Some((id_token, property_span)) = self.first_token_and_rest(directive.arguments) else {
            return;
        };
        if id_token.quoted {
            self.diagnostics.push(error(
                "MS1201",
                "style identifier must not be a JSON string",
                id_token.span,
            ));
            return;
        }
        let Ok(id) = StyleId::parse(&id_token.text) else {
            self.diagnostics
                .push(error("MS1201", "invalid style identifier", id_token.span));
            return;
        };
        if let Some(first) = self.style_ids.insert(id.to_string(), id_token.span) {
            self.duplicate(id_token.span, first, "duplicate style identifier");
            return;
        }
        let Some(properties) = self.properties(property_span) else {
            return;
        };
        let mut style = StyleProperties::default();
        let mut seen = HashSet::new();
        for property in properties {
            if !seen.insert(property.key.clone()) {
                self.invalid(property.span, "duplicate style property");
                continue;
            }
            let valid = match property.key.as_str() {
                "bold" => assign_bool(&property, &mut style.bold),
                "italic" => assign_bool(&property, &mut style.italic),
                "wrap" => assign_bool(&property, &mut style.wrap),
                "text-color" => assign_color(&property, &mut style.text_color),
                "fill" => assign_color(&property, &mut style.fill),
                "font-size" => assign_positive_number(&property, &mut style.font_size),
                "align" => assign_align(&property, &mut style.align),
                "valign" => assign_valign(&property, &mut style.valign),
                "number" => assign_number_format(&property, &mut style.number),
                "decimals" => assign_decimals(&property, &mut style.decimals),
                "currency" => assign_currency(&property, &mut style.currency),
                _ => false,
            };
            if !valid {
                self.diagnostics
                    .push(error("MS2201", "invalid style property", property.span));
            }
        }
        if style.number == Some(NumberFormat::Currency) && style.currency.is_none() {
            self.diagnostics.push(error(
                "MS2201",
                "currency number styles require currency=\"XXX\"",
                directive.line.content,
            ));
        }
        self.workbook.styles.push(Style {
            id,
            properties: style,
            origin: Some(origin(directive.line.span)),
        });
    }

    fn lower_name(&mut self, directive: &Directive) {
        if self.current_sheet.is_some() || !self.workbook.sheets.is_empty() {
            self.invalid(directive.line.content, "@name must precede every @sheet");
            return;
        }
        let raw = self.slice(directive.arguments).to_owned();
        let Some((id_raw, target_raw)) = raw.split_once(" = ") else {
            self.invalid(directive.arguments, "expected @name identifier = target");
            return;
        };
        let id_start = directive.arguments.start;
        let id_span = Span::new(id_start, id_start + id_raw.len());
        let Ok(id) = NameId::parse(id_raw) else {
            self.diagnostics
                .push(error("MS1201", "invalid name identifier", id_span));
            return;
        };
        if Coordinate::parse(id_raw).is_ok() || looks_like_r1c1(id_raw) {
            self.diagnostics.push(error(
                "MS1201",
                "a name cannot resemble a cell address",
                id_span,
            ));
            return;
        }
        if matches!(id_raw, "true" | "false") {
            self.diagnostics.push(error(
                "MS1201",
                "a name cannot use a boolean literal as its identifier",
                id_span,
            ));
            return;
        }
        if let Some(first) = self.value_ids.insert(id.to_string(), id_span) {
            self.duplicate(
                id_span,
                first,
                "name conflicts with an existing table or name",
            );
            return;
        }
        let target = target_raw.trim().to_owned();
        if target.is_empty() {
            self.invalid(directive.arguments, "name target cannot be empty");
            return;
        }
        self.pending_names.push((id, target, directive.line.span));
    }

    fn resolve_names(&mut self) {
        for (id, target, span) in std::mem::take(&mut self.pending_names) {
            let resolved = if let Some((sheet_raw, target_raw)) = target.split_once('!') {
                match SheetId::parse(sheet_raw) {
                    Ok(sheet) if self.sheet_ids.contains_key(sheet.as_str()) => {
                        // Preserve the authored reference shape. `A1` is a scalar
                        // named cell, while `A1:A1` remains a range despite
                        // covering the same coordinate.
                        if let Ok(coordinate) = Coordinate::parse(target_raw) {
                            Some(NameTarget::Cell(SheetCoordinate { sheet, coordinate }))
                        } else if let Ok(range) = Range::parse(target_raw) {
                            Some(NameTarget::Range(SheetRange { sheet, range }))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            } else if let Some((table_raw, header)) = parse_bracket_target(&target) {
                match TableId::parse(table_raw) {
                    Ok(table)
                        if self
                            .table_headers
                            .get(table.as_str())
                            .is_some_and(|headers| {
                                headers.iter().any(|candidate| candidate == &header)
                            }) =>
                    {
                        Some(NameTarget::TableColumn { table, header })
                    }
                    _ => None,
                }
            } else {
                None
            };
            if let Some(target) = resolved {
                self.workbook.names.push(Name {
                    id,
                    target,
                    origin: Some(origin(span)),
                });
            } else {
                self.diagnostics
                    .push(error("MS2101", "unresolved named-range target", span));
            }
        }
    }

    fn lower_extension_declaration(&mut self, directive: &Directive, required: bool) {
        if self.current_sheet.is_some() || !self.workbook.sheets.is_empty() {
            self.invalid(
                directive.line.content,
                "extension declarations must precede sheets",
            );
            return;
        }
        let Some(tokens) = self.exact_tokens(directive.arguments, 1) else {
            return;
        };
        if tokens[0].quoted {
            self.diagnostics.push(error(
                "MS1201",
                "extension capability must not be a JSON string",
                tokens[0].span,
            ));
            return;
        }
        let Ok(capability) = ExtensionId::parse(&tokens[0].text) else {
            self.diagnostics.push(error(
                "MS1201",
                "invalid extension capability",
                tokens[0].span,
            ));
            return;
        };
        if let Some(existing) = self
            .workbook
            .extensions
            .iter()
            .find(|existing| existing.capability.id == capability.id)
        {
            let first = span_from_origin(existing.origin, tokens[0].span);
            self.duplicate(
                tokens[0].span,
                first,
                "duplicate or conflicting extension declaration",
            );
            return;
        }
        let supported = self
            .options
            .supported_extensions
            .iter()
            .any(|candidate| candidate == &tokens[0].text);
        if !supported {
            let diagnostic = if required {
                error(
                    "MS3101",
                    "required extension is not available",
                    tokens[0].span,
                )
            } else {
                warning(
                    "MS3102",
                    "optional extension is not available",
                    tokens[0].span,
                )
            };
            self.diagnostics.push(diagnostic);
        }
        self.workbook.extensions.push(ExtensionDeclaration {
            capability,
            required,
            origin: Some(origin(directive.line.span)),
        });
    }

    fn lower_extension(&mut self, extension: &ExtensionBlock) {
        let Some(tokens) = self.exact_tokens(extension.directive.arguments, 2) else {
            return;
        };
        if tokens[0].quoted {
            self.diagnostics.push(error(
                "MS1201",
                "extension capability must not be a JSON string",
                tokens[0].span,
            ));
            return;
        }
        let Ok(capability) = ExtensionId::parse(&tokens[0].text) else {
            self.diagnostics.push(error(
                "MS1201",
                "invalid extension capability",
                tokens[0].span,
            ));
            return;
        };
        if !tokens[1].quoted {
            self.invalid(
                tokens[1].span,
                "extension instance name must be a JSON string",
            );
            return;
        }
        let payload = self.slice(extension.payload).to_owned();
        let instance = Extension {
            capability,
            name: tokens[1].text.clone(),
            payload,
            origin: Some(origin(extension.span)),
            payload_origin: Some(origin(extension.payload)),
        };
        if let Some(sheet) = self.current_sheet.as_ref() {
            if let Some(existing) = sheet.items.iter().find_map(|item| match item {
                SheetItem::Extension(existing)
                    if existing.capability == instance.capability
                        && existing.name == instance.name =>
                {
                    Some(existing)
                }
                _ => None,
            }) {
                let first = span_from_origin(existing.origin, extension.directive.line.content);
                self.duplicate(
                    extension.directive.line.content,
                    first,
                    "duplicate extension instance in sheet scope",
                );
                return;
            }
            self.push_sheet_item(
                SheetItem::Extension(instance),
                extension.directive.line.content,
            );
        } else {
            if let Some(existing) = self.workbook.extension_instances.iter().find(|existing| {
                existing.capability == instance.capability && existing.name == instance.name
            }) {
                let first = span_from_origin(existing.origin, extension.directive.line.content);
                self.duplicate(
                    extension.directive.line.content,
                    first,
                    "duplicate extension instance in workbook scope",
                );
                return;
            }
            self.workbook.extension_instances.push(instance);
        }
    }

    fn lower_fill(&mut self, directive: &Directive) {
        let raw = self.slice(directive.arguments);
        let Some((target_raw, formula_raw)) = split_target_and_rest(raw) else {
            self.invalid(directive.arguments, "expected @fill target =formula");
            return;
        };
        let Ok(formula) = FormulaSource::new(formula_raw) else {
            self.invalid(directive.arguments, "fill formula must begin with =");
            return;
        };
        let Some(target) = parse_fill_target(target_raw) else {
            self.diagnostics.push(error(
                "MS2102",
                "invalid or unresolved fill target",
                directive.arguments,
            ));
            return;
        };
        if !self.fill_target_is_blank(&target) {
            let resolved = self.fill_target_resolves(&target);
            self.diagnostics.push(error(
                if resolved { "MS2201" } else { "MS2102" },
                if resolved {
                    "fill target cells must be blank in source"
                } else {
                    "fill target must resolve to a preceding block or table"
                },
                directive.arguments,
            ));
        }
        self.push_sheet_item(
            SheetItem::Fill(Fill {
                target,
                formula,
                origin: Some(origin(directive.line.span)),
            }),
            directive.line.content,
        );
    }

    fn fill_target_is_blank(&self, target: &FillTarget) -> bool {
        let Some(sheet) = self.current_sheet.as_ref() else {
            return false;
        };
        match target {
            FillTarget::Range(range) => range_cells(sheet, *range).is_some_and(|cells| {
                !cells.is_empty() && cells.iter().all(|cell| matches!(cell.value, Value::Blank))
            }),
            FillTarget::TableColumn { table, header } => sheet.items.iter().any(|item| {
                let SheetItem::Table(candidate) = item else {
                    return false;
                };
                if &candidate.id != table {
                    return false;
                }
                if candidate.block.cells.len() <= 1 {
                    return false;
                }
                let Some(index) = candidate.block.cells.first().and_then(|row| {
                    row.iter()
                        .position(|cell| matches!(&cell.value, Value::Text(text) if text == header))
                }) else {
                    return false;
                };
                candidate.block.cells[1..]
                    .iter()
                    .all(|row| matches!(row[index].value, Value::Blank))
            }),
        }
    }

    fn fill_target_resolves(&self, target: &FillTarget) -> bool {
        let Some(sheet) = self.current_sheet.as_ref() else {
            return false;
        };
        match target {
            FillTarget::Range(range) => range_cells(sheet, *range).is_some(),
            FillTarget::TableColumn { table, header } => sheet.items.iter().any(|item| {
                matches!(item, SheetItem::Table(candidate) if &candidate.id == table
                    && candidate.block.cells.len() > 1
                    && candidate.block.cells.first().is_some_and(|row| row.iter().any(|cell| matches!(&cell.value, Value::Text(text) if text == header))))
            }),
        }
    }

    fn lower_apply(&mut self, directive: &Directive) {
        let raw = self.slice(directive.arguments);
        let Some((target_raw, styles_raw)) = split_target_and_rest(raw) else {
            self.invalid(
                directive.arguments,
                "@apply requires a target and at least one style",
            );
            return;
        };
        let target_end = directive.arguments.start + target_raw.len();
        let style_start = directive.arguments.end - styles_raw.len();
        let Ok(tokens) = lex_tokens(self.source, Span::new(style_start, directive.arguments.end))
        else {
            self.invalid(directive.arguments, "malformed @apply arguments");
            return;
        };
        let Some(target) = parse_apply_target(target_raw) else {
            self.diagnostics.push(error(
                "MS2102",
                "invalid apply target",
                Span::new(directive.arguments.start, target_end),
            ));
            return;
        };
        let mut styles = Vec::new();
        for token in &tokens {
            if token.quoted {
                self.diagnostics.push(error(
                    "MS1201",
                    "style identifier must not be a JSON string",
                    token.span,
                ));
                continue;
            }
            let Ok(style) = StyleId::parse(&token.text) else {
                self.diagnostics
                    .push(error("MS1201", "invalid style identifier", token.span));
                continue;
            };
            if !self.style_ids.contains_key(style.as_str()) {
                self.diagnostics
                    .push(error("MS2102", "unresolved style identifier", token.span));
            }
            styles.push(style);
        }
        self.push_sheet_item(
            SheetItem::Apply(Apply {
                target,
                styles,
                origin: Some(origin(directive.line.span)),
            }),
            directive.line.content,
        );
    }

    fn validate_apply_targets(&mut self) {
        let mut tables = HashMap::new();
        for sheet in &self.workbook.sheets {
            for item in &sheet.items {
                let SheetItem::Table(table) = item else {
                    continue;
                };
                let headers = table
                    .block
                    .cells
                    .first()
                    .into_iter()
                    .flatten()
                    .filter_map(|cell| match &cell.value {
                        Value::Text(header) => Some(header.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                tables.insert(table.id.to_string(), (sheet.id.clone(), headers));
            }
        }

        for sheet in &self.workbook.sheets {
            for item in &sheet.items {
                let SheetItem::Apply(apply) = item else {
                    continue;
                };
                let ApplyTarget::Table { table, region } = &apply.target else {
                    continue;
                };
                let resolved = tables.get(table.as_str()).is_some_and(|(owner, headers)| {
                    owner == &sheet.id
                        && match region {
                            TableRegion::Headers | TableRegion::Data => true,
                            TableRegion::Column { header } => headers.contains(header),
                        }
                });
                if !resolved {
                    self.diagnostics.push(error(
                        "MS2102",
                        "table application target must resolve in the current sheet",
                        span_from_origin(apply.origin, Span::new(0, 0)),
                    ));
                }
            }
        }
    }

    fn lower_column(&mut self, directive: &Directive) {
        let Some(tokens) = self.exact_tokens(directive.arguments, 2) else {
            return;
        };
        if tokens[0].quoted {
            self.diagnostics.push(error(
                "MS1202",
                "column range must not be a JSON string",
                tokens[0].span,
            ));
            return;
        }
        let Some(columns) = parse_column_range(&tokens[0].text) else {
            self.diagnostics
                .push(error("MS1202", "invalid column range", tokens[0].span));
            return;
        };
        if tokens[1].quoted {
            self.diagnostics.push(error(
                "MS2201",
                "column width must not be a JSON string",
                tokens[1].span,
            ));
            return;
        }
        let Some(width) = tokens[1]
            .text
            .strip_prefix("width=")
            .and_then(parse_json_number)
            .filter(|number| *number > 0.0)
        else {
            self.diagnostics.push(error(
                "MS2201",
                "column width must be a positive finite number",
                tokens[1].span,
            ));
            return;
        };
        self.push_sheet_item(
            SheetItem::ColumnGeometry(ColumnGeometry {
                columns,
                width,
                origin: Some(origin(directive.line.span)),
            }),
            directive.line.content,
        );
    }

    fn lower_row(&mut self, directive: &Directive) {
        let Some(tokens) = self.exact_tokens(directive.arguments, 2) else {
            return;
        };
        if tokens[0].quoted {
            self.diagnostics.push(error(
                "MS1202",
                "row range must not be a JSON string",
                tokens[0].span,
            ));
            return;
        }
        let Some(rows) = parse_row_range(&tokens[0].text) else {
            self.diagnostics
                .push(error("MS1202", "invalid row range", tokens[0].span));
            return;
        };
        if tokens[1].quoted {
            self.diagnostics.push(error(
                "MS2201",
                "row height must not be a JSON string",
                tokens[1].span,
            ));
            return;
        }
        let Some(height) = tokens[1]
            .text
            .strip_prefix("height=")
            .and_then(parse_json_number)
            .filter(|number| *number > 0.0)
        else {
            self.diagnostics.push(error(
                "MS2201",
                "row height must be a positive finite number",
                tokens[1].span,
            ));
            return;
        };
        self.push_sheet_item(
            SheetItem::RowGeometry(RowGeometry {
                rows,
                height,
                origin: Some(origin(directive.line.span)),
            }),
            directive.line.content,
        );
    }

    fn exact_tokens(&mut self, span: Span, count: usize) -> Option<Vec<Token>> {
        match lex_tokens(self.source, span) {
            Ok(tokens) if tokens.len() == count => Some(tokens),
            Ok(_) => {
                self.invalid(span, format!("expected exactly {count} argument(s)"));
                None
            }
            Err(message) => {
                self.invalid(span, message);
                None
            }
        }
    }

    fn first_token_and_rest(&mut self, span: Span) -> Option<(Token, Span)> {
        let Ok(tokens) = lex_tokens(self.source, span) else {
            self.invalid(span, "malformed directive arguments");
            return None;
        };
        let Some(first) = tokens.first().cloned() else {
            self.invalid(span, "missing directive argument");
            return None;
        };
        let mut rest_start = first.span.end;
        while rest_start < span.end && self.source.as_bytes()[rest_start] == b' ' {
            rest_start += 1;
        }
        Some((first, Span::new(rest_start, span.end)))
    }

    fn properties(&mut self, span: Span) -> Option<Vec<Property>> {
        let tokens = match lex_tokens(self.source, span) {
            Ok(tokens) => tokens,
            Err(message) => {
                self.invalid(span, message);
                return None;
            }
        };
        let mut properties = Vec::new();
        for token in tokens {
            let Some((key, raw_value)) = token.raw.split_once('=') else {
                self.invalid(token.span, "expected key=value property");
                return None;
            };
            if key.is_empty() || raw_value.is_empty() {
                self.invalid(token.span, "property key and value cannot be empty");
                return None;
            }
            let value = if raw_value.starts_with('"') {
                serde_json::from_str(raw_value)
                    .ok()
                    .map(PropertyValue::String)
            } else if raw_value == "true" {
                Some(PropertyValue::Boolean(true))
            } else if raw_value == "false" {
                Some(PropertyValue::Boolean(false))
            } else if let Some(number) = parse_json_number(raw_value) {
                Some(PropertyValue::Number(number))
            } else if is_identifier(raw_value) {
                Some(PropertyValue::Bare(raw_value.to_owned()))
            } else {
                None
            };
            let Some(value) = value else {
                self.invalid(token.span, "invalid property value");
                return None;
            };
            properties.push(Property {
                key: key.to_owned(),
                value,
                span: token.span,
            });
        }
        Some(properties)
    }

    fn invalid(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(error("MS1101", message, span));
    }

    fn duplicate(&mut self, span: Span, first: Span, message: impl Into<String>) {
        self.duplicate_with_code("MS1301", span, first, message);
    }

    fn duplicate_with_code(
        &mut self,
        code: &'static str,
        span: Span,
        first: Span,
        message: impl Into<String>,
    ) {
        let mut diagnostic = error(code, message, span);
        diagnostic.related.push(marksheet_model::RelatedDiagnostic {
            message: "first declared here".to_owned(),
            span: marksheet_model::LabeledSpan {
                span: origin(first).span,
                label: None,
            },
        });
        self.diagnostics.push(diagnostic);
    }

    fn slice(&self, span: Span) -> &str {
        &self.source[span.range()]
    }
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    raw: String,
    span: Span,
    quoted: bool,
}

fn lex_tokens(source: &str, span: Span) -> Result<Vec<Token>, &'static str> {
    let bytes = source.as_bytes();
    let mut cursor = span.start;
    let mut tokens = Vec::new();
    while cursor < span.end {
        while cursor < span.end && bytes[cursor] == b' ' {
            cursor += 1;
        }
        if cursor == span.end {
            break;
        }
        let start = cursor;
        let mut in_string = false;
        let mut escaped = false;
        let mut brackets = 0_u32;
        while cursor < span.end {
            match bytes[cursor] {
                b'"' if !escaped => in_string = !in_string,
                b'\\' if in_string => escaped = !escaped,
                b'[' if !in_string => brackets += 1,
                b']' if !in_string && brackets > 0 => brackets -= 1,
                b' ' if !in_string && brackets == 0 => break,
                _ => escaped = false,
            }
            cursor += 1;
        }
        if in_string {
            return Err("unterminated JSON string");
        }
        let raw = &source[start..cursor];
        let quoted = raw.starts_with('"');
        let text = if quoted {
            serde_json::from_str(raw).map_err(|_| "invalid JSON string")?
        } else {
            raw.to_owned()
        };
        tokens.push(Token {
            text,
            raw: raw.to_owned(),
            span: Span::new(start, cursor),
            quoted,
        });
    }
    Ok(tokens)
}

#[derive(Clone, Debug)]
struct Property {
    key: String,
    value: PropertyValue,
    span: Span,
}
#[derive(Clone, Debug)]
enum PropertyValue {
    String(String),
    Boolean(bool),
    Number(f64),
    Bare(String),
}
impl Property {
    fn string_value(&self) -> Option<String> {
        match &self.value {
            PropertyValue::String(value) => Some(value.clone()),
            _ => None,
        }
    }
}

fn is_identifier(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value.as_bytes()[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn parse_json_number(value: &str) -> Option<f64> {
    serde_json::from_str::<serde_json::Number>(value)
        .ok()?
        .as_f64()
        .filter(|number| number.is_finite())
}

fn looks_like_r1c1(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    let Some(rest) = upper.strip_prefix('R') else {
        return false;
    };
    let Some((row, column)) = rest.split_once('C') else {
        return false;
    };
    !row.is_empty()
        && !column.is_empty()
        && row.bytes().all(|byte| byte.is_ascii_digit())
        && column.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_bracket_target(value: &str) -> Option<(&str, String)> {
    let (id, tail) = value.split_once('[')?;
    let encoded_header = tail.strip_suffix(']')?;
    if id.is_empty() || encoded_header.is_empty() {
        return None;
    }

    let mut characters = encoded_header.chars().peekable();
    let mut header = String::with_capacity(encoded_header.len());
    while let Some(character) = characters.next() {
        if character != ']' {
            header.push(character);
            continue;
        }
        characters.next_if_eq(&']')?;
        header.push(']');
    }
    Some((id, header))
}

/// Finds the separator after a target while allowing spaces inside a
/// structured-reference header and `]]` escapes.
fn split_target_and_rest(value: &str) -> Option<(&str, &str)> {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut in_brackets = false;
    while index < bytes.len() {
        match bytes[index] {
            b'[' => in_brackets = true,
            b']' if in_brackets && bytes.get(index + 1) == Some(&b']') => index += 1,
            b']' => in_brackets = false,
            b' ' if !in_brackets => {
                let rest = value[index..].trim_start();
                return (!rest.is_empty()).then_some((&value[..index], rest));
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn parse_fill_target(value: &str) -> Option<FillTarget> {
    if let Ok(range) = Range::parse(value) {
        return Some(FillTarget::Range(range));
    }
    let (table, header) = parse_bracket_target(value)?;
    Some(FillTarget::TableColumn {
        table: TableId::parse(table).ok()?,
        header,
    })
}

fn parse_apply_target(value: &str) -> Option<ApplyTarget> {
    if let Ok(range) = Range::parse(value) {
        return Some(ApplyTarget::Range(range));
    }
    let (table, region) = parse_bracket_target(value)?;
    let table = TableId::parse(table).ok()?;
    let region = match region.as_str() {
        "#Headers" => TableRegion::Headers,
        "#Data" => TableRegion::Data,
        header => TableRegion::Column {
            header: header.to_owned(),
        },
    };
    Some(ApplyTarget::Table { table, region })
}

fn range_cells(sheet: &Sheet, target: Range) -> Option<Vec<&Cell>> {
    for item in &sheet.items {
        let block = match item {
            SheetItem::Block(block) => block,
            SheetItem::Table(table) => &table.block,
            _ => continue,
        };
        let footprint = block.footprint().ok()?.range().ok()?;
        if !footprint.contains(target.start) || !footprint.contains(target.end) {
            continue;
        }
        let mut result = Vec::new();
        for row in target.start.row..=target.end.row {
            for column in target.start.column..=target.end.column {
                let row_index = usize::try_from(row - block.anchor.row).ok()?;
                let column_index = usize::try_from(column - block.anchor.column).ok()?;
                result.push(&block.cells[row_index][column_index]);
            }
        }
        return Some(result);
    }
    None
}

fn parse_column_number(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return None;
    }
    Coordinate::parse(&format!("{value}1"))
        .ok()
        .map(|coordinate| coordinate.column)
}
fn parse_column_range(value: &str) -> Option<ColumnRange> {
    let (first, second) = value.split_once(':').unwrap_or((value, value));
    ColumnRange::new(parse_column_number(first)?, parse_column_number(second)?).ok()
}
fn parse_row_range(value: &str) -> Option<RowRange> {
    let (first, second) = value.split_once(':').unwrap_or((value, value));
    RowRange::new(parse_row_number(first)?, parse_row_number(second)?).ok()
}
fn parse_row_number(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn assign_bool(property: &Property, target: &mut Option<bool>) -> bool {
    if let PropertyValue::Boolean(value) = property.value {
        *target = Some(value);
        true
    } else {
        false
    }
}
fn assign_color(property: &Property, target: &mut Option<Color>) -> bool {
    let PropertyValue::String(value) = &property.value else {
        return false;
    };
    match Color::parse(value) {
        Ok(value) => {
            *target = Some(value);
            true
        }
        Err(_) => false,
    }
}
fn assign_positive_number(property: &Property, target: &mut Option<f64>) -> bool {
    let PropertyValue::Number(value) = property.value else {
        return false;
    };
    if value.is_finite() && value > 0.0 {
        *target = Some(value);
        true
    } else {
        false
    }
}
fn assign_align(property: &Property, target: &mut Option<HorizontalAlignment>) -> bool {
    let PropertyValue::Bare(value) = &property.value else {
        return false;
    };
    let parsed = match value.as_str() {
        "left" => HorizontalAlignment::Left,
        "center" => HorizontalAlignment::Center,
        "right" => HorizontalAlignment::Right,
        "general" => HorizontalAlignment::General,
        _ => return false,
    };
    *target = Some(parsed);
    true
}
fn assign_valign(property: &Property, target: &mut Option<VerticalAlignment>) -> bool {
    let PropertyValue::Bare(value) = &property.value else {
        return false;
    };
    let parsed = match value.as_str() {
        "top" => VerticalAlignment::Top,
        "middle" => VerticalAlignment::Middle,
        "bottom" => VerticalAlignment::Bottom,
        _ => return false,
    };
    *target = Some(parsed);
    true
}
fn assign_number_format(property: &Property, target: &mut Option<NumberFormat>) -> bool {
    let PropertyValue::Bare(value) = &property.value else {
        return false;
    };
    let parsed = match value.as_str() {
        "general" => NumberFormat::General,
        "integer" => NumberFormat::Integer,
        "decimal" => NumberFormat::Decimal,
        "percent" => NumberFormat::Percent,
        "currency" => NumberFormat::Currency,
        "date" => NumberFormat::Date,
        "datetime" => NumberFormat::DateTime,
        _ => return false,
    };
    *target = Some(parsed);
    true
}
fn assign_decimals(property: &Property, target: &mut Option<u8>) -> bool {
    let PropertyValue::Number(value) = property.value else {
        return false;
    };
    value.fract() == 0.0
        && (0.0..=15.0).contains(&value)
        && format!("{value:.0}").parse::<u8>().is_ok_and(|parsed| {
            *target = Some(parsed);
            true
        })
}
fn assign_currency(property: &Property, target: &mut Option<String>) -> bool {
    let PropertyValue::String(value) = &property.value else {
        return false;
    };
    if value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        *target = Some(value.clone());
        true
    } else {
        false
    }
}

fn origin(span: Span) -> Origin {
    Origin {
        span: marksheet_model::ByteSpan {
            start: span.start as u64,
            end: span.end as u64,
        },
    }
}

fn span_from_origin(value: Option<Origin>, fallback: Span) -> Span {
    value
        .and_then(|origin| {
            Some(Span::new(
                usize::try_from(origin.span.start).ok()?,
                usize::try_from(origin.span.end).ok()?,
            ))
        })
        .unwrap_or(fallback)
}
