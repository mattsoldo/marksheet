#![allow(clippy::too_many_lines)] // ZIP inventory validation is deliberately one atomic pass.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    path::Component,
};

use quick_xml::{Reader, events::Event};
use zip::{CompressionMethod, ZipArchive};

use crate::{ConversionLimits, ConvertError, ConvertErrorCode};

use super::xml::{attribute, invalid, local_name, required_attribute, resource, validate_xml};

#[derive(Clone, Debug)]
pub(super) struct Package {
    parts: BTreeMap<String, Vec<u8>>,
    hardened: BTreeSet<String>,
    macro_enabled: bool,
}

impl Package {
    pub(super) fn open(bytes: &[u8], limits: ConversionLimits) -> Result<Self, ConvertError> {
        limits.check_input(bytes.len())?;
        let cursor = Cursor::new(bytes);
        let mut archive = ZipArchive::new(cursor).map_err(|error| {
            ConvertError::new(
                ConvertErrorCode::InvalidPackage,
                format!("input is not a readable ZIP package: {error}"),
            )
        })?;
        if archive.len() > limits.max_zip_entries {
            return Err(resource(
                "[zip]",
                "ZIP entry count exceeds the configured limit",
            ));
        }

        let mut parts = BTreeMap::new();
        let mut folded_names = BTreeSet::new();
        let mut total = 0_u64;
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).map_err(|error| {
                ConvertError::new(
                    ConvertErrorCode::InvalidPackage,
                    format!("cannot read ZIP directory entry {index}: {error}"),
                )
            })?;
            if file.encrypted() {
                return Err(ConvertError::new(
                    ConvertErrorCode::UnsupportedPackage,
                    "encrypted XLSX entries are not accepted",
                ));
            }
            if file.is_dir() || file.is_symlink() {
                return Err(invalid(
                    file.name(),
                    "directory and symlink ZIP entries are not accepted",
                ));
            }
            if !matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            ) {
                return Err(ConvertError::new(
                    ConvertErrorCode::UnsupportedPackage,
                    format!("unsupported ZIP compression for {}", file.name()),
                ));
            }
            let raw_name = std::str::from_utf8(file.name_raw())
                .map_err(|_| {
                    ConvertError::new(
                        ConvertErrorCode::InvalidPackage,
                        "ZIP entry name is not valid UTF-8",
                    )
                })?
                .to_owned();
            validate_part_name(&raw_name)?;
            if file.enclosed_name().is_none() {
                return Err(invalid(&raw_name, "ZIP entry name is not safely enclosed"));
            }
            if !folded_names.insert(raw_name.to_ascii_lowercase()) {
                return Err(invalid(&raw_name, "duplicate or case-alias ZIP entry name"));
            }
            if file.size() > limits.max_zip_entry_uncompressed_bytes {
                return Err(resource(
                    &raw_name,
                    "ZIP entry exceeds the uncompressed entry limit",
                ));
            }
            total = total
                .checked_add(file.size())
                .ok_or_else(|| resource(&raw_name, "ZIP uncompressed size overflow"))?;
            if total > limits.max_zip_total_uncompressed_bytes {
                return Err(resource(
                    &raw_name,
                    "ZIP total uncompressed size exceeds the limit",
                ));
            }
            if file.compressed_size() > 0
                && file.size()
                    > file
                        .compressed_size()
                        .saturating_mul(limits.max_zip_compression_ratio)
            {
                return Err(resource(
                    &raw_name,
                    "ZIP entry compression ratio exceeds the configured limit",
                ));
            }
            let declared_size = file.size();
            let capacity = usize::try_from(declared_size)
                .map_err(|_| resource(&raw_name, "ZIP entry does not fit addressable memory"))?;
            let mut content = Vec::with_capacity(capacity);
            file.by_ref()
                .take(declared_size.saturating_add(1))
                .read_to_end(&mut content)
                .map_err(|error| {
                    invalid(&raw_name, &format!("cannot decompress ZIP entry: {error}"))
                })?;
            if u64::try_from(content.len()).unwrap_or(u64::MAX) != declared_size {
                return Err(invalid(
                    &raw_name,
                    "ZIP entry size does not match its directory metadata",
                ));
            }
            if parts.insert(raw_name.clone(), content).is_some() {
                return Err(invalid(&raw_name, "duplicate ZIP entry name"));
            }
        }

        let mut hardened = BTreeSet::new();
        for (name, content) in &parts {
            if has_xml_part_name(name) {
                validate_xml(name, content, limits)?;
                hardened.insert(name.clone());
            }
        }
        for required in ["[Content_Types].xml", "_rels/.rels", "xl/workbook.xml"] {
            if !parts.contains_key(required) {
                return Err(invalid(required, "required OOXML part is missing"));
            }
        }

        let mut package = Self {
            parts,
            hardened,
            macro_enabled: false,
        };
        let content_types = ContentTypes::parse(package.part("[Content_Types].xml")?, limits)?;
        let mut total_relationships = 0_usize;
        for rels_part in package.parts.keys().filter(|name| {
            name.as_str() == "_rels/.rels"
                || std::path::Path::new(name)
                    .extension()
                    .is_some_and(|value| value.eq_ignore_ascii_case("rels"))
        }) {
            let source_part = source_part_for_relationships(rels_part)?;
            let relationships = package.relationships(rels_part, &source_part, limits)?;
            total_relationships = total_relationships
                .checked_add(relationships.len())
                .ok_or_else(|| resource(rels_part, "package relationship count overflow"))?;
            if total_relationships > limits.max_relationships {
                return Err(resource(
                    rels_part,
                    "package relationship count exceeds the configured limit",
                ));
            }
            for relationship in relationships {
                content_types.validate_relationship(&relationship, rels_part)?;
            }
        }
        let root = package.relationships("_rels/.rels", "", limits)?;
        let office = root
            .iter()
            .find(|relationship| relationship.kind.ends_with("/officeDocument"))
            .ok_or_else(|| invalid("_rels/.rels", "root officeDocument relationship is missing"))?;
        if office.target != "xl/workbook.xml" {
            return Err(invalid(
                "_rels/.rels",
                "root officeDocument relationship must target xl/workbook.xml",
            ));
        }
        package.macro_enabled =
            content_types
                .content_type(&office.target)
                .is_some_and(|content_type| {
                    content_type.eq_ignore_ascii_case(
                        "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
                    )
                });
        Ok(package)
    }

    fn part(&self, name: &str) -> Result<&[u8], ConvertError> {
        self.parts
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| invalid(name, "referenced OOXML part is missing"))
    }

    /// Returns a part that is about to be parsed as XML, hardening it first.
    ///
    /// Parts are selected by relationship target rather than by name, so a
    /// worksheet, styles, sharedStrings, or table part may carry any name the
    /// package author chooses. The name-based pass in [`Package::open`] would
    /// skip such a part entirely, so hardening runs again here, at the point
    /// the part is claimed for parsing, for anything `open` did not cover.
    pub(super) fn xml_part(
        &self,
        name: &str,
        limits: ConversionLimits,
    ) -> Result<&[u8], ConvertError> {
        let content = self.part(name)?;
        if !self.hardened.contains(name) {
            validate_xml(name, content, limits)?;
        }
        Ok(content)
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.parts.keys().map(String::as_str)
    }

    pub(super) const fn is_macro_enabled(&self) -> bool {
        self.macro_enabled
    }

    pub(super) fn relationship_inventory(
        &self,
        limits: ConversionLimits,
    ) -> Result<Vec<RelationshipInventory>, ConvertError> {
        let mut inventory = Vec::new();
        for rels_part in self.parts.keys().filter(|name| {
            name.as_str() == "_rels/.rels"
                || std::path::Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("rels"))
        }) {
            let source = source_part_for_relationships(rels_part)?;
            for relationship in self.relationships(rels_part, &source, limits)? {
                if inventory.len() >= limits.max_relationships {
                    return Err(resource(
                        rels_part,
                        "package relationship count exceeds the configured limit",
                    ));
                }
                inventory.push(RelationshipInventory {
                    rels_part: rels_part.clone(),
                    source: source.clone(),
                    relationship,
                });
            }
        }
        Ok(inventory)
    }

    pub(super) fn relationships(
        &self,
        rels_part: &str,
        source_part: &str,
        limits: ConversionLimits,
    ) -> Result<Vec<Relationship>, ConvertError> {
        let bytes = self.xml_part(rels_part, limits)?;
        let mut reader = Reader::from_reader(bytes);
        let mut relationships = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(element) | Event::Empty(element))
                    if local_name(element.name().as_ref()) == b"Relationship" =>
                {
                    if relationships.len() >= limits.max_relationships {
                        return Err(resource(rels_part, "relationship count exceeds the limit"));
                    }
                    if attribute(&reader, &element, b"TargetMode", rels_part)?
                        .is_some_and(|mode| mode.eq_ignore_ascii_case("external"))
                    {
                        return Err(ConvertError::new(
                            ConvertErrorCode::UnsupportedPackage,
                            "external OOXML relationships are rejected",
                        ));
                    }
                    let id = required_attribute(&reader, &element, b"Id", rels_part)?;
                    if relationships
                        .iter()
                        .any(|candidate: &Relationship| candidate.id == id)
                    {
                        return Err(invalid(rels_part, "duplicate relationship identifier"));
                    }
                    let target = required_attribute(&reader, &element, b"Target", rels_part)?;
                    let target = resolve_target(source_part, &target)
                        .map_err(|message| invalid(rels_part, message))?;
                    if !self.parts.contains_key(&target) {
                        return Err(invalid(rels_part, "relationship target part is missing"));
                    }
                    relationships.push(Relationship {
                        id,
                        kind: required_attribute(&reader, &element, b"Type", rels_part)?,
                        target,
                    });
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(error) => {
                    return Err(invalid(
                        rels_part,
                        &format!("malformed relationships XML: {error}"),
                    ));
                }
            }
        }
        Ok(relationships)
    }
}

#[derive(Clone, Debug, Default)]
struct ContentTypes {
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
}

impl ContentTypes {
    fn parse(bytes: &[u8], limits: ConversionLimits) -> Result<Self, ConvertError> {
        let part = "[Content_Types].xml";
        let mut reader = Reader::from_reader(bytes);
        let mut content_types = Self::default();
        let mut entries = 0_usize;
        loop {
            match reader.read_event() {
                Ok(Event::Start(element) | Event::Empty(element)) => {
                    match local_name(element.name().as_ref()) {
                        b"Default" => {
                            entries = entries.checked_add(1).ok_or_else(|| {
                                resource(part, "content type entry count overflow")
                            })?;
                            let extension =
                                required_attribute(&reader, &element, b"Extension", part)?;
                            if extension.is_empty()
                                || extension.contains(['/', '\\', '.'])
                                || !extension.is_ascii()
                            {
                                return Err(invalid(part, "content type extension is invalid"));
                            }
                            let key = extension.to_ascii_lowercase();
                            let value =
                                required_attribute(&reader, &element, b"ContentType", part)?;
                            if content_types.defaults.insert(key, value).is_some() {
                                return Err(invalid(part, "duplicate default content type"));
                            }
                        }
                        b"Override" => {
                            entries = entries.checked_add(1).ok_or_else(|| {
                                resource(part, "content type entry count overflow")
                            })?;
                            let package_name =
                                required_attribute(&reader, &element, b"PartName", part)?;
                            let name = package_name.strip_prefix('/').ok_or_else(|| {
                                invalid(part, "override part name must be package-absolute")
                            })?;
                            validate_part_name(name)?;
                            let value =
                                required_attribute(&reader, &element, b"ContentType", part)?;
                            if content_types
                                .overrides
                                .insert(name.to_owned(), value)
                                .is_some()
                            {
                                return Err(invalid(part, "duplicate override content type"));
                            }
                        }
                        _ => {}
                    }
                    if entries > limits.max_zip_entries {
                        return Err(resource(part, "content type entry count exceeds the limit"));
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(error) => {
                    return Err(invalid(
                        part,
                        &format!("malformed content types XML: {error}"),
                    ));
                }
            }
        }
        Ok(content_types)
    }

    fn content_type(&self, part: &str) -> Option<&str> {
        self.overrides.get(part).map(String::as_str).or_else(|| {
            std::path::Path::new(part)
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| self.defaults.get(&extension.to_ascii_lowercase()))
                .map(String::as_str)
        })
    }

    fn validate_relationship(
        &self,
        relationship: &Relationship,
        rels_part: &str,
    ) -> Result<(), ConvertError> {
        let expected: &[&str] = if relationship.kind.ends_with("/officeDocument") {
            &[
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
                "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
            ]
        } else if relationship.kind.ends_with("/worksheet") {
            &["application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"]
        } else if relationship.kind.ends_with("/styles") {
            &["application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"]
        } else if relationship.kind.ends_with("/sharedStrings") {
            &["application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"]
        } else if relationship.kind.ends_with("/table") {
            &["application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml"]
        } else {
            return Ok(());
        };
        let actual = self.content_type(&relationship.target).ok_or_else(|| {
            invalid(
                rels_part,
                "relationship target has no declared content type",
            )
        })?;
        if !expected.contains(&actual) {
            return Err(invalid(
                rels_part,
                "relationship target content type does not match its relationship kind",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(super) struct Relationship {
    pub id: String,
    pub kind: String,
    pub target: String,
}

#[derive(Clone, Debug)]
pub(super) struct RelationshipInventory {
    pub rels_part: String,
    pub source: String,
    pub relationship: Relationship,
}

pub(super) fn relationships_part(source_part: &str) -> String {
    match source_part.rsplit_once('/') {
        Some((directory, file)) => format!("{directory}/_rels/{file}.rels"),
        None => format!("_rels/{source_part}.rels"),
    }
}

fn has_xml_part_name(name: &str) -> bool {
    std::path::Path::new(name).extension().is_some_and(|value| {
        value.eq_ignore_ascii_case("xml") || value.eq_ignore_ascii_case("rels")
    }) || name == "[Content_Types].xml"
        || name == "_rels/.rels"
}

fn validate_part_name(name: &str) -> Result<(), ConvertError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains('?')
        || name.contains('#')
        || name.contains(':')
        || name
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid(name, "non-canonical or unsafe ZIP entry name"));
    }
    let path = std::path::Path::new(name);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid(name, "unsafe ZIP entry path"));
    }
    Ok(())
}

fn resolve_target(source_part: &str, target: &str) -> Result<String, &'static str> {
    if target.is_empty()
        || target.starts_with('/')
        || target.contains('\\')
        || target.contains(':')
        || target.contains('?')
        || target.contains('#')
    {
        return Err("relationship target is absolute, external, or malformed");
    }
    let mut components: Vec<&str> = source_part
        .rsplit_once('/')
        .map_or_else(Vec::new, |(directory, _)| directory.split('/').collect());
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err("relationship target escapes the package root");
                }
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        return Err("relationship target resolves to the package root");
    }
    Ok(components.join("/"))
}

fn source_part_for_relationships(rels_part: &str) -> Result<String, ConvertError> {
    if rels_part == "_rels/.rels" {
        return Ok(String::new());
    }
    let (directory, file) = rels_part
        .rsplit_once("/_rels/")
        .ok_or_else(|| invalid(rels_part, "relationship part is not in an _rels directory"))?;
    let source_file = file
        .strip_suffix(".rels")
        .ok_or_else(|| invalid(rels_part, "relationship part has an invalid suffix"))?;
    Ok(format!("{directory}/{source_file}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::{ZipWriter, write::SimpleFileOptions};

    const CONTENT_TYPES: &str = "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/></Types>";
    const ROOT_RELS: &str = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";

    fn package(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn minimal_entries() -> [(&'static str, &'static str); 3] {
        [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", "<workbook/>"),
        ]
    }

    #[test]
    fn target_resolution_stays_inside_package() {
        assert_eq!(
            resolve_target("xl/worksheets/sheet1.xml", "../tables/table1.xml").unwrap(),
            "xl/tables/table1.xml"
        );
        assert!(resolve_target("xl/workbook.xml", "../../outside").is_err());
        assert!(resolve_target("xl/workbook.xml", "https://example.com").is_err());
    }

    #[test]
    fn rejects_external_relationships() {
        let external = "<Relationships><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"https://example.com/workbook.xml\" TargetMode=\"External\"/></Relationships>";
        let bytes = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", external),
            ("xl/workbook.xml", "<workbook/>"),
        ]);
        let error = Package::open(&bytes, ConversionLimits::default()).unwrap_err();
        assert_eq!(error.code, ConvertErrorCode::UnsupportedPackage);
    }

    #[test]
    fn rejects_document_types_before_semantic_parsing() {
        let bytes = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            (
                "xl/workbook.xml",
                "<!DOCTYPE workbook [<!ENTITY payload \"value\">]><workbook/>",
            ),
        ]);
        let error = Package::open(&bytes, ConversionLimits::default()).unwrap_err();
        assert_eq!(error.code, ConvertErrorCode::UnsupportedPackage);
    }

    #[test]
    fn rejects_unsafe_and_case_alias_entry_names() {
        let mut traversal = minimal_entries().to_vec();
        traversal.push(("../escape.xml", "<escape/>"));
        let error = Package::open(&package(&traversal), ConversionLimits::default()).unwrap_err();
        assert_eq!(error.code, ConvertErrorCode::InvalidPackage);

        let mut alias = minimal_entries().to_vec();
        alias.push(("XL/workbook.xml", "<workbook/>"));
        let error = Package::open(&package(&alias), ConversionLimits::default()).unwrap_err();
        assert_eq!(error.code, ConvertErrorCode::InvalidPackage);
    }

    #[test]
    fn rejects_relationship_content_type_mismatch() {
        let mismatched = "<Types><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/xml\"/></Types>";
        let bytes = package(&[
            ("[Content_Types].xml", mismatched),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", "<workbook/>"),
        ]);
        let error = Package::open(&bytes, ConversionLimits::default()).unwrap_err();
        assert_eq!(error.code, ConvertErrorCode::InvalidPackage);
    }

    #[test]
    fn enforces_entry_and_xml_event_budgets() {
        let bytes = package(&minimal_entries());
        let limits = ConversionLimits {
            max_zip_entries: 2,
            ..ConversionLimits::default()
        };
        assert_eq!(
            Package::open(&bytes, limits).unwrap_err().code,
            ConvertErrorCode::ResourceLimit
        );

        let limits = ConversionLimits {
            max_xml_events: 1,
            ..ConversionLimits::default()
        };
        assert_eq!(
            Package::open(&bytes, limits).unwrap_err().code,
            ConvertErrorCode::ResourceLimit
        );
    }

    #[test]
    fn relationship_budget_is_package_global() {
        let workbook_rels = "<Relationships><Relationship Id=\"rMetadata\" Type=\"http://example.test/metadata\" Target=\"metadata.xml\"/></Relationships>";
        let bytes = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", "<workbook/>"),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
            ("xl/metadata.xml", "<metadata/>"),
        ]);
        let limits = ConversionLimits {
            max_relationships: 1,
            ..ConversionLimits::default()
        };
        assert_eq!(
            Package::open(&bytes, limits).unwrap_err().code,
            ConvertErrorCode::ResourceLimit
        );
    }
}
