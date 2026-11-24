//! Strict, bounded reader for the explicitly supported Digital `.dig` v2 subset.
//!
//! This module intentionally returns a format-specific representation; it does not
//! resolve component names, mutate the editor document, or perform file I/O.

use crate::domain::{
    Component, ComponentId, ComponentKind, Document, PinDirection, PinId, PinRef, Position,
};
use std::collections::BTreeSet;
#[cfg(test)]
use std::collections::{BTreeMap, VecDeque};

const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 10_000;
const MAX_WIRES: usize = 30_000;
const MAX_STRING_BYTES: usize = 4 * 1024;
const MAX_ATTRIBUTES_PER_ELEMENT: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigV2 {
    pub elements: Vec<DigElement>,
    pub wires: Vec<DigWire>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigElement {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub label: Option<String>,
    /// Explicit Digital Const value; None distinguishes an omitted default from explicit HIGH.
    pub value: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigWire {
    pub p1: DigPoint,
    pub p2: DigPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigReadErrorKind {
    InputLimit,
    InvalidUtf8,
    Xml,
    Unsupported,
    MissingField,
    DuplicateField,
    InvalidValue,
    Limit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigReadError {
    pub kind: DigReadErrorKind,
    pub offset: usize,
    pub context: String,
    pub message: String,
}

impl std::fmt::Display for DigReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at byte {} ({}): {}",
            self.kind, self.offset, self.context, self.message
        )
    }
}

impl std::error::Error for DigReadError {}

const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigWriteErrorKind {
    InvalidXmlCharacter,
    Limit,
    OutputLimit,
    UnsupportedValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigWriteError {
    pub kind: DigWriteErrorKind,
    pub context: String,
    pub message: String,
}

impl std::fmt::Display for DigWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.kind, self.context, self.message)
    }
}

impl std::error::Error for DigWriteError {}

impl std::fmt::Display for DigWriteErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidXmlCharacter => "invalid XML character",
            Self::Limit => "resource limit",
            Self::OutputLimit => "output limit",
            Self::UnsupportedValue => "unsupported value",
        })
    }
}

/// Serialize the supported `.dig` v2 subset as deterministic UTF-8 XML.
/// Measurement ordering is deliberately omitted because it is not represented by `DigV2`.
pub fn serialize_v2(document: &DigV2) -> Result<Vec<u8>, DigWriteError> {
    if document.elements.len() > MAX_ELEMENTS {
        return Err(write_error(
            DigWriteErrorKind::Limit,
            "elements",
            "element count exceeds 10,000",
        ));
    }
    if document.wires.len() > MAX_WIRES {
        return Err(write_error(
            DigWriteErrorKind::Limit,
            "wires",
            "wire count exceeds 30,000",
        ));
    }
    for (index, element) in document.elements.iter().enumerate() {
        validate_write_string(&element.name, &format!("elements[{index}].name"))?;
        if element.value.is_some() && element.name != "Const" {
            return Err(write_error(
                DigWriteErrorKind::UnsupportedValue,
                format!("elements[{index}].Value"),
                "Value is supported only for Const",
            ));
        }
        if let Some(label) = &element.label {
            if let Err(validation) = crate::domain::validate_label(label) {
                let kind = match validation {
                    crate::domain::LabelError::TooLong { .. } => DigWriteErrorKind::Limit,
                    crate::domain::LabelError::InvalidXmlCharacter { .. } => {
                        DigWriteErrorKind::InvalidXmlCharacter
                    }
                };
                return Err(write_error(
                    kind,
                    format!("elements[{index}].label"),
                    validation.to_string(),
                ));
            }
            validate_write_string(label, &format!("elements[{index}].label"))?;
        }
    }

    let mut output = Vec::new();
    write_bytes(&mut output, b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<circuit><version>2</version><attributes/><visualElements", "document")?;
    write_bytes(
        &mut output,
        if document.elements.is_empty() {
            b"/>"
        } else {
            b">"
        },
        "visualElements",
    )?;
    for (index, element) in document.elements.iter().enumerate() {
        let context = format!("elements[{index}]");
        write_bytes(&mut output, b"<visualElement><elementName>", &context)?;
        write_text(&mut output, &element.name, &format!("{context}.name"))?;
        write_bytes(&mut output, b"</elementName><elementAttributes", &context)?;
        if element.label.is_none() && element.value.is_none() {
            write_bytes(&mut output, b"/>", &context)?;
        } else {
            write_bytes(&mut output, b">", &context)?;
            if let Some(label) = &element.label {
                write_bytes(
                    &mut output,
                    b"<entry><string>Label</string><string>",
                    &context,
                )?;
                write_text(&mut output, label, &format!("{context}.label"))?;
                write_bytes(&mut output, b"</string></entry>", &context)?;
            }
            if let Some(value) = element.value {
                write_bytes(
                    &mut output,
                    b"<entry><string>Value</string><long>",
                    &context,
                )?;
                write_bytes(&mut output, if value { b"1" } else { b"0" }, &context)?;
                write_bytes(&mut output, b"</long></entry>", &context)?;
            }
            write_bytes(&mut output, b"</elementAttributes>", &context)?;
        }
        write_bytes(&mut output, b"<pos x=\"", &context)?;
        write_bytes(&mut output, element.x.to_string().as_bytes(), &context)?;
        write_bytes(&mut output, b"\" y=\"", &context)?;
        write_bytes(&mut output, element.y.to_string().as_bytes(), &context)?;
        write_bytes(&mut output, b"\"/></visualElement>", &context)?;
    }
    if !document.elements.is_empty() {
        write_bytes(&mut output, b"</visualElements>", "visualElements")?;
    }
    write_bytes(&mut output, b"<wires", "wires")?;
    write_bytes(
        &mut output,
        if document.wires.is_empty() {
            b"/>"
        } else {
            b">"
        },
        "wires",
    )?;
    for (index, wire) in document.wires.iter().enumerate() {
        let context = format!("wires[{index}]");
        write_bytes(&mut output, b"<wire><p1 x=\"", &context)?;
        write_point(&mut output, wire.p1, &context)?;
        write_bytes(&mut output, b"/><p2 x=\"", &context)?;
        write_point(&mut output, wire.p2, &context)?;
        write_bytes(&mut output, b"/></wire>", &context)?;
    }
    if !document.wires.is_empty() {
        write_bytes(&mut output, b"</wires>", "wires")?;
    }
    write_bytes(&mut output, b"</circuit>", "document")?;
    Ok(output)
}

fn write_point(output: &mut Vec<u8>, point: DigPoint, context: &str) -> Result<(), DigWriteError> {
    write_bytes(output, point.x.to_string().as_bytes(), context)?;
    write_bytes(output, b"\" y=\"", context)?;
    write_bytes(output, point.y.to_string().as_bytes(), context)?;
    write_bytes(output, b"\"", context)
}

fn write_bytes(output: &mut Vec<u8>, bytes: &[u8], context: &str) -> Result<(), DigWriteError> {
    if output
        .len()
        .checked_add(bytes.len())
        .is_none_or(|length| length > MAX_OUTPUT_BYTES)
    {
        return Err(write_error(
            DigWriteErrorKind::OutputLimit,
            context,
            "serialized output exceeds 4 MiB",
        ));
    }
    output.reserve_exact(bytes.len());
    output.extend_from_slice(bytes);
    Ok(())
}

fn write_text(output: &mut Vec<u8>, value: &str, context: &str) -> Result<(), DigWriteError> {
    for ch in value.chars() {
        let escaped: &[u8] = match ch {
            '&' => b"&amp;",
            '<' => b"&lt;",
            '>' => b"&gt;",
            '\r' => b"&#13;",
            _ => {
                let mut utf8 = [0; 4];
                write_bytes(output, ch.encode_utf8(&mut utf8).as_bytes(), context)?;
                continue;
            }
        };
        write_bytes(output, escaped, context)?;
    }
    Ok(())
}

fn validate_write_string(value: &str, context: &str) -> Result<(), DigWriteError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(write_error(
            DigWriteErrorKind::Limit,
            context,
            "decoded string exceeds 4 KiB",
        ));
    }
    if value.chars().any(|ch| !matches!(ch as u32, 0x9 | 0xa | 0xd | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)) {
        return Err(write_error(DigWriteErrorKind::InvalidXmlCharacter, context, "string contains a character forbidden by XML 1.0"));
    }
    Ok(())
}

fn write_error(
    kind: DigWriteErrorKind,
    context: impl Into<String>,
    message: impl Into<String>,
) -> DigWriteError {
    DigWriteError {
        kind,
        context: context.into(),
        message: message.into(),
    }
}

impl std::fmt::Display for DigReadErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::InputLimit => "input limit",
            Self::InvalidUtf8 => "invalid UTF-8",
            Self::Xml => "XML error",
            Self::Unsupported => "unsupported content",
            Self::MissingField => "missing field",
            Self::DuplicateField => "duplicate field",
            Self::InvalidValue => "invalid value",
            Self::Limit => "resource limit",
        };
        f.write_str(name)
    }
}

/// Parse strict `.dig` XML version 2, rejecting every field not represented here.
pub fn parse_v2(bytes: &[u8]) -> Result<DigV2, DigReadError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(error(
            DigReadErrorKind::InputLimit,
            MAX_INPUT_BYTES,
            "document",
            "input exceeds 4 MiB",
        ));
    }
    let input = std::str::from_utf8(bytes).map_err(|e| {
        error(
            DigReadErrorKind::InvalidUtf8,
            e.valid_up_to(),
            "document",
            "input must be UTF-8",
        )
    })?;
    if let Some((offset, _)) = input.char_indices().find(|(_, ch)| !matches!(*ch as u32, 0x9 | 0xa | 0xd | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)) {
        return Err(error(DigReadErrorKind::Xml, offset, "document", "input contains a character forbidden by XML 1.0"));
    }
    let root = XmlParser::new(input).parse()?;
    if root.name != "circuit" {
        return Err(error(
            DigReadErrorKind::Unsupported,
            root.offset,
            "root",
            "expected <circuit>",
        ));
    }
    no_attributes(&root, "circuit")?;
    whitespace_text(&root, "circuit")?;
    let mut version = None;
    let mut attrs = None;
    let mut visual = None;
    let mut wires = None;
    let mut measurement = None;
    for child in &root.children {
        let slot = match child.name.as_str() {
            "version" => &mut version,
            "attributes" => &mut attrs,
            "visualElements" => &mut visual,
            "wires" => &mut wires,
            "measurementOrdering" => &mut measurement,
            other => {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    child.offset,
                    other,
                    "unsupported circuit field",
                ));
            }
        };
        if slot.replace(child).is_some() {
            return Err(error(
                DigReadErrorKind::DuplicateField,
                child.offset,
                child.name.clone(),
                "field occurs more than once",
            ));
        }
    }
    let version = version.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            root.offset,
            "version",
            "required field is absent",
        )
    })?;
    let version_text = leaf(version, "version")?;
    let version_number = trim_xml_space(version_text).parse::<u32>().map_err(|_| {
        error(
            DigReadErrorKind::InvalidValue,
            version.offset,
            "version",
            "expected integer version",
        )
    })?;
    if version_number != 2 {
        return Err(error(
            DigReadErrorKind::Unsupported,
            version.offset,
            "version",
            format!("only explicit version 2 is supported (found {version_number})"),
        ));
    }
    let attrs = attrs.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            root.offset,
            "attributes",
            "required empty field is absent",
        )
    })?;
    require_empty(attrs, "attributes")?;
    if let Some(order) = measurement {
        require_empty(order, "measurementOrdering")?;
    }
    let visual = visual.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            root.offset,
            "visualElements",
            "required field is absent",
        )
    })?;
    no_attributes(visual, "visualElements")?;
    whitespace_text(visual, "visualElements")?;
    if visual.children.len() > MAX_ELEMENTS {
        return Err(error(
            DigReadErrorKind::Limit,
            visual.offset,
            "visualElements",
            "element count exceeds 10,000",
        ));
    }
    let mut elements = Vec::with_capacity(visual.children.len());
    for node in &visual.children {
        if node.name != "visualElement" {
            return Err(error(
                DigReadErrorKind::Unsupported,
                node.offset,
                "visualElements",
                format!("unsupported child <{}>", node.name),
            ));
        }
        elements.push(parse_element(node)?);
    }
    let wires_node = wires.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            root.offset,
            "wires",
            "required field is absent",
        )
    })?;
    no_attributes(wires_node, "wires")?;
    whitespace_text(wires_node, "wires")?;
    if wires_node.children.len() > MAX_WIRES {
        return Err(error(
            DigReadErrorKind::Limit,
            wires_node.offset,
            "wires",
            "wire count exceeds 30,000",
        ));
    }
    let mut parsed_wires = Vec::with_capacity(wires_node.children.len());
    for node in &wires_node.children {
        if node.name != "wire" {
            return Err(error(
                DigReadErrorKind::Unsupported,
                node.offset,
                "wires",
                format!("unsupported child <{}>", node.name),
            ));
        }
        parsed_wires.push(parse_wire(node)?);
    }
    Ok(DigV2 {
        elements,
        wires: parsed_wires,
    })
}

fn parse_element(node: &Node) -> Result<DigElement, DigReadError> {
    no_attributes(node, "visualElement")?;
    whitespace_text(node, "visualElement")?;
    let mut name = None;
    let mut attributes = None;
    let mut pos = None;
    for child in &node.children {
        let field = match child.name.as_str() {
            "elementName" => &mut name,
            "elementAttributes" => &mut attributes,
            "pos" => &mut pos,
            other => {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    child.offset,
                    other,
                    "unsupported visualElement field (rotation/resources are not supported)",
                ));
            }
        };
        if field.replace(child).is_some() {
            return Err(error(
                DigReadErrorKind::DuplicateField,
                child.offset,
                child.name.clone(),
                "field occurs more than once",
            ));
        }
    }
    let name_node = name.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            node.offset,
            "elementName",
            "required field is absent",
        )
    })?;
    let element_name = leaf(name_node, "elementName")?;
    bounded_string(element_name, name_node.offset, "elementName")?;
    let attr_node = attributes.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            node.offset,
            "elementAttributes",
            "required field is absent",
        )
    })?;
    no_attributes(attr_node, "elementAttributes")?;
    whitespace_text(attr_node, "elementAttributes")?;
    let mut label = None;
    let mut value = None;
    let mut value_offset = None;
    for entry in &attr_node.children {
        if entry.name != "entry" {
            return Err(error(
                DigReadErrorKind::Unsupported,
                entry.offset,
                "elementAttributes",
                format!("unsupported child <{}>", entry.name),
            ));
        }
        no_attributes(entry, "entry")?;
        whitespace_text(entry, "entry")?;
        if entry.children.len() != 2 || entry.children[0].name != "string" {
            return Err(error(
                DigReadErrorKind::Unsupported,
                entry.offset,
                "elementAttributes",
                "attribute entry must have a string key and supported value",
            ));
        }
        let key = leaf(&entry.children[0], "attribute key")?;
        let payload = &entry.children[1];
        let raw = leaf(payload, "attribute value")?;
        match key {
            "Label" => {
                if payload.name != "string" {
                    return Err(error(
                        DigReadErrorKind::Unsupported,
                        payload.offset,
                        "Label",
                        "Label requires a string value",
                    ));
                }
                if label.is_some() {
                    return Err(error(
                        DigReadErrorKind::DuplicateField,
                        entry.offset,
                        "Label",
                        "label occurs more than once",
                    ));
                }
                bounded_string(raw, payload.offset, "Label")?;
                crate::domain::validate_label(raw).map_err(|validation| {
                    error(
                        DigReadErrorKind::InvalidValue,
                        payload.offset,
                        "Label",
                        validation.to_string(),
                    )
                })?;
                label = Some(raw.to_owned());
            }
            "Value" => {
                if value.is_some() {
                    return Err(error(
                        DigReadErrorKind::DuplicateField,
                        entry.offset,
                        "Value",
                        "Value occurs more than once",
                    ));
                }
                if !matches!(payload.name.as_str(), "long" | "int") {
                    return Err(error(
                        DigReadErrorKind::Unsupported,
                        payload.offset,
                        "Value",
                        "Value must use a long or legacy int tag",
                    ));
                }
                let trimmed = trim_xml_space(raw);
                if trimmed.is_empty()
                    || !trimmed.bytes().enumerate().all(|(i, b)| {
                        b.is_ascii_digit()
                            || (i == 0 && b == b'-' && trimmed.len() > 1)
                            || (i == 0 && b == b'+' && trimmed.len() > 1)
                    })
                {
                    return Err(error(
                        DigReadErrorKind::InvalidValue,
                        payload.offset,
                        "Value",
                        "expected signed decimal 0 or 1",
                    ));
                }
                let parsed: i64 = trimmed.parse().map_err(|_| {
                    error(
                        DigReadErrorKind::InvalidValue,
                        payload.offset,
                        "Value",
                        "signed decimal value is out of range",
                    )
                })?;
                value_offset = Some(payload.offset);
                value = Some(match parsed {
                    0 => false,
                    1 => true,
                    _ => {
                        return Err(error(
                            DigReadErrorKind::InvalidValue,
                            payload.offset,
                            "Value",
                            "one-bit Const accepts only 0 or 1",
                        ));
                    }
                });
            }
            _ => {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    entry.children[0].offset,
                    "elementAttributes",
                    format!("unsupported attribute {key:?}"),
                ));
            }
        }
    }
    if element_name != "Const" && value.is_some() {
        return Err(error(
            DigReadErrorKind::Unsupported,
            value_offset.unwrap_or(attr_node.offset),
            "Value",
            "Value is supported only for Const",
        ));
    }
    let pos_node = pos.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            node.offset,
            "pos",
            "required field is absent",
        )
    })?;
    let point = parse_point(pos_node, "pos")?;
    Ok(DigElement {
        name: element_name.to_owned(),
        x: point.x,
        y: point.y,
        label,
        value,
    })
}

fn parse_wire(node: &Node) -> Result<DigWire, DigReadError> {
    no_attributes(node, "wire")?;
    whitespace_text(node, "wire")?;
    let mut p1 = None;
    let mut p2 = None;
    for child in &node.children {
        let field = match child.name.as_str() {
            "p1" => &mut p1,
            "p2" => &mut p2,
            other => {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    child.offset,
                    other,
                    "unsupported wire field",
                ));
            }
        };
        if field.replace(child).is_some() {
            return Err(error(
                DigReadErrorKind::DuplicateField,
                child.offset,
                child.name.clone(),
                "field occurs more than once",
            ));
        }
    }
    let p1_node = p1.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            node.offset,
            "p1",
            "required endpoint is absent",
        )
    })?;
    let p2_node = p2.ok_or_else(|| {
        error(
            DigReadErrorKind::MissingField,
            node.offset,
            "p2",
            "required endpoint is absent",
        )
    })?;
    Ok(DigWire {
        p1: parse_point(p1_node, "p1")?,
        p2: parse_point(p2_node, "p2")?,
    })
}

fn parse_point(node: &Node, context: &str) -> Result<DigPoint, DigReadError> {
    if !node.children.is_empty() || !all_xml_space(&node.text) {
        return Err(error(
            DigReadErrorKind::Unsupported,
            node.offset,
            context,
            "point must be an empty element with x and y attributes",
        ));
    }
    let mut x = None;
    let mut y = None;
    for (key, value) in &node.attributes {
        let slot = match key.as_str() {
            "x" => &mut x,
            "y" => &mut y,
            other => {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    node.offset,
                    context,
                    format!("unsupported coordinate attribute {other}"),
                ));
            }
        };
        if slot.replace(value.as_str()).is_some() {
            return Err(error(
                DigReadErrorKind::DuplicateField,
                node.offset,
                format!("{context}.{key}"),
                "coordinate occurs more than once",
            ));
        }
    }
    let x = coordinate(
        x.ok_or_else(|| {
            error(
                DigReadErrorKind::MissingField,
                node.offset,
                format!("{context}.x"),
                "coordinate is absent",
            )
        })?,
        node.offset,
        &format!("{context}.x"),
    )?;
    let y = coordinate(
        y.ok_or_else(|| {
            error(
                DigReadErrorKind::MissingField,
                node.offset,
                format!("{context}.y"),
                "coordinate is absent",
            )
        })?,
        node.offset,
        &format!("{context}.y"),
    )?;
    Ok(DigPoint { x, y })
}

fn coordinate(value: &str, offset: usize, context: &str) -> Result<i32, DigReadError> {
    if value.is_empty()
        || value.trim() != value
        || !value
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_digit() || (i == 0 && (b == b'-' || b == b'+')))
    {
        return Err(error(
            DigReadErrorKind::InvalidValue,
            offset,
            context,
            "expected a signed decimal i32 integer",
        ));
    }
    value.parse::<i32>().map_err(|_| {
        error(
            DigReadErrorKind::InvalidValue,
            offset,
            context,
            "coordinate is outside the i32 range",
        )
    })
}

fn bounded_string(value: &str, offset: usize, context: &str) -> Result<(), DigReadError> {
    if value.len() > MAX_STRING_BYTES {
        Err(error(
            DigReadErrorKind::Limit,
            offset,
            context,
            "decoded string exceeds 4 KiB",
        ))
    } else {
        Ok(())
    }
}

fn require_empty(node: &Node, context: &str) -> Result<(), DigReadError> {
    no_attributes(node, context)?;
    if !node.children.is_empty() || !all_xml_space(&node.text) {
        return Err(error(
            DigReadErrorKind::Unsupported,
            node.offset,
            context,
            "only an empty field is supported",
        ));
    }
    Ok(())
}

fn leaf<'a>(node: &'a Node, context: &str) -> Result<&'a str, DigReadError> {
    no_attributes(node, context)?;
    if !node.children.is_empty() {
        return Err(error(
            DigReadErrorKind::Unsupported,
            node.offset,
            context,
            "nested markup is unsupported",
        ));
    }
    Ok(&node.text)
}

fn no_attributes(node: &Node, context: &str) -> Result<(), DigReadError> {
    if !node.attributes.is_empty() {
        return Err(error(
            DigReadErrorKind::Unsupported,
            node.offset,
            context,
            "unexpected XML attribute",
        ));
    }
    Ok(())
}

fn whitespace_text(node: &Node, context: &str) -> Result<(), DigReadError> {
    if !all_xml_space(&node.text) {
        return Err(error(
            DigReadErrorKind::Unsupported,
            node.offset,
            context,
            "unexpected text content",
        ));
    }
    Ok(())
}

fn all_xml_space(value: &str) -> bool {
    value
        .chars()
        .all(|ch| matches!(ch, '\t' | '\n' | '\r' | ' '))
}

fn trim_xml_space(value: &str) -> &str {
    value.trim_matches(|ch| matches!(ch, '\t' | '\n' | '\r' | ' '))
}

fn error(
    kind: DigReadErrorKind,
    offset: usize,
    context: impl Into<String>,
    message: impl Into<String>,
) -> DigReadError {
    DigReadError {
        kind,
        offset,
        context: context.into(),
        message: message.into(),
    }
}

#[derive(Debug)]
struct Node {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
    offset: usize,
}

struct XmlParser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    at: usize,
    stack: Vec<Node>,
    root: Option<Node>,
}

impl<'a> XmlParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            at: 0,
            stack: Vec::new(),
            root: None,
        }
    }

    fn parse(mut self) -> Result<Node, DigReadError> {
        let consumed_bom = self.bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        if consumed_bom {
            self.at = 3;
        }
        while self.at < self.bytes.len() {
            if self.bytes[self.at] != b'<' {
                let start = self.at;
                while self.at < self.bytes.len() && self.bytes[self.at] != b'<' {
                    self.at += 1;
                }
                if self.source[start..self.at].contains("]]>") {
                    return Err(error(
                        DigReadErrorKind::Xml,
                        start,
                        "text",
                        "']]>' is forbidden in XML character data",
                    ));
                }
                if let Some(parent) = self.stack.last_mut() {
                    let decoded = decode_entities(&self.source[start..self.at], start)?;
                    parent.text.push_str(&decoded);
                } else if let Some((relative, _)) = self.source[start..self.at]
                    .char_indices()
                    .find(|(_, ch)| !matches!(ch, '\t' | '\n' | '\r' | ' '))
                {
                    return Err(error(
                        DigReadErrorKind::Xml,
                        start + relative,
                        "document",
                        "text outside root element must be literal XML whitespace",
                    ));
                }
            } else if self.starts("<!--") {
                self.skip_comment()?;
            } else if self.starts("<?xml") && self.stack.is_empty() && self.root.is_none() {
                if self.at != 0 && !(consumed_bom && self.at == 3)
                    || self
                        .bytes
                        .get(self.at + 5)
                        .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'?')
                {
                    return Err(error(
                        DigReadErrorKind::Xml,
                        self.at,
                        "XML declaration",
                        "declaration must occur at the start of the document",
                    ));
                }
                self.skip_declaration()?;
            } else if self.starts("</") {
                self.close_tag()?;
            } else if self.starts("<!") {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    self.at,
                    "XML",
                    "DTD, entity declarations and CDATA are forbidden",
                ));
            } else if self.starts("<?") {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    self.at,
                    "XML",
                    "processing instructions are unsupported",
                ));
            } else {
                self.open_tag()?;
            }
        }
        if !self.stack.is_empty() {
            return Err(error(
                DigReadErrorKind::Xml,
                self.at,
                self.stack
                    .last()
                    .map(|n| n.name.clone())
                    .unwrap_or_default(),
                "unclosed element",
            ));
        }
        self.root
            .ok_or_else(|| error(DigReadErrorKind::Xml, 0, "document", "missing root element"))
    }

    fn starts(&self, text: &str) -> bool {
        self.bytes[self.at..].starts_with(text.as_bytes())
    }

    fn skip_space(&mut self) {
        while self.at < self.bytes.len() && is_xml_space_byte(self.bytes[self.at]) {
            self.at += 1;
        }
    }

    fn read_name(&mut self) -> Result<String, DigReadError> {
        let start = self.at;
        while self.at < self.bytes.len()
            && (self.bytes[self.at].is_ascii_alphanumeric()
                || matches!(self.bytes[self.at], b'_' | b'-' | b':' | b'.'))
        {
            self.at += 1;
        }
        if self.at == start || !self.bytes[start].is_ascii_alphabetic() && self.bytes[start] != b'_'
        {
            return Err(error(
                DigReadErrorKind::Xml,
                start,
                "XML name",
                "invalid or missing element/attribute name",
            ));
        }
        Ok(self.source[start..self.at].to_owned())
    }

    fn open_tag(&mut self) -> Result<(), DigReadError> {
        let offset = self.at;
        self.at += 1;
        let name = self.read_name()?;
        let mut attributes = Vec::new();
        loop {
            self.skip_space();
            if self.at >= self.bytes.len() {
                return Err(error(
                    DigReadErrorKind::Xml,
                    self.at,
                    &name,
                    "unterminated start tag",
                ));
            }
            if self.starts(">") {
                self.at += 1;
                break;
            }
            if self.starts("/>") {
                self.at += 2;
                let node = Node {
                    name,
                    attributes,
                    children: Vec::new(),
                    text: String::new(),
                    offset,
                };
                self.push_node(node)?;
                return Ok(());
            }
            let attr_offset = self.at;
            let key = self.read_name()?;
            if attributes.len() >= MAX_ATTRIBUTES_PER_ELEMENT {
                return Err(error(
                    DigReadErrorKind::Limit,
                    attr_offset,
                    &name,
                    "XML attribute count exceeds 64",
                ));
            }
            if attributes.iter().any(|(existing, _)| existing == &key) {
                return Err(error(
                    DigReadErrorKind::DuplicateField,
                    attr_offset,
                    key,
                    "XML attribute is repeated",
                ));
            }
            self.skip_space();
            if !self.starts("=") {
                return Err(error(
                    DigReadErrorKind::Xml,
                    self.at,
                    key,
                    "expected '=' after attribute name",
                ));
            }
            self.at += 1;
            self.skip_space();
            if self.at >= self.bytes.len() || !matches!(self.bytes[self.at], b'\'' | b'"') {
                return Err(error(
                    DigReadErrorKind::Xml,
                    self.at,
                    key,
                    "expected quoted attribute value",
                ));
            }
            let quote = self.bytes[self.at];
            self.at += 1;
            let start = self.at;
            while self.at < self.bytes.len() && self.bytes[self.at] != quote {
                self.at += 1;
            }
            if self.at == self.bytes.len() {
                return Err(error(
                    DigReadErrorKind::Xml,
                    start,
                    key,
                    "unterminated attribute value",
                ));
            }
            if self.source[start..self.at].contains('<') {
                return Err(error(
                    DigReadErrorKind::Xml,
                    start,
                    &key,
                    "'<' is not allowed in an attribute value",
                ));
            }
            let value = decode_entities(&self.source[start..self.at], start)?;
            self.at += 1;
            if self.at < self.bytes.len()
                && !is_xml_space_byte(self.bytes[self.at])
                && !self.starts(">")
                && !self.starts("/>")
            {
                return Err(error(
                    DigReadErrorKind::Xml,
                    self.at,
                    &name,
                    "whitespace is required between XML attributes",
                ));
            }
            attributes.push((key, value));
        }
        if self.stack.len() + 1 > MAX_DEPTH {
            return Err(error(
                DigReadErrorKind::Limit,
                offset,
                name,
                "XML nesting exceeds 32",
            ));
        }
        self.stack.push(Node {
            name,
            attributes,
            children: Vec::new(),
            text: String::new(),
            offset,
        });
        Ok(())
    }

    fn push_node(&mut self, node: Node) -> Result<(), DigReadError> {
        if self.stack.len() + 1 > MAX_DEPTH {
            return Err(error(
                DigReadErrorKind::Limit,
                node.offset,
                node.name,
                "XML nesting exceeds 32",
            ));
        }
        if let Some(parent) = self.stack.last_mut() {
            parent.children.push(node);
        } else if self.root.replace(node).is_some() {
            return Err(error(
                DigReadErrorKind::Xml,
                self.at,
                "document",
                "multiple root elements",
            ));
        }
        Ok(())
    }

    fn close_tag(&mut self) -> Result<(), DigReadError> {
        let offset = self.at;
        self.at += 2;
        let name = self.read_name()?;
        self.skip_space();
        if !self.starts(">") {
            return Err(error(DigReadErrorKind::Xml, self.at, name, "expected '>'"));
        }
        self.at += 1;
        let node = self.stack.pop().ok_or_else(|| {
            error(
                DigReadErrorKind::Xml,
                offset,
                name.clone(),
                "unexpected closing tag",
            )
        })?;
        if node.name != name {
            return Err(error(
                DigReadErrorKind::Xml,
                offset,
                name,
                format!("mismatched closing tag for <{}>", node.name),
            ));
        }
        self.push_node(node)
    }

    fn skip_comment(&mut self) -> Result<(), DigReadError> {
        let start = self.at;
        self.at += 4;
        if let Some(end) = self.source[self.at..].find("-->") {
            if self.source[self.at..self.at + end].contains("--") {
                return Err(error(
                    DigReadErrorKind::Xml,
                    start,
                    "comment",
                    "'--' is forbidden inside XML comments",
                ));
            }
            self.at += end + 3;
            Ok(())
        } else {
            Err(error(
                DigReadErrorKind::Xml,
                start,
                "comment",
                "unterminated XML comment",
            ))
        }
    }

    fn skip_declaration(&mut self) -> Result<(), DigReadError> {
        let start = self.at;
        self.at += 5;
        if let Some(end) = self.source[self.at..].find("?>") {
            let declaration = &self.source[start..self.at + end + 2];
            validate_xml_declaration(declaration, start)?;
            self.at += end + 2;
            Ok(())
        } else {
            Err(error(
                DigReadErrorKind::Xml,
                start,
                "XML declaration",
                "unterminated declaration",
            ))
        }
    }
}

fn is_xml_space_byte(byte: u8) -> bool {
    matches!(byte, b' ' | 0x09 | 0x0d | 0x0a)
}

fn validate_xml_declaration(declaration: &str, offset: usize) -> Result<(), DigReadError> {
    let malformed =
        |message: &str| error(DigReadErrorKind::Xml, offset, "XML declaration", message);
    let unsupported = |message: &str| {
        error(
            DigReadErrorKind::Unsupported,
            offset,
            "XML declaration",
            message,
        )
    };
    let inner = declaration
        .strip_prefix("<?xml")
        .and_then(|s| s.strip_suffix("?>"))
        .ok_or_else(|| malformed("malformed XML declaration"))?;
    let bytes = inner.as_bytes();
    let mut at = 0;
    let skip_space = |at: &mut usize| {
        while *at < bytes.len() && is_xml_space_byte(bytes[*at]) {
            *at += 1;
        }
    };
    if bytes.first().is_none_or(|b| !is_xml_space_byte(*b)) {
        return Err(malformed(
            "whitespace and version information are required after '<?xml'",
        ));
    }
    let mut seen_version = false;
    let mut seen_encoding = false;
    let mut seen_standalone = false;
    let mut last_rank = 0;
    while at < bytes.len() {
        skip_space(&mut at);
        if at == bytes.len() {
            break;
        }
        let name_start = at;
        while at < bytes.len() && bytes[at].is_ascii_alphabetic() {
            at += 1;
        }
        if at == name_start {
            return Err(malformed("invalid XML declaration field name"));
        }
        let name = &inner[name_start..at];
        while at < bytes.len() && is_xml_space_byte(bytes[at]) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            return Err(malformed("expected '=' in XML declaration field"));
        }
        at += 1;
        while at < bytes.len() && is_xml_space_byte(bytes[at]) {
            at += 1;
        }
        let quote = *bytes
            .get(at)
            .filter(|b| matches!(b, b'\'' | b'\"'))
            .ok_or_else(|| malformed("XML declaration values must be quoted"))?;
        at += 1;
        let value_start = at;
        while at < bytes.len() && bytes[at] != quote {
            at += 1;
        }
        if at == bytes.len() {
            return Err(malformed("unterminated XML declaration value"));
        }
        let value = &inner[value_start..at];
        at += 1;
        let rank = match name {
            "version" => {
                if seen_version || last_rank != 0 {
                    return Err(malformed("version must occur exactly once and first"));
                }
                seen_version = true;
                1
            }
            "encoding" => {
                if seen_encoding || !seen_version || last_rank > 1 {
                    return Err(malformed("encoding is duplicated or out of order"));
                }
                seen_encoding = true;
                2
            }
            "standalone" => {
                if seen_standalone || !seen_version || last_rank > 2 {
                    return Err(malformed("standalone is duplicated or out of order"));
                }
                seen_standalone = true;
                3
            }
            _ => {
                return Err(unsupported(&format!(
                    "unsupported XML declaration field {name:?}"
                )));
            }
        };
        last_rank = rank;
        match name {
            "version" if value != "1.0" => {
                return Err(unsupported(&format!(
                    "XML version {value:?} is unsupported; only 1.0 is supported"
                )));
            }
            "encoding" if !value.eq_ignore_ascii_case("UTF-8") => {
                return Err(unsupported("only UTF-8 encoding is supported"));
            }
            "standalone" if !matches!(value, "yes" | "no") => {
                return Err(malformed("standalone must be 'yes' or 'no'"));
            }
            _ => {}
        }
        if at < bytes.len() && !is_xml_space_byte(bytes[at]) {
            return Err(malformed(
                "whitespace is required between XML declaration fields",
            ));
        }
    }
    if !seen_version {
        return Err(malformed("required version field is absent"));
    }
    Ok(())
}

fn normalize_xml_newlines(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            output.push('\n');
        } else {
            output.push(ch);
        }
    }
    output
}

fn decode_entities(value: &str, offset: usize) -> Result<String, DigReadError> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    let mut consumed = 0;
    while let Some(index) = rest.find('&') {
        output.push_str(&normalize_xml_newlines(&rest[..index]));
        let tail = &rest[index + 1..];
        let end = tail.find(';').ok_or_else(|| {
            error(
                DigReadErrorKind::Xml,
                offset + consumed + index,
                "entity",
                "unterminated XML entity",
            )
        })?;
        let entity = &tail[..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            numeric if numeric.starts_with("#x") => u32::from_str_radix(&numeric[2..], 16)
                .ok()
                .and_then(char::from_u32),
            numeric if numeric.starts_with('#') => {
                numeric[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        }
        .ok_or_else(|| {
            error(
                DigReadErrorKind::Unsupported,
                offset + consumed + index,
                "entity",
                "only XML predefined and numeric entities are supported",
            )
        })?;
        if matches!(ch as u32, 0..=8 | 11..=12 | 14..=31 | 0xfffe..=0xffff) {
            return Err(error(
                DigReadErrorKind::Xml,
                offset + consumed + index,
                "entity",
                "invalid XML character reference",
            ));
        }
        output.push(ch);
        let skip = index + end + 2;
        consumed += skip;
        rest = &tail[end + 1..];
    }
    output.push_str(&normalize_xml_newlines(rest));
    Ok(output)
}

/// Resource cap for geometry topology/intersection validation (bounded O(n²)).
#[cfg(test)]
const GEOMETRY_WORK_LIMIT: usize = 80_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainCodecError {
    Invalid(String),
}
impl std::fmt::Display for DomainCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for DomainCodecError {}
fn invalid(message: impl Into<String>) -> DomainCodecError {
    DomainCodecError::Invalid(message.into())
}
type Coord = (i32, i32);

fn kind_from_name(name: &str) -> Option<ComponentKind> {
    crate::components::from_digital_name(name)
}
fn name_from_kind(kind: ComponentKind) -> &'static str {
    kind.digital_name()
}
// Domain editor positions denote the artwork bounds center. Digital stores its logical origin;
// share the renderer's toolkit-neutral placement metrics to keep pins aligned with the artwork.
fn center_offset(kind: ComponentKind) -> Result<(i32, i32), DomainCodecError> {
    let offset = crate::symbols::center_from_origin_offset(crate::symbols::DEFAULT_STYLE, kind)
        .ok_or_else(|| invalid("component kind has no default symbol"))?;
    Ok((
        exact_i32(offset[0], "symbol center x offset")?,
        exact_i32(offset[1], "symbol center y offset")?,
    ))
}
fn pin_offsets(kind: ComponentKind, pin: PinId) -> Option<Coord> {
    match (kind, pin.direction, pin.index) {
        (ComponentKind::Const, PinDirection::Output, 0) => Some((0, 0)),
        (ComponentKind::And | ComponentKind::Or | ComponentKind::XOr, PinDirection::Input, 0) => {
            Some((0, 0))
        }
        (ComponentKind::And | ComponentKind::Or | ComponentKind::XOr, PinDirection::Input, 1) => {
            Some((0, 40))
        }
        (ComponentKind::And | ComponentKind::Or | ComponentKind::XOr, PinDirection::Output, 0) => {
            Some((60, 20))
        }
        (
            ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XNOr,
            PinDirection::Input,
            0,
        ) => Some((0, 0)),
        (
            ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XNOr,
            PinDirection::Input,
            1,
        ) => Some((0, 40)),
        (
            ComponentKind::NAnd | ComponentKind::NOr | ComponentKind::XNOr,
            PinDirection::Output,
            0,
        ) => Some((80, 20)),
        (ComponentKind::Not, PinDirection::Input, 0) => Some((0, 0)),
        (ComponentKind::Not, PinDirection::Output, 0) => Some((40, 0)),
        (ComponentKind::Input, PinDirection::Output, 0)
        | (ComponentKind::Output, PinDirection::Input, 0) => Some((0, 0)),
        _ => None,
    }
}
fn exact_i32(value: f32, what: &str) -> Result<i32, DomainCodecError> {
    if !value.is_finite()
        || value.fract() != 0.0
        || (value as f64) < i32::MIN as f64
        || (value as f64) > i32::MAX as f64
    {
        return Err(invalid(format!("{what} is not an exact i32 coordinate")));
    }
    let integer = value as i32;
    if integer as f32 != value {
        return Err(invalid(format!("{what} loses f32 precision")));
    }
    Ok(integer)
}
fn pin_coord(component: &Component, pin: PinId) -> Result<Coord, DomainCodecError> {
    let x = exact_i32(component.position.x, "component x")?;
    let y = exact_i32(component.position.y, "component y")?;
    let offset = center_offset(component.kind)?;
    let pin = pin_offsets(component.kind, pin).ok_or_else(|| invalid("invalid pin reference"))?;
    Ok((
        x.checked_sub(offset.0)
            .and_then(|n| n.checked_add(pin.0))
            .ok_or_else(|| invalid("pin x overflow"))?,
        y.checked_sub(offset.1)
            .and_then(|n| n.checked_add(pin.1))
            .ok_or_else(|| invalid("pin y overflow"))?,
    ))
}
fn domain_pin_coord(component: &Component, pin: PinId) -> Result<Coord, DomainCodecError> {
    pin_coord(component, pin)
}

/// Convert a supported `.dig` v2 document to editable domain data. No simulator compilation is performed.
pub fn to_domain(dig: &DigV2) -> Result<Document, DomainCodecError> {
    if dig.elements.len() > MAX_ELEMENTS || dig.wires.len() > MAX_WIRES {
        return Err(invalid("document exceeds element/wire limit"));
    }
    let mut components = Vec::with_capacity(dig.elements.len());
    for (index, element) in dig.elements.iter().enumerate() {
        let kind = kind_from_name(&element.name)
            .ok_or_else(|| invalid(format!("unsupported component {:?}", element.name)))?;
        let (dx, dy) = center_offset(kind)?;
        let x = element
            .x
            .checked_add(dx)
            .ok_or_else(|| invalid("component center x overflow"))?;
        let y = element
            .y
            .checked_add(dy)
            .ok_or_else(|| invalid("component center y overflow"))?;
        let position = Position {
            x: x as f32,
            y: y as f32,
        };
        // Validate the exact integer center, not a floating-point inverse subtraction.
        // At large magnitudes `center as f32 - offset as f32` can round back to a
        // different origin even though the integer `origin + offset` is the exact,
        // representable center that export will produce. Export/import must share this
        // integer placement contract so every accepted export can be reopened.
        if exact_i32(position.x, "component center x")? != x
            || exact_i32(position.y, "component center y")? != y
        {
            return Err(invalid("component origin/center loses f32 precision"));
        }
        if kind != ComponentKind::Const && element.value.is_some() {
            return Err(invalid("Value is supported only for Const"));
        }
        components.push(Component {
            id: ComponentId(index as u64),
            kind,
            position,
            label: element.label.clone().unwrap_or_default(),
            constant_value: element.value.unwrap_or(true),
        });
    }
    // Validate every supported pin's exact integer placement without requiring valid or
    // compilable connectivity. Coincident pins and arbitrary segment topology remain preserved.
    all_pins(&Document {
        components: components.clone(),
        wires: crate::domain::Wiring::GeometricSegments(Vec::new()),
    })?;
    let mut segment_ids = BTreeSet::new();
    let mut segments = Vec::with_capacity(dig.wires.len());
    for (index, wire) in dig.wires.iter().enumerate() {
        if wire.p1 == wire.p2 {
            return Err(invalid(format!("wires[{index}] is a zero-length segment")));
        }
        let id = u64::try_from(index).map_err(|_| invalid("segment ID overflow"))?;
        if !segment_ids.insert(id) {
            return Err(invalid(format!("duplicate segment ID {id}")));
        }
        segments.push(crate::geometric_wiring::Segment {
            id,
            a: crate::geometric_wiring::Point::new(wire.p1.x, wire.p1.y),
            b: crate::geometric_wiring::Point::new(wire.p2.x, wire.p2.y),
        });
    }
    Ok(Document {
        components,
        wires: crate::domain::Wiring::GeometricSegments(segments),
    })
}
fn all_pins(doc: &Document) -> Result<Vec<(PinRef, Coord)>, DomainCodecError> {
    let mut ids = BTreeSet::new();
    let mut pins = Vec::new();
    for c in &doc.components {
        if !ids.insert(c.id) {
            return Err(invalid("duplicate component ID"));
        }
        if !c.position.valid() {
            return Err(invalid("non-finite component position"));
        }
        for direction in [PinDirection::Input, PinDirection::Output] {
            for index in 0..3 {
                let pin = PinId { direction, index };
                if c.kind.has_pin(pin) {
                    pins.push((
                        PinRef {
                            component: c.id,
                            pin,
                        },
                        domain_pin_coord(c, pin)?,
                    ));
                }
            }
        }
    }
    Ok(pins)
}
#[cfg(test)]
fn geometry_groups(
    wires: &[DigWire],
    pin_at: &BTreeMap<Coord, PinRef>,
) -> Result<Vec<Vec<PinRef>>, DomainCodecError> {
    if wires.len() > MAX_WIRES {
        return Err(invalid("wire count exceeds limit"));
    }
    let work = wires
        .len()
        .checked_mul(wires.len().saturating_sub(1))
        .and_then(|n| n.checked_div(2))
        .and_then(|n| n.checked_add(wires.len().checked_mul(pin_at.len())?))
        .ok_or_else(|| invalid("geometry work overflow"))?;
    if work > GEOMETRY_WORK_LIMIT {
        return Err(invalid("wire geometry validation work budget exceeded"));
    }
    let mut adj: BTreeMap<Coord, Vec<Coord>> = BTreeMap::new();
    for w in wires {
        let a = (w.p1.x, w.p1.y);
        let b = (w.p2.x, w.p2.y);
        if a == b {
            return Err(invalid("zero-length wire"));
        }
        if a.0 != b.0 && a.1 != b.1 {
            return Err(invalid("diagonal wire"));
        }
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    // Reject collinear overlap and endpoint-on-segment normalization cases; proper crossings stay disconnected.
    for i in 0..wires.len() {
        for j in i + 1..wires.len() {
            let a = &wires[i];
            let b = &wires[j];
            let (a1, a2) = ((a.p1.x, a.p1.y), (a.p2.x, a.p2.y));
            let (b1, b2) = ((b.p1.x, b.p1.y), (b.p2.x, b.p2.y));
            let ah = a1.1 == a2.1;
            let bh = b1.1 == b2.1;
            if ah == bh {
                if ah && a1.1 == b1.1 && ranges_overlap(a1.0, a2.0, b1.0, b2.0)
                    || !ah && a1.0 == b1.0 && ranges_overlap(a1.1, a2.1, b1.1, b2.1)
                {
                    // Any positive-length collinear intersection is overlapping geometry,
                    // even if the segments also share an endpoint. Adjacent segments meet
                    // at one point and do not satisfy ranges_overlap.
                    return Err(invalid("overlapping or duplicate wire segments"));
                }
            } else {
                let (h1, h2, v1, v2) = if ah {
                    (a1, a2, b1, b2)
                } else {
                    (b1, b2, a1, a2)
                };
                let cross = (v1.0, h1.1);
                if between(cross.0, h1.0, h2.0) && between(cross.1, v1.1, v2.1) {
                    let on_h_endpoint = cross == h1 || cross == h2;
                    let on_v_endpoint = cross == v1 || cross == v2;
                    if on_h_endpoint != on_v_endpoint {
                        return Err(invalid(
                            "wire endpoint or pin on segment interior requires normalization",
                        ));
                    }
                    // Interior crossings deliberately do not connect.
                }
            }
        }
    }
    // Every geometric junction is explicit at a shared endpoint. Non-pin bends must have degree 2.
    for (point, neighbors) in &adj {
        if !pin_at.contains_key(point) && neighbors.len() != 2 {
            return Err(invalid(
                "dangling endpoint or non-pin branch in wire geometry",
            ));
        }
    }
    for point in pin_at.keys() {
        if wires.iter().any(|w| point_on_interior(*point, w)) {
            return Err(invalid("pin on segment interior requires normalization"));
        }
    }
    // Reject loops; canonical-route validation below bounds accepted path shapes.
    let mut graph_seen = BTreeSet::new();
    for start in adj.keys().copied() {
        if graph_seen.contains(&start) {
            continue;
        }
        let mut stack = vec![(start, None)];
        graph_seen.insert(start);
        while let Some((point, parent)) = stack.pop() {
            for next in &adj[&point] {
                if Some(*next) == parent {
                    continue;
                }
                if !graph_seen.insert(*next) {
                    return Err(invalid("wire loops are unsupported"));
                }
                stack.push((*next, Some(point)));
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for start in adj.keys().copied() {
        if seen.contains(&start) {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        let mut pins = Vec::new();
        seen.insert(start);
        while let Some(p) = queue.pop_front() {
            if let Some(pin) = pin_at.get(&p) {
                pins.push(*pin)
            }
            for next in &adj[&p] {
                if seen.insert(*next) {
                    queue.push_back(*next)
                }
            }
        }
        pins.sort();
        groups.push(pins);
    }
    groups.sort();
    Ok(groups)
}
#[cfg(test)]
fn ranges_overlap(a: i32, b: i32, c: i32, d: i32) -> bool {
    a.min(b) < c.max(d) && c.min(d) < a.max(b)
}
#[cfg(test)]
fn between(v: i32, a: i32, b: i32) -> bool {
    v >= a.min(b) && v <= a.max(b)
}
#[cfg(test)]
fn point_on_interior(p: Coord, w: &DigWire) -> bool {
    if w.p1.x == w.p2.x {
        p.0 == w.p1.x && between(p.1, w.p1.y, w.p2.y) && p.1 != w.p1.y && p.1 != w.p2.y
    } else {
        p.1 == w.p1.y && between(p.0, w.p1.x, w.p2.x) && p.0 != w.p1.x && p.0 != w.p2.x
    }
}

/// Convert editable data to the supported Digital v2 representation. Geometric segments are
/// serialized verbatim; topology/compiler validation is deliberately not a codec prerequisite.
pub fn from_domain(doc: &Document) -> Result<DigV2, DomainCodecError> {
    if doc.components.len() > MAX_ELEMENTS || doc.wires.len() > MAX_WIRES {
        return Err(invalid("document exceeds element/wire limit"));
    }
    let mut ids = BTreeSet::new();
    let mut elements = Vec::with_capacity(doc.components.len());
    for c in &doc.components {
        if c.kind != ComponentKind::Const && !c.constant_value {
            return Err(invalid(format!(
                "component {} has a non-default constant value but is not Const",
                c.id.0
            )));
        }
        if !ids.insert(c.id) {
            return Err(invalid("duplicate component ID"));
        }
        let x = exact_i32(c.position.x, "component center x")?;
        let y = exact_i32(c.position.y, "component center y")?;
        let (dx, dy) = center_offset(c.kind)?;
        let ox = x
            .checked_sub(dx)
            .ok_or_else(|| invalid("component origin x overflow"))?;
        let oy = y
            .checked_sub(dy)
            .ok_or_else(|| invalid("component origin y overflow"))?;
        elements.push(DigElement {
            name: name_from_kind(c.kind).into(),
            x: ox,
            y: oy,
            label: if c.label.is_empty() {
                None
            } else {
                Some(c.label.clone())
            },
            value: if c.kind == ComponentKind::Const {
                Some(c.constant_value)
            } else {
                None
            },
        });
    }
    all_pins(doc)?;
    let segments = doc.wires.geometric().unwrap();
    if segments.len() > MAX_WIRES {
        return Err(invalid("segment count exceeds 30,000"));
    }
    let mut ids = BTreeSet::new();
    let mut wires = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        if !ids.insert(segment.id) {
            return Err(invalid(format!(
                "duplicate segment ID {} at index {index}",
                segment.id
            )));
        }
        if segment.a == segment.b {
            return Err(invalid(format!("zero-length segment at index {index}")));
        }
        wires.push(DigWire {
            p1: DigPoint {
                x: segment.a.x,
                y: segment.a.y,
            },
            p2: DigPoint {
                x: segment.b.x,
                y: segment.b.y,
            },
        });
    }
    let dig = DigV2 { elements, wires };
    serialize_v2(&dig).map_err(|e| invalid(e.to_string()))?;
    Ok(dig)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str =
        "<circuit><version>2</version><attributes/><visualElements/><wires/></circuit>";

    fn doc(elements: &str, wires: &str) -> String {
        format!(
            "<circuit><version>2</version><attributes/><visualElements>{elements}</visualElements><wires>{wires}</wires></circuit>"
        )
    }

    fn expect_error(xml: &str, kind: DigReadErrorKind, context: &str) -> DigReadError {
        let err = parse_v2(xml.as_bytes()).expect_err(xml);
        assert_eq!(err.kind, kind, "{err:?}");
        assert!(err.context.contains(context), "{err:?}");
        err
    }

    #[test]
    fn domain_conversion_supports_all_kinds_offsets_negative_positions_and_unicode_labels() {
        let source = DigV2 {
            elements: vec![
                DigElement {
                    name: "In".into(),
                    x: -31,
                    y: -12,
                    label: Some("λ input".into()),
                    value: None,
                },
                DigElement {
                    name: "Out".into(),
                    x: 18,
                    y: -2,
                    label: Some(String::new()),
                    value: None,
                },
                DigElement {
                    name: "And".into(),
                    x: -100,
                    y: 40,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "Or".into(),
                    x: 3,
                    y: 7,
                    label: Some("OR".into()),
                    value: None,
                },
                DigElement {
                    name: "NAnd".into(),
                    x: -220,
                    y: -40,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "NOr".into(),
                    x: -320,
                    y: -80,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "XOr".into(),
                    x: -420,
                    y: -120,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "XNOr".into(),
                    x: -520,
                    y: -160,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "Not".into(),
                    x: -20,
                    y: -4,
                    label: None,
                    value: None,
                },
            ],
            wires: vec![],
        };
        let domain = to_domain(&source).unwrap();
        assert_eq!(
            domain.components.iter().map(|c| c.id.0).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            domain
                .components
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>(),
            vec!["λ input", "", "", "OR", "", "", "", "", ""]
        );
        assert_eq!(
            from_domain(&domain).unwrap(),
            DigV2 {
                elements: source
                    .elements
                    .iter()
                    .cloned()
                    .map(|mut e| {
                        if e.label.as_deref() == Some("") {
                            e.label = None
                        }
                        e
                    })
                    .collect(),
                wires: vec![]
            }
        );
    }
    #[test]
    fn codec_pin_placement_matches_render_anchors_for_every_component_kind() {
        use crate::domain::*;
        use crate::{symbol_render::SymbolLayout, symbols::SymbolStyle};
        for kind in [
            ComponentKind::Input,
            ComponentKind::Output,
            ComponentKind::And,
            ComponentKind::NAnd,
            ComponentKind::Or,
            ComponentKind::NOr,
            ComponentKind::XOr,
            ComponentKind::XNOr,
            ComponentKind::Not,
        ] {
            let (dx, dy) = center_offset(kind).unwrap();
            let component = Component {
                id: ComponentId(0),
                kind,
                position: Position {
                    x: dx as f32,
                    y: dy as f32,
                },
                label: String::new(),
                constant_value: true,
            };
            let layout =
                SymbolLayout::centered_on(SymbolStyle::Rect, kind, [dx as f32, dy as f32]).unwrap();
            for (pin, coord) in all_pins(&Document {
                components: vec![component.clone()],
                wires: crate::domain::Wiring::GeometricSegments(vec![]),
            })
            .unwrap()
            {
                let anchor = layout.pin_position(pin.pin).unwrap();
                assert_eq!(
                    coord,
                    (anchor[0] as i32, anchor[1] as i32),
                    "{kind:?} {pin:?}"
                );
            }
            let encoded = from_domain(&Document {
                components: vec![component],
                wires: crate::domain::Wiring::GeometricSegments(vec![]),
            })
            .unwrap();
            assert_eq!(
                (encoded.elements[0].x, encoded.elements[0].y),
                (0, 0),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn geometry_roundtrips_simple_segments_and_rejects_bad_geometry() {
        use crate::domain::*;
        let input = Component {
            id: ComponentId(1),
            kind: ComponentKind::Input,
            position: Position { x: 0., y: 0. },
            label: String::new(),
            constant_value: true,
        };
        let output = Component {
            id: ComponentId(2),
            kind: ComponentKind::Output,
            position: Position { x: 100., y: 0. },
            label: String::new(),
            constant_value: true,
        };
        let components = vec![input.clone(), output.clone()];
        let doc = Document {
            wires: crate::test_support::routed_wires(
                &components,
                &[(
                    PinRef {
                        component: input.id,
                        pin: PinId {
                            direction: PinDirection::Output,
                            index: 0,
                        },
                    },
                    PinRef {
                        component: output.id,
                        pin: PinId {
                            direction: PinDirection::Input,
                            index: 0,
                        },
                    },
                )],
            ),
            components,
        };
        let xml = serialize_v2(&from_domain(&doc).unwrap()).unwrap();
        let reopened = to_domain(&parse_v2(&xml).unwrap()).unwrap();
        assert_eq!(
            reopened
                .components
                .iter()
                .map(|c| (c.kind, c.position, c.label.as_str()))
                .collect::<Vec<_>>(),
            doc.components
                .iter()
                .map(|c| (c.kind, c.position, c.label.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(reopened.wires.len(), 1);
        for geometry in [
            vec![DigWire {
                p1: DigPoint { x: 0, y: 0 },
                p2: DigPoint { x: 0, y: 0 },
            }],
            vec![DigWire {
                p1: DigPoint { x: 0, y: 0 },
                p2: DigPoint { x: 1, y: 1 },
            }],
            vec![DigWire {
                p1: DigPoint { x: 0, y: 0 },
                p2: DigPoint { x: 100, y: 0 },
            }],
        ] {
            assert!(
                geometry_groups(
                    &geometry,
                    &BTreeMap::from([(
                        (0, 0),
                        PinRef {
                            component: ComponentId(0),
                            pin: PinId {
                                direction: PinDirection::Output,
                                index: 0
                            }
                        }
                    )])
                )
                .is_err()
            );
        }
    }
    #[test]
    fn import_preserves_pin_to_pin_chain_and_exports_same_geometry() {
        let source = DigV2 {
            elements: vec![
                DigElement {
                    name: "In".into(),
                    x: 0,
                    y: 0,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "Out".into(),
                    x: 50,
                    y: 0,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "Out".into(),
                    x: 100,
                    y: 0,
                    label: None,
                    value: None,
                },
            ],
            wires: vec![
                DigWire {
                    p1: DigPoint { x: 0, y: 0 },
                    p2: DigPoint { x: 50, y: 0 },
                },
                DigWire {
                    p1: DigPoint { x: 50, y: 0 },
                    p2: DigPoint { x: 100, y: 0 },
                },
            ],
        };
        let imported = to_domain(&source).unwrap();
        assert_eq!(imported.wires.len(), 2);
        let exported = from_domain(&imported).unwrap();
        let segments = |wires: &[DigWire]| {
            wires
                .iter()
                .map(|w| {
                    let a = (w.p1.x, w.p1.y);
                    let b = (w.p2.x, w.p2.y);
                    if a <= b { (a, b) } else { (b, a) }
                })
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(segments(&exported.wires), segments(&source.wires));
        assert_eq!(
            segments(&from_domain(&to_domain(&exported).unwrap()).unwrap().wires),
            segments(&source.wires)
        );
    }

    #[test]
    fn import_preserves_noncanonical_route_without_rerouting() {
        let source = DigV2 {
            elements: vec![
                DigElement {
                    name: "In".into(),
                    x: 0,
                    y: 0,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "Out".into(),
                    x: 100,
                    y: 100,
                    label: None,
                    value: None,
                },
            ],
            wires: vec![
                DigWire {
                    p1: DigPoint { x: 0, y: 0 },
                    p2: DigPoint { x: 0, y: 100 },
                },
                DigWire {
                    p1: DigPoint { x: 0, y: 100 },
                    p2: DigPoint { x: 100, y: 100 },
                },
            ],
        };
        let imported = to_domain(&source).unwrap();
        assert_eq!(from_domain(&imported).unwrap().wires, source.wires);
        assert!(matches!(
            imported.wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
    }

    #[test]
    fn fanout_net_normalization_is_independent_of_xml_element_order() {
        let wires = vec![
            DigWire {
                p1: DigPoint { x: 100, y: 0 },
                p2: DigPoint { x: 0, y: 0 },
            },
            DigWire {
                p1: DigPoint { x: 0, y: 0 },
                p2: DigPoint { x: 0, y: 100 },
            },
        ];
        // XML order: output at (100,0), input at (0,0), output at (0,100).
        for elements in [
            vec![("Out", 100, 0), ("In", 0, 0), ("Out", 0, 100)],
            vec![("Out", 0, 100), ("Out", 100, 0), ("In", 0, 0)],
            vec![("In", 0, 0), ("Out", 0, 100), ("Out", 100, 0)],
        ] {
            let doc = DigV2 {
                elements: elements
                    .into_iter()
                    .map(|(name, x, y)| DigElement {
                        name: name.into(),
                        x,
                        y,
                        label: None,
                        value: None,
                    })
                    .collect(),
                wires: wires.clone(),
            };
            let domain = to_domain(&doc).unwrap();
            let exported = from_domain(&domain).unwrap();
            assert_eq!(exported.wires.len(), 2);
            assert_eq!(to_domain(&exported).unwrap().wires.len(), 2);
            // Each path remains the intended geometric pin-to-pin connection, not an
            // overlapping star edge from the distant pin.
            let canonical_segment = |w: &DigWire| {
                let (a, b) = ((w.p1.x, w.p1.y), (w.p2.x, w.p2.y));
                if a <= b { (a, b) } else { (b, a) }
            };
            let got = exported
                .wires
                .iter()
                .map(canonical_segment)
                .collect::<BTreeSet<_>>();
            let expected = wires.iter().map(canonical_segment).collect::<BTreeSet<_>>();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn rejects_positive_length_collinear_overlap_even_when_segments_share_endpoint() {
        let wire = |p1: (i32, i32), p2: (i32, i32)| DigWire {
            p1: DigPoint { x: p1.0, y: p1.1 },
            p2: DigPoint { x: p2.0, y: p2.1 },
        };
        let cases = [
            vec![
                wire((0, 0), (100, 0)),
                wire((50, 0), (50, 50)),
                wire((50, 0), (0, 0)),
            ],
            vec![wire((0, 0), (100, 0)), wire((100, 0), (50, 0))],
            vec![wire((100, 0), (0, 0)), wire((50, 0), (100, 0))],
        ];
        for wires in cases {
            assert!(
                geometry_groups(&wires, &BTreeMap::new()).is_err(),
                "{wires:?}"
            );
        }
    }

    #[test]
    fn fanout_and_disconnected_crossings_follow_endpoint_topology() {
        let pin = |id| PinRef {
            component: crate::domain::ComponentId(id),
            pin: crate::domain::PinId {
                direction: crate::domain::PinDirection::Output,
                index: 0,
            },
        };
        let fanout = vec![
            DigWire {
                p1: DigPoint { x: 0, y: 0 },
                p2: DigPoint { x: 10, y: 0 },
            },
            DigWire {
                p1: DigPoint { x: 10, y: 0 },
                p2: DigPoint { x: 10, y: 10 },
            },
            DigWire {
                p1: DigPoint { x: 10, y: 0 },
                p2: DigPoint { x: 20, y: 0 },
            },
        ];
        let fan_pins = BTreeMap::from([
            ((0, 0), pin(1)),
            ((10, 0), pin(2)),
            ((10, 10), pin(3)),
            ((20, 0), pin(4)),
        ]);
        assert_eq!(
            geometry_groups(&fanout, &fan_pins).unwrap(),
            vec![vec![pin(1), pin(2), pin(3), pin(4)]]
        );
        let crossing = vec![
            DigWire {
                p1: DigPoint { x: 0, y: 5 },
                p2: DigPoint { x: 10, y: 5 },
            },
            DigWire {
                p1: DigPoint { x: 5, y: 0 },
                p2: DigPoint { x: 5, y: 10 },
            },
        ];
        let crossing_pins = BTreeMap::from([
            ((0, 5), pin(1)),
            ((10, 5), pin(2)),
            ((5, 0), pin(3)),
            ((5, 10), pin(4)),
        ]);
        assert_eq!(geometry_groups(&crossing, &crossing_pins).unwrap().len(), 2);
        let reversed_bend = vec![
            DigWire {
                p1: DigPoint { x: 0, y: 10 },
                p2: DigPoint { x: 0, y: 0 },
            },
            DigWire {
                p1: DigPoint { x: 10, y: 10 },
                p2: DigPoint { x: 0, y: 10 },
            },
        ];
        let bend_pins = BTreeMap::from([((0, 0), pin(5)), ((10, 10), pin(6))]);
        assert_eq!(
            geometry_groups(&reversed_bend, &bend_pins).unwrap(),
            vec![vec![pin(5), pin(6)]]
        );
        let t = vec![
            DigWire {
                p1: DigPoint { x: 0, y: 0 },
                p2: DigPoint { x: 10, y: 0 },
            },
            DigWire {
                p1: DigPoint { x: 5, y: 0 },
                p2: DigPoint { x: 5, y: 5 },
            },
        ];
        assert!(geometry_groups(&t, &BTreeMap::new()).is_err());
        let over_budget = (0..401)
            .map(|i| DigWire {
                p1: DigPoint { x: i * 2, y: 0 },
                p2: DigPoint { x: i * 2 + 1, y: 0 },
            })
            .collect::<Vec<_>>();
        assert!(
            geometry_groups(&over_budget, &BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
    }
    #[test]
    fn geometric_segment_route_roundtrips_verbatim() {
        use crate::domain::*;
        use crate::geometric_wiring::{Point, Segment};
        let components = vec![
            Component {
                id: ComponentId(1),
                kind: ComponentKind::Input,
                position: Position { x: 0., y: 0. },
                label: String::new(),
                constant_value: true,
            },
            Component {
                id: ComponentId(2),
                kind: ComponentKind::Output,
                position: Position { x: 100., y: 80. },
                label: String::new(),
                constant_value: true,
            },
        ];
        let start = crate::pin_geometry::pin_point(
            &components[0],
            PinId {
                direction: PinDirection::Output,
                index: 0,
            },
        )
        .unwrap();
        let end = crate::pin_geometry::pin_point(
            &components[1],
            PinId {
                direction: PinDirection::Input,
                index: 0,
            },
        )
        .unwrap();
        let corner = Point::new(end.x, start.y);
        let doc = Document {
            components,
            wires: Wiring::GeometricSegments(vec![
                Segment {
                    id: 9,
                    a: start,
                    b: corner,
                },
                Segment {
                    id: 10,
                    a: corner,
                    b: end,
                },
            ]),
        };
        let dig = from_domain(&doc).unwrap();
        assert_eq!(dig.wires.len(), 2);
        let restored = to_domain(&parse_v2(&serialize_v2(&dig).unwrap()).unwrap()).unwrap();
        let export_again = from_domain(&restored).unwrap();
        assert_eq!(export_again.wires, dig.wires);
        assert_eq!(restored.wires.geometric().unwrap().len(), 2);
    }
    #[test]
    fn exact_coordinate_and_bad_reference_validation() {
        use crate::domain::*;
        let fractional = Document {
            components: vec![Component {
                id: ComponentId(1),
                kind: ComponentKind::Input,
                position: Position { x: 0.5, y: 0.0 },
                label: String::new(),
                constant_value: true,
            }],
            wires: crate::domain::Wiring::GeometricSegments(vec![]),
        };
        assert!(from_domain(&fractional).is_err());
        let f32_i32_overflow = Document {
            components: vec![Component {
                id: ComponentId(2),
                kind: ComponentKind::Input,
                position: Position {
                    x: i32::MAX as f32,
                    y: 0.0,
                },
                label: String::new(),
                constant_value: true,
            }],
            wires: crate::domain::Wiring::GeometricSegments(vec![]),
        };
        assert!(from_domain(&f32_i32_overflow).is_err());
    }

    #[test]
    fn accepts_empty_v2_and_empty_measurement_ordering() {
        assert_eq!(
            parse_v2(EMPTY.as_bytes()).unwrap(),
            DigV2 {
                elements: vec![],
                wires: vec![]
            }
        );
        let with_order = EMPTY.replace("</circuit>", "<measurementOrdering/></circuit>");
        assert!(parse_v2(with_order.as_bytes()).unwrap().elements.is_empty());
    }

    #[test]
    fn parses_ordered_elements_escaped_unicode_labels_and_signed_positions() {
        let xml = doc(
            concat!(
                "<visualElement><elementName>And</elementName><elementAttributes><entry><string>Label</string><string>R &amp; D &lt;λ&gt; &#x1F4A1;</string></entry></elementAttributes><pos x=\"-12\" y=\"+7\"/></visualElement>",
                "<visualElement><elementName>In</elementName><elementAttributes/><pos x=\"0\" y=\"-2147483648\"/></visualElement>"
            ),
            "",
        );
        let parsed = parse_v2(xml.as_bytes()).unwrap();
        assert_eq!(
            parsed.elements[0],
            DigElement {
                name: "And".into(),
                x: -12,
                y: 7,
                label: Some("R & D <λ> 💡".into()),
                value: None,
            }
        );
        assert_eq!(parsed.elements[1].name, "In");
        assert_eq!(parsed.elements[1].y, i32::MIN);
        let max_coordinate = doc(
            "<visualElement><elementName>Out</elementName><elementAttributes/><pos x=\"2147483647\" y=\"0\"/></visualElement>",
            "",
        );
        assert_eq!(
            parse_v2(max_coordinate.as_bytes()).unwrap().elements[0].x,
            i32::MAX
        );
    }

    #[test]
    fn preserves_wire_order_and_arbitrary_geometries() {
        let xml = doc(
            "",
            "<wire><p1 x=\"0\" y=\"1\"/><p2 x=\"2\" y=\"3\"/></wire><wire><p1 x=\"8\" y=\"9\"/><p2 x=\"-4\" y=\"-3\"/></wire><wire><p1 x=\"5\" y=\"5\"/><p2 x=\"5\" y=\"5\"/></wire>",
        );
        let parsed = parse_v2(xml.as_bytes()).unwrap();
        assert_eq!(parsed.wires[0].p2, DigPoint { x: 2, y: 3 });
        assert_eq!(parsed.wires[1].p1, DigPoint { x: 8, y: 9 });
        assert_eq!(parsed.wires[2].p1, parsed.wires[2].p2);
        assert_eq!(parse_v2(&serialize_v2(&parsed).unwrap()).unwrap(), parsed);
        assert!(
            to_domain(&parsed)
                .unwrap_err()
                .to_string()
                .contains("wires[2]")
        );
    }

    #[test]
    fn requires_version_two_and_required_fields() {
        expect_error(
            "<circuit><attributes/><visualElements/><wires/></circuit>",
            DigReadErrorKind::MissingField,
            "version",
        );
        for v in ["0", "1", "3", "99"] {
            let xml = EMPTY.replace("<version>2</version>", &format!("<version>{v}</version>"));
            expect_error(&xml, DigReadErrorKind::Unsupported, "version");
        }
        expect_error(
            "<circuit><version>2</version></circuit>",
            DigReadErrorKind::MissingField,
            "attributes",
        );
    }

    #[test]
    fn rejects_unsupported_fields_attributes_metadata_and_measurement_values() {
        let cases = [
            (EMPTY.replace("<attributes/>", "<attributes><entry/></attributes>"), "attributes"),
            (EMPTY.replace("<visualElements/>", "<visualElements><visualElement><rotate>0</rotate></visualElement></visualElements>"), "rotate"),
            (EMPTY.replace("<attributes/>", "<attributes foo=\"x\"/>"), "attributes"),
            (EMPTY.replace("</circuit>", "<testData/></circuit>"), "testData"),
            (EMPTY.replace("</circuit>", "<measurementOrdering><string>a</string></measurementOrdering></circuit>"), "measurementOrdering"),
            (EMPTY.replace("<attributes/>", "<attributes><metadata/></attributes>"), "attributes"),
        ];
        for (xml, context) in cases {
            expect_error(&xml, DigReadErrorKind::Unsupported, context);
        }
        let element = "<visualElement><elementName>A</elementName><elementAttributes><entry><string>Bits</string><int>2</int></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>";
        expect_error(
            &doc(element, ""),
            DigReadErrorKind::Unsupported,
            "elementAttributes",
        );
        for (key, value) in [("Bits", "1"), ("Inputs", "2"), ("wideShape", "false")] {
            let explicit_default = format!(
                "<visualElement><elementName>NAnd</elementName><elementAttributes><entry><string>{key}</string><string>{value}</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>"
            );
            expect_error(
                &doc(&explicit_default, ""),
                DigReadErrorKind::Unsupported,
                "elementAttributes",
            );
        }
        let wrong_case = "<visualElement><elementName>nand</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement>";
        assert!(to_domain(&parse_v2(doc(wrong_case, "").as_bytes()).unwrap()).is_err());
        let resource = "<visualElement><elementName>ROM</elementName><elementAttributes><entry><string>Data</string><string>memory.hex</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>";
        expect_error(
            &doc(resource, ""),
            DigReadErrorKind::Unsupported,
            "elementAttributes",
        );
    }

    #[test]
    fn rejects_malformed_xml_duplicates_missing_fields_extra_roots_and_text() {
        for input in [
            "",
            "<circuit>",
            "<circuit><version>2</version></circut>",
            "<circuit/><circuit/>",
        ] {
            expect_error(input, DigReadErrorKind::Xml, "");
        }
        expect_error("<wrong/>", DigReadErrorKind::Unsupported, "root");
        expect_error(
            &EMPTY.replace(
                "<version>2</version>",
                "<version>2</version><version>2</version>",
            ),
            DigReadErrorKind::DuplicateField,
            "version",
        );
        let no_pos = doc(
            "<visualElement><elementName>A</elementName><elementAttributes/></visualElement>",
            "",
        );
        expect_error(&no_pos, DigReadErrorKind::MissingField, "pos");
        expect_error(
            &EMPTY.replace("<attributes/>", "<attributes>junk</attributes>"),
            DigReadErrorKind::Unsupported,
            "attributes",
        );
        expect_error(
            "<circuit>junk<version>2</version></circuit>",
            DigReadErrorKind::Unsupported,
            "circuit",
        );
        expect_error(
            "<circuit>\u{a0}<version>2</version></circuit>",
            DigReadErrorKind::Unsupported,
            "circuit",
        );
        let duplicate_attr = doc(
            "<visualElement><elementName>A</elementName><elementAttributes/><pos x=\"0\" x=\"1\" y=\"0\"/></visualElement>",
            "",
        );
        expect_error(&duplicate_attr, DigReadErrorKind::DuplicateField, "x");
    }

    #[test]
    fn rejects_coordinate_overflow_fraction_and_non_numeric_values() {
        for value in ["2147483648", "-2147483649", "1.5", "nope", "", " 1"] {
            let xml = EMPTY.replace("<visualElements/>", &format!("<visualElements><visualElement><elementName>A</elementName><elementAttributes/><pos x=\"{value}\" y=\"0\"/></visualElement></visualElements>"));
            let err = parse_v2(xml.as_bytes()).unwrap_err();
            assert!(
                matches!(
                    err.kind,
                    DigReadErrorKind::InvalidValue | DigReadErrorKind::Xml
                ),
                "{err:?}"
            );
            assert!(err.context.contains("pos.x"), "{err:?}");
        }
    }

    #[test]
    fn requires_whitespace_between_attributes_and_bounds_attribute_count() {
        let missing_separator = EMPTY.replace("<circuit>", "<circuit x=\"0\"y=\"1\">");
        let error = parse_v2(missing_separator.as_bytes()).unwrap_err();
        assert_eq!(error.kind, DigReadErrorKind::Xml);
        assert!(error.message.contains("whitespace"), "{error:?}");

        let attributes = (0..65)
            .map(|index| format!(" a{index}=\"x\""))
            .collect::<String>();
        let over_budget = format!("<circuit{attributes}/> ");
        let error = parse_v2(over_budget.as_bytes()).unwrap_err();
        assert_eq!(error.kind, DigReadErrorKind::Limit);
        assert!(error.message.contains("attribute"), "{error:?}");
    }

    #[test]
    fn xml_declaration_must_follow_document_start_or_consumed_bom() {
        for whitespace in [" ", "  ", "   "] {
            let xml = format!("{whitespace}<?xml version=\"1.0\"?>{EMPTY}");
            let error = parse_v2(xml.as_bytes()).unwrap_err();
            assert_eq!(error.kind, DigReadErrorKind::Xml, "{whitespace:?}");
            assert!(error.message.contains("start of the document"), "{error:?}");
        }

        let bom_declaration = format!("\u{feff}<?xml version=\"1.0\"?>{EMPTY}");
        assert!(parse_v2(bom_declaration.as_bytes()).is_ok());
    }

    #[test]
    fn validates_xml_declaration_grammar_and_encoding() {
        let valid = [
            "<?xml version=\"1.0\"?>",
            "<?xml version = '1.0' encoding = \"UTF-8\"?>",
            "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>",
            "<?xml version=\"1.0\" standalone=\"no\"?>",
        ];
        for declaration in valid {
            assert!(
                parse_v2(format!("{declaration}{EMPTY}").as_bytes()).is_ok(),
                "{declaration}"
            );
        }
        for declaration in [
            "<?xml?>",
            "<?xml version9.9?>",
            "<?xml\u{b}version=\"1.0\"?>",
            "<?xml version=\"9.9\"?>",
            "<?xml version=\"1.0\" fake='encoding=\"utf-8\"'?>",
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?>",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" version=\"1.0\"?>",
            "<?xml version=\"1.0\" version=\"1.0\"?>",
            "<?xml version=1.0?>",
            "<?xml version=\"1.0\" standalone=\"maybe\"?>",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\" encoding=\"UTF-8\"?>",
        ] {
            assert!(
                parse_v2(format!("{declaration}{EMPTY}").as_bytes()).is_err(),
                "accepted {declaration}"
            );
        }
        let err =
            parse_v2(format!("<?xml version=\"1.0\" encoding=\"UTF-16\"?>{EMPTY}").as_bytes())
                .unwrap_err();
        assert_eq!(err.kind, DigReadErrorKind::Unsupported);
    }

    #[test]
    fn normalizes_literal_newlines_before_entity_decoding() {
        let label = "one\r\ntwo\rthree&#13;four";
        let xml = doc(
            &format!(
                "<visualElement><elementName>A</elementName><elementAttributes><entry><string>Label</string><string>{label}</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>"
            ),
            "",
        );
        assert_eq!(
            parse_v2(xml.as_bytes()).unwrap().elements[0]
                .label
                .as_deref(),
            Some("one\ntwo\nthree\rfour")
        );
    }

    #[test]
    fn entity_errors_report_original_input_offset_once() {
        for prefix in ["abc&amp;", "a&amp;b&amp;middle&amp;", &"x".repeat(900)] {
            let invalid = "&bogus;";
            let contents = format!("{prefix}{invalid}");
            let xml = format!("<circuit>{contents}</circuit>");
            let error = parse_v2(xml.as_bytes()).unwrap_err();
            assert_eq!(error.offset, xml.find(invalid).unwrap(), "{error:?}");
            assert!(error.offset < xml.len(), "{error:?}");
        }
    }

    #[test]
    fn rejects_dtd_external_entities_invalid_utf8_and_malformed_entities() {
        for input in [
            "<!DOCTYPE circuit><circuit/>",
            "<circuit><!-- bad--inside --></circuit>",
            "<circuit><!-- unterminated</circuit>",
            "<circuit>&#;</circuit>",
            "<circuit>&#x;</circuit>",
            "<circuit>&#0;</circuit>",
            "<circuit>&#xD800;</circuit>",
            "<circuit>&external;</circuit>",
            "<circuit>&amp</circuit>",
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?><circuit/>",
            "<circuit>\u{1}</circuit>",
        ] {
            let err = parse_v2(input.as_bytes()).unwrap_err();
            assert!(matches!(
                err.kind,
                DigReadErrorKind::Unsupported | DigReadErrorKind::Xml
            ));
        }
        assert_eq!(
            parse_v2(&[0xff]).unwrap_err().kind,
            DigReadErrorKind::InvalidUtf8
        );
    }

    #[test]
    fn input_depth_count_and_decoded_string_budgets_are_enforced() {
        assert_eq!(
            parse_v2(&vec![b' '; MAX_INPUT_BYTES + 1]).unwrap_err().kind,
            DigReadErrorKind::InputLimit
        );
        let at_depth = format!(
            "{}<z/>{}",
            "<a>".repeat(MAX_DEPTH - 1),
            "</a>".repeat(MAX_DEPTH - 1)
        );
        assert!(XmlParser::new(&at_depth).parse().is_ok());
        let over_depth = format!(
            "{}<z/>{}",
            "<a>".repeat(MAX_DEPTH),
            "</a>".repeat(MAX_DEPTH)
        );
        assert_eq!(
            XmlParser::new(&over_depth).parse().unwrap_err().kind,
            DigReadErrorKind::Limit
        );
        let mut at_input_limit = EMPTY.strip_suffix("</circuit>").unwrap().to_owned();
        at_input_limit.push_str("<!--");
        let padding = MAX_INPUT_BYTES - at_input_limit.len() - "--></circuit>".len();
        at_input_limit.push_str(&" ".repeat(padding));
        at_input_limit.push_str("--></circuit>");
        assert_eq!(at_input_limit.len(), MAX_INPUT_BYTES);
        assert!(parse_v2(at_input_limit.as_bytes()).is_ok());
        let exact = "a".repeat(MAX_STRING_BYTES);
        let exact_xml = doc(
            &format!(
                "<visualElement><elementName>A</elementName><elementAttributes><entry><string>Label</string><string>{exact}</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>"
            ),
            "",
        );
        assert_eq!(
            parse_v2(exact_xml.as_bytes()).unwrap().elements[0]
                .label
                .as_deref(),
            Some(exact.as_str())
        );
        let large = "a".repeat(MAX_STRING_BYTES + 1);
        let large_name = doc(
            &format!(
                "<visualElement><elementName>{large}</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement>"
            ),
            "",
        );
        assert_eq!(
            parse_v2(large_name.as_bytes()).unwrap_err().kind,
            DigReadErrorKind::Limit
        );
        let large_label = doc(
            &format!(
                "<visualElement><elementName>A</elementName><elementAttributes><entry><string>Label</string><string>{large}</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>"
            ),
            "",
        );
        assert_eq!(
            parse_v2(large_label.as_bytes()).unwrap_err().kind,
            DigReadErrorKind::Limit
        );
        let decoded_large = "&#97;".repeat(MAX_STRING_BYTES + 1);
        let decoded_label = doc(
            &format!(
                "<visualElement><elementName>A</elementName><elementAttributes><entry><string>Label</string><string>{decoded_large}</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>"
            ),
            "",
        );
        assert_eq!(
            parse_v2(decoded_label.as_bytes()).unwrap_err().kind,
            DigReadErrorKind::Limit
        );
    }

    #[test]
    fn count_limit_boundaries_are_accepted_and_overages_rejected() {
        let element = "<visualElement><elementName>A</elementName><elementAttributes/><pos x=\"0\" y=\"0\"/></visualElement>";
        let elements = format!(
            "<circuit><version>2</version><attributes/><visualElements>{}</visualElements><wires/></circuit>",
            element.repeat(MAX_ELEMENTS)
        );
        assert_eq!(
            parse_v2(elements.as_bytes()).unwrap().elements.len(),
            MAX_ELEMENTS
        );
        let over = elements.replacen(
            "</visualElements>",
            &format!("{element}</visualElements>"),
            1,
        );
        assert_eq!(
            parse_v2(over.as_bytes()).unwrap_err().kind,
            DigReadErrorKind::Limit
        );
        let wire = "<wire><p1 x=\"0\" y=\"0\"/><p2 x=\"1\" y=\"0\"/></wire>";
        let wires = format!(
            "<circuit><version>2</version><attributes/><visualElements/><wires>{}</wires></circuit>",
            wire.repeat(MAX_WIRES)
        );
        assert_eq!(parse_v2(wires.as_bytes()).unwrap().wires.len(), MAX_WIRES);
        let over = wires.replacen("</wires>", &format!("{wire}</wires>"), 1);
        assert_eq!(
            parse_v2(over.as_bytes()).unwrap_err().kind,
            DigReadErrorKind::Limit
        );
    }

    #[test]
    fn serializer_exact_empty_document_and_semantic_roundtrip() {
        let empty = DigV2 {
            elements: vec![],
            wires: vec![],
        };
        let xml = serialize_v2(&empty).unwrap();
        assert_eq!(xml, b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<circuit><version>2</version><attributes/><visualElements/><wires/></circuit>");
        assert_eq!(parse_v2(&xml).unwrap(), empty);

        let full = DigV2 {
            elements: vec![
                DigElement {
                    name: "arbitrary & <λ>".into(),
                    x: i32::MIN,
                    y: i32::MAX,
                    label: None,
                    value: None,
                },
                DigElement {
                    name: "second".into(),
                    x: -7,
                    y: 8,
                    label: Some(String::new()),
                    value: None,
                },
                DigElement {
                    name: "third".into(),
                    x: 9,
                    y: -10,
                    label: Some("ordered".into()),
                    value: None,
                },
            ],
            wires: vec![
                DigWire {
                    p1: DigPoint {
                        x: i32::MAX,
                        y: i32::MIN,
                    },
                    p2: DigPoint { x: -5, y: 6 },
                },
                DigWire {
                    p1: DigPoint { x: 6, y: -5 },
                    p2: DigPoint { x: 6, y: -5 },
                },
            ],
        };
        let bytes = serialize_v2(&full).unwrap();
        assert_eq!(parse_v2(&bytes).unwrap(), full);
        assert_eq!(serialize_v2(&parse_v2(&bytes).unwrap()).unwrap(), bytes);
        assert_eq!(serialize_v2(&full).unwrap(), bytes);
        let noncanonical = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("<circuit>", "<circuit>\n")
            .replace("<visualElements>", "<visualElements>\n")
            .replace("<wires>", "<wires>\n");
        assert_eq!(
            serialize_v2(&parse_v2(noncanonical.as_bytes()).unwrap()).unwrap(),
            bytes
        );
    }

    #[test]
    fn serializer_preserves_xml_text_characters_and_rejects_invalid_values() {
        let value = "λ\tline\nreturn\r\r\n &amp; ]]> <";
        let model = DigV2 {
            elements: vec![DigElement {
                name: "N\t\n\r".into(),
                x: 0,
                y: 1,
                label: Some(value.into()),
                value: None,
            }],
            wires: vec![],
        };
        let bytes = serialize_v2(&model).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(xml.contains("&#13;"));
        assert!(xml.contains("]]&gt;"));
        assert!(xml.contains("&amp;amp;"));
        assert_eq!(parse_v2(&bytes).unwrap(), model);
        for invalid in ["bad\u{1}", "bad\u{0}"] {
            let model = DigV2 {
                elements: vec![DigElement {
                    name: invalid.into(),
                    x: 0,
                    y: 0,
                    label: None,
                    value: None,
                }],
                wires: vec![],
            };
            assert_eq!(
                serialize_v2(&model).unwrap_err().context,
                "elements[0].name"
            );
            let model = DigV2 {
                elements: vec![DigElement {
                    name: "ok".into(),
                    x: 0,
                    y: 0,
                    label: Some(invalid.into()),
                    value: None,
                }],
                wires: vec![],
            };
            assert_eq!(
                serialize_v2(&model).unwrap_err().context,
                "elements[0].label"
            );
        }
    }

    #[test]
    fn serializer_enforces_each_model_budget_and_output_during_build() {
        let element = DigElement {
            name: "x".into(),
            x: 0,
            y: 0,
            label: None,
            value: None,
        };
        let wire = DigWire {
            p1: DigPoint { x: 0, y: 0 },
            p2: DigPoint { x: 1, y: 1 },
        };
        assert!(
            serialize_v2(&DigV2 {
                elements: vec![element.clone(); MAX_ELEMENTS],
                wires: vec![]
            })
            .is_ok()
        );
        assert!(
            serialize_v2(&DigV2 {
                elements: vec![],
                wires: vec![wire.clone(); MAX_WIRES]
            })
            .is_ok()
        );
        assert_eq!(
            serialize_v2(&DigV2 {
                elements: vec![element.clone(); MAX_ELEMENTS + 1],
                wires: vec![]
            })
            .unwrap_err()
            .kind,
            DigWriteErrorKind::Limit
        );
        assert_eq!(
            serialize_v2(&DigV2 {
                elements: vec![],
                wires: vec![wire.clone(); MAX_WIRES + 1]
            })
            .unwrap_err()
            .kind,
            DigWriteErrorKind::Limit
        );
        assert_eq!(
            serialize_v2(&DigV2 {
                elements: vec![DigElement {
                    name: "λ".repeat(MAX_STRING_BYTES / 2 + 1),
                    ..element.clone()
                }],
                wires: vec![]
            })
            .unwrap_err()
            .kind,
            DigWriteErrorKind::Limit
        );
        assert!(
            serialize_v2(&DigV2 {
                elements: vec![DigElement {
                    name: "λ".repeat(MAX_STRING_BYTES / 2),
                    ..element.clone()
                }],
                wires: vec![]
            })
            .is_ok()
        );
        assert!(
            serialize_v2(&DigV2 {
                elements: vec![DigElement {
                    label: Some("a".repeat(MAX_STRING_BYTES)),
                    value: None,
                    ..element.clone()
                }],
                wires: vec![]
            })
            .is_ok()
        );
        assert_eq!(
            serialize_v2(&DigV2 {
                elements: vec![DigElement {
                    label: Some("a".repeat(MAX_STRING_BYTES + 1)),
                    value: None,
                    ..element.clone()
                }],
                wires: vec![]
            })
            .unwrap_err()
            .context,
            "elements[0].label"
        );

        let empty_suffix = DigElement {
            label: Some(String::new()),
            value: None,
            ..element.clone()
        };
        let base_empty = serialize_v2(&DigV2 {
            elements: vec![empty_suffix.clone()],
            wires: vec![],
        })
        .unwrap()
        .len();
        let base_full = serialize_v2(&DigV2 {
            elements: vec![
                DigElement {
                    label: Some("x".repeat(MAX_STRING_BYTES)),
                    value: None,
                    ..element.clone()
                },
                empty_suffix.clone(),
            ],
            wires: vec![],
        })
        .unwrap()
        .len();
        let per_full = base_full - base_empty;
        let full_labels = (MAX_OUTPUT_BYTES - base_empty) / per_full;
        let mut elements = vec![
            DigElement {
                label: Some("x".repeat(MAX_STRING_BYTES)),
                value: None,
                ..element.clone()
            };
            full_labels
        ];
        elements.push(empty_suffix);
        let base = serialize_v2(&DigV2 {
            elements: elements.clone(),
            wires: vec![],
        })
        .unwrap();
        let fill = MAX_OUTPUT_BYTES - base.len();
        assert!(fill <= MAX_STRING_BYTES);
        if let Some(last) = elements.last_mut() {
            last.label = Some("z".repeat(fill));
        }
        assert_eq!(
            serialize_v2(&DigV2 {
                elements,
                wires: vec![]
            })
            .unwrap()
            .len(),
            MAX_OUTPUT_BYTES
        );
        let huge = DigV2 {
            elements: vec![
                DigElement {
                    name: "&".repeat(MAX_STRING_BYTES),
                    x: 0,
                    y: 0,
                    label: Some("&".repeat(MAX_STRING_BYTES)),
                    value: None,
                };
                MAX_ELEMENTS
            ],
            wires: vec![],
        };
        assert_eq!(
            serialize_v2(&huge).unwrap_err().kind,
            DigWriteErrorKind::OutputLimit
        );
    }

    #[test]
    fn outside_root_requires_raw_xml_whitespace_but_inside_entities_are_decoded() {
        for prefix in ["&#32;", "&#x9;"] {
            let xml = format!("{prefix}{EMPTY}");
            let error = parse_v2(xml.as_bytes()).unwrap_err();
            assert_eq!(error.kind, DigReadErrorKind::Xml);
            assert_eq!(error.offset, 0);
            let xml = format!("{EMPTY}{prefix}");
            let error = parse_v2(xml.as_bytes()).unwrap_err();
            assert_eq!(error.kind, DigReadErrorKind::Xml);
            assert_eq!(error.offset, EMPTY.len());
        }
        assert!(parse_v2(format!(" \t\r\n{EMPTY}\n ").as_bytes()).is_ok());
        let label = EMPTY.replace("<visualElements/>", "<visualElements><visualElement><elementName>A</elementName><elementAttributes><entry><string>Label</string><string>&#32;&#x9;</string></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement></visualElements>");
        assert_eq!(
            parse_v2(label.as_bytes()).unwrap().elements[0]
                .label
                .as_deref(),
            Some(" \t")
        );
    }

    #[test]
    fn every_supported_kind_roundtrips_integer_centers_near_f32_precision_boundary() {
        let kinds = [
            ComponentKind::Input,
            ComponentKind::Output,
            ComponentKind::And,
            ComponentKind::NAnd,
            ComponentKind::Or,
            ComponentKind::NOr,
            ComponentKind::XOr,
            ComponentKind::XNOr,
            ComponentKind::Not,
            ComponentKind::Const,
        ];
        for kind in kinds {
            for (x, y) in [(67_108_864.0, -67_108_864.0), (-67_108_864.0, 67_108_864.0)] {
                let document = Document {
                    components: vec![Component {
                        id: ComponentId(0),
                        kind,
                        position: Position { x, y },
                        label: String::new(),
                        constant_value: true,
                    }],
                    wires: crate::domain::Wiring::GeometricSegments(Vec::new()),
                };
                let encoded = serialize_v2(&from_domain(&document).unwrap()).unwrap();
                let reopened = to_domain(&parse_v2(&encoded).unwrap()).unwrap();
                assert_eq!(
                    reopened.components, document.components,
                    "{kind:?} at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn const_values_preserve_absence_legacy_int_and_canonical_long() {
        let element = |entries: &str| {
            format!(
                "<visualElement><elementName>Const</elementName><elementAttributes>{entries}</elementAttributes><pos x=\"-120\" y=\"-20\"/></visualElement>"
            )
        };
        let absent = parse_v2(
            EMPTY
                .replace(
                    "<visualElements/>",
                    &format!("<visualElements>{}</visualElements>", element("")),
                )
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(absent.elements[0].value, None);
        assert!(to_domain(&absent).unwrap().components[0].constant_value);
        for (tag, raw, expected) in [
            ("long", "1", true),
            ("int", "  -0\r\n", false),
            ("int", "+1", true),
        ] {
            let entry = format!("<entry><string>Value</string><{tag}>{raw}</{tag}></entry>");
            let xml = EMPTY.replace(
                "<visualElements/>",
                &format!("<visualElements>{}</visualElements>", element(&entry)),
            );
            let parsed = parse_v2(xml.as_bytes()).unwrap();
            assert_eq!(parsed.elements[0].value, Some(expected));
            let document = to_domain(&parsed).unwrap();
            assert_eq!(document.components[0].constant_value, expected);
            let exported = serialize_v2(&from_domain(&document).unwrap()).unwrap();
            let reopened = to_domain(&parse_v2(&exported).unwrap()).unwrap();
            assert_eq!(
                reopened.components[0].position,
                document.components[0].position
            );
            assert_eq!(reopened.components[0].constant_value, expected);
            let expected_xml = if expected {
                "<entry><string>Value</string><long>1</long></entry>"
            } else {
                "<entry><string>Value</string><long>0</long></entry>"
            };
            assert!(String::from_utf8(exported).unwrap().contains(expected_xml));
        }
        let label_first = "<entry><string>Label</string><string>escaped &amp; name</string></entry><entry><string>Value</string><long>0</long></entry>";
        let label_last = "<entry><string>Value</string><long>0</long></entry><entry><string>Label</string><string>escaped &amp; name</string></entry>";
        for entries in [label_first, label_last] {
            let xml = EMPTY.replace(
                "<visualElements/>",
                &format!("<visualElements>{}</visualElements>", element(entries)),
            );
            assert_eq!(
                parse_v2(xml.as_bytes()).unwrap().elements[0]
                    .label
                    .as_deref(),
                Some("escaped & name")
            );
        }
    }

    #[test]
    fn const_value_is_strict_and_only_const_can_be_exported_with_value() {
        let element = |payload: &str| {
            format!(
                "<visualElement><elementName>Const</elementName><elementAttributes><entry><string>Value</string>{payload}</entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>"
            )
        };
        for payload in [
            "<long>-1</long>",
            "<long>2</long>",
            "<long>9223372036854775808</long>",
            "<long>0x1</long>",
            "<integer>1</integer>",
            "<long>1 <x/></long>",
        ] {
            let xml = EMPTY.replace(
                "<visualElements/>",
                &format!("<visualElements>{}</visualElements>", element(payload)),
            );
            assert!(parse_v2(xml.as_bytes()).is_err(), "{payload}");
        }
        let duplicate = "<visualElement><elementName>Const</elementName><elementAttributes><entry><string>Value</string><long>1</long></entry><entry><string>Value</string><int>0</int></entry></elementAttributes><pos x=\"0\" y=\"0\"/></visualElement>";
        let xml = EMPTY.replace(
            "<visualElements/>",
            &format!("<visualElements>{}</visualElements>", duplicate),
        );
        assert_eq!(
            parse_v2(xml.as_bytes()).unwrap_err().kind,
            DigReadErrorKind::DuplicateField
        );
        let nonconst = DigV2 {
            elements: vec![DigElement {
                name: "In".into(),
                x: 0,
                y: 0,
                label: None,
                value: Some(true),
            }],
            wires: vec![],
        };
        assert_eq!(
            serialize_v2(&nonconst).unwrap_err().kind,
            DigWriteErrorKind::UnsupportedValue
        );
        let nonconst_with_value = "<visualElement><elementAttributes><entry><string>Value</string><long>0</long></entry></elementAttributes><pos x=\"0\" y=\"0\"/><elementName>In</elementName></visualElement>";
        let xml = EMPTY.replace(
            "<visualElements/>",
            &format!("<visualElements>{}</visualElements>", nonconst_with_value),
        );
        let error = parse_v2(xml.as_bytes()).unwrap_err();
        assert_eq!(error.kind, DigReadErrorKind::Unsupported);
        assert_eq!(error.context, "Value");
        assert_eq!(error.offset, xml.find("<long>0").unwrap());

        let low_nonconst = Document {
            components: vec![Component {
                id: ComponentId(0),
                kind: ComponentKind::Input,
                position: Position { x: 0.0, y: 0.0 },
                label: String::new(),
                constant_value: false,
            }],
            wires: crate::domain::Wiring::GeometricSegments(vec![]),
        };
        assert!(
            from_domain(&low_nonconst)
                .unwrap_err()
                .to_string()
                .contains("not Const")
        );
    }

    #[test]
    fn errors_are_located_and_name_the_relevant_context() {
        let bad = EMPTY.replace(
            "<visualElements/>",
            "<visualElements><mystery/></visualElements>",
        );
        let err = expect_error(&bad, DigReadErrorKind::Unsupported, "visualElements");
        assert_eq!(err.offset, bad.find("<mystery").unwrap());
        let err = expect_error(
            "<circuit><version>not-a-number</version></circuit>",
            DigReadErrorKind::InvalidValue,
            "version",
        );
        assert_eq!(err.offset, 9);
    }

    fn geometric_document(wires: Vec<DigWire>, elements: Vec<DigElement>) -> DigV2 {
        DigV2 { elements, wires }
    }
    fn dw(a: (i32, i32), b: (i32, i32)) -> DigWire {
        DigWire {
            p1: DigPoint { x: a.0, y: a.1 },
            p2: DigPoint { x: b.0, y: b.1 },
        }
    }

    #[test]
    fn geometric_conversion_is_verbatim_and_does_not_require_a_netlist() {
        let source = geometric_document(
            vec![
                dw((10, 0), (0, 0)),   // reversed orientation
                dw((10, 0), (0, 0)),   // duplicate geometry remains a separate record
                dw((0, 0), (10, 0)),   // reversed duplicate
                dw((10, 0), (10, 10)), // T
                dw((10, 0), (20, 0)),
                dw((10, 0), (10, -10)),     // four-way endpoint
                dw((40, 0), (50, 10)),      // diagonal
                dw((100, 100), (101, 101)), // disconnected branch
                dw((120, 100), (130, 100)), // loop edge 1
                dw((130, 100), (130, 110)), // loop edge 2
                dw((130, 110), (120, 110)), // loop edge 3
                dw((120, 110), (120, 100)), // loop edge 4
                dw((200, 0), (210, 10)),    // crossing edge 1
                dw((200, 10), (210, 0)),    // crossing edge 2
                dw((300, 0), (320, 0)),     // positive collinear overlap
                dw((310, 0), (330, 0)),
            ],
            vec![DigElement {
                name: "In".into(),
                x: -20,
                y: 5,
                label: Some(" source ".into()),
                value: None,
            }],
        );
        let document = to_domain(&source).unwrap();
        assert!(matches!(
            document.wires,
            crate::domain::Wiring::GeometricSegments(_)
        ));
        let segments = document.wires.geometric().unwrap();
        assert_eq!(segments.len(), source.wires.len());
        assert_eq!(
            segments.iter().map(|s| s.id).collect::<Vec<_>>(),
            (0..16).collect::<Vec<_>>()
        );
        assert_eq!(
            segments
                .iter()
                .map(|s| (s.a.x, s.a.y, s.b.x, s.b.y))
                .collect::<Vec<_>>(),
            source
                .wires
                .iter()
                .map(|w| (w.p1.x, w.p1.y, w.p2.x, w.p2.y))
                .collect::<Vec<_>>()
        );
        assert_eq!(from_domain(&document).unwrap(), source);
        let xml = serialize_v2(&source).unwrap();
        assert_eq!(parse_v2(&xml).unwrap(), source);
    }

    #[test]
    fn empty_and_extreme_coordinate_geometric_documents_roundtrip() {
        assert_eq!(
            to_domain(&geometric_document(vec![], vec![]))
                .unwrap()
                .wires
                .len(),
            0
        );
        let source = geometric_document(
            vec![
                dw((i32::MIN, i32::MAX), (i32::MAX, i32::MIN)),
                dw((i32::MAX, i32::MAX), (i32::MIN, i32::MIN)),
            ],
            vec![],
        );
        let imported = to_domain(&source).unwrap();
        assert_eq!(from_domain(&imported).unwrap(), source);
        // Origin itself may not be exactly representable as f32; its integer-derived
        // center can still be. Preserve that accepted origin exactly on export.
        let representable_center = DigV2 {
            elements: vec![DigElement {
                name: "In".into(),
                x: 16_777_217,
                y: 0,
                label: None,
                value: None,
            }],
            wires: vec![],
        };
        let roundtrip = from_domain(&to_domain(&representable_center).unwrap()).unwrap();
        assert_eq!(roundtrip, representable_center);
        let bad = DigV2 {
            elements: vec![DigElement {
                name: "XOr".into(),
                x: 16_777_217,
                y: 0,
                label: None,
                value: None,
            }],
            wires: vec![],
        };
        assert!(
            to_domain(&bad)
                .unwrap_err()
                .to_string()
                .contains("precision")
        );
    }

    #[test]
    fn geometric_conversion_refuses_zero_length_and_invalid_component_placements_only() {
        let zero = geometric_document(vec![dw((i32::MIN, 0), (i32::MIN, 0))], vec![]);
        assert!(
            to_domain(&zero)
                .unwrap_err()
                .to_string()
                .contains("wires[0]")
        );
        let invalid_position = DigV2 {
            elements: vec![DigElement {
                name: "And".into(),
                x: i32::MAX,
                y: 0,
                label: None,
                value: None,
            }],
            wires: vec![],
        };
        assert!(to_domain(&invalid_position).is_err());
        let duplicate_ids = Document {
            components: vec![],
            wires: crate::domain::Wiring::GeometricSegments(vec![
                crate::geometric_wiring::Segment {
                    id: 7,
                    a: crate::geometric_wiring::Point::new(0, 0),
                    b: crate::geometric_wiring::Point::new(1, 1),
                },
                crate::geometric_wiring::Segment {
                    id: 7,
                    a: crate::geometric_wiring::Point::new(2, 2),
                    b: crate::geometric_wiring::Point::new(3, 3),
                },
            ]),
        };
        assert!(
            from_domain(&duplicate_ids)
                .unwrap_err()
                .to_string()
                .contains("duplicate segment ID")
        );
        let zero_domain = Document {
            components: vec![],
            wires: crate::domain::Wiring::GeometricSegments(vec![
                crate::geometric_wiring::Segment {
                    id: 9,
                    a: crate::geometric_wiring::Point::new(4, 4),
                    b: crate::geometric_wiring::Point::new(4, 4),
                },
            ]),
        };
        assert!(
            from_domain(&zero_domain)
                .unwrap_err()
                .to_string()
                .contains("index 0")
        );
    }
}
