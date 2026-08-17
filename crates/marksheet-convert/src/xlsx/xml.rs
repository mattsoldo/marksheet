use quick_xml::{Reader, XmlVersion, events::Event};

use crate::{ConversionLimits, ConversionLocation, ConvertError, ConvertErrorCode};

pub(super) fn validate_xml(
    part: &str,
    bytes: &[u8],
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().check_comments = true;
    reader.config_mut().check_end_names = true;
    let mut depth = 0_usize;
    let mut events = 0_usize;
    loop {
        events = events
            .checked_add(1)
            .ok_or_else(|| resource(part, "XML event count overflow"))?;
        if events > limits.max_xml_events {
            return Err(resource(
                part,
                "XML event count exceeds the configured limit",
            ));
        }
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| resource(part, "XML depth overflow"))?;
                if depth > limits.max_xml_depth {
                    return Err(resource(part, "XML depth exceeds the configured limit"));
                }
                validate_attributes(part, &reader, &element, limits)?;
            }
            Ok(Event::Empty(element)) => validate_attributes(part, &reader, &element, limits)?,
            Ok(Event::End(_)) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid(part, "XML closes an element that was not open"))?;
            }
            Ok(Event::Text(text)) => {
                let decoded = text.decode().map_err(|error| {
                    invalid(part, &format!("invalid XML text encoding: {error}"))
                })?;
                if decoded.len() > limits.max_string_bytes {
                    return Err(resource(
                        part,
                        "XML text exceeds the configured string limit",
                    ));
                }
            }
            Ok(Event::CData(text)) => {
                if text.len() > limits.max_string_bytes {
                    return Err(resource(
                        part,
                        "XML CDATA exceeds the configured string limit",
                    ));
                }
            }
            Ok(Event::DocType(_)) => {
                return Err(ConvertError::new(
                    ConvertErrorCode::UnsupportedPackage,
                    "OOXML parts must not contain a document type declaration",
                )
                .at(xlsx_location(part, Some("DOCTYPE"))));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(invalid(part, &format!("malformed XML: {error}")));
            }
        }
    }
    if depth != 0 {
        return Err(invalid(part, "XML document has unclosed elements"));
    }
    Ok(())
}

fn validate_attributes(
    part: &str,
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    limits: ConversionLimits,
) -> Result<(), ConvertError> {
    let mut count = 0_usize;
    for attribute in element.attributes().with_checks(true) {
        let attribute = attribute
            .map_err(|error| invalid(part, &format!("invalid or duplicate attribute: {error}")))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| resource(part, "XML attribute count overflow"))?;
        if count > limits.max_xml_attributes {
            return Err(resource(
                part,
                "XML element attribute count exceeds the configured limit",
            ));
        }
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|error| invalid(part, &format!("invalid XML attribute: {error}")))?;
        if value.len() > limits.max_string_bytes.min(limits.max_xml_attribute_bytes) {
            return Err(resource(part, "XML attribute exceeds the configured limit"));
        }
    }
    Ok(())
}

pub(super) fn attribute(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    part: &str,
) -> Result<Option<String>, ConvertError> {
    for candidate in element.attributes().with_checks(true) {
        let candidate =
            candidate.map_err(|error| invalid(part, &format!("invalid XML attribute: {error}")))?;
        if local_name(candidate.key.as_ref()) == name {
            return candidate
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map(|value| Some(value.into_owned()))
                .map_err(|error| invalid(part, &format!("invalid XML attribute value: {error}")));
        }
    }
    Ok(None)
}

pub(super) fn required_attribute(
    reader: &Reader<&[u8]>,
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    part: &str,
) -> Result<String, ConvertError> {
    attribute(reader, element, name, part)?.ok_or_else(|| {
        invalid(
            part,
            &format!(
                "element {} is missing attribute {}",
                String::from_utf8_lossy(local_name(element.name().as_ref())),
                String::from_utf8_lossy(name)
            ),
        )
    })
}

pub(super) fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

pub(super) fn escape_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
    output
}

pub(super) fn escape_attribute(value: &str) -> String {
    escape_text(value).replace('"', "&quot;")
}

pub(super) fn invalid(part: &str, message: &str) -> ConvertError {
    ConvertError::new(ConvertErrorCode::InvalidPackage, message).at(xlsx_location(part, None))
}

pub(super) fn resource(part: &str, message: &str) -> ConvertError {
    ConvertError::new(ConvertErrorCode::ResourceLimit, message).at(xlsx_location(part, None))
}

pub(super) fn xlsx_location(part: &str, reference: Option<&str>) -> ConversionLocation {
    ConversionLocation::Xlsx {
        part: part.to_owned(),
        reference: reference.map(str::to_owned),
    }
}
