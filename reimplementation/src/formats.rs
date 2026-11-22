//! Strict, bounded reader for the explicitly supported Digital `.dig` v2 subset.
//!
//! This module intentionally returns a format-specific representation; it does not
//! resolve component names, mutate the editor document, or perform file I/O.

const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 10_000;
const MAX_WIRES: usize = 30_000;
const MAX_STRING_BYTES: usize = 4 * 1024;

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
        if entry.children.len() != 2
            || entry.children[0].name != "string"
            || entry.children[1].name != "string"
        {
            return Err(error(
                DigReadErrorKind::Unsupported,
                entry.offset,
                "elementAttributes",
                "only a Label string entry is supported",
            ));
        }
        let key = leaf(&entry.children[0], "attribute key")?;
        if key != "Label" {
            return Err(error(
                DigReadErrorKind::Unsupported,
                entry.children[0].offset,
                "elementAttributes",
                format!("unsupported attribute {key:?}"),
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
        let value = leaf(&entry.children[1], "Label")?;
        bounded_string(value, entry.children[1].offset, "Label")?;
        label = Some(value.to_owned());
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
        if self.bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
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
                let decoded = decode_entities(&self.source[start..self.at], start)?;
                if let Some(parent) = self.stack.last_mut() {
                    parent.text.push_str(&decoded);
                } else if !all_xml_space(&decoded) {
                    return Err(error(
                        DigReadErrorKind::Xml,
                        start,
                        "document",
                        "text outside root element",
                    ));
                }
            } else if self.starts("<!--") {
                self.skip_comment()?;
            } else if self.starts("<?xml") && self.stack.is_empty() && self.root.is_none() {
                if self.at != 0 && self.at != 3
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
        while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
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
            if declaration.contains("encoding")
                && !declaration
                    .to_ascii_lowercase()
                    .contains("encoding=\"utf-8\"")
                && !declaration
                    .to_ascii_lowercase()
                    .contains("encoding='utf-8'")
                && !declaration
                    .to_ascii_lowercase()
                    .contains("encoding=\"utf8\"")
                && !declaration.to_ascii_lowercase().contains("encoding='utf8'")
            {
                return Err(error(
                    DigReadErrorKind::Unsupported,
                    start,
                    "XML declaration",
                    "only UTF-8 encoding is supported",
                ));
            }
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

fn decode_entities(value: &str, offset: usize) -> Result<String, DigReadError> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    let mut consumed = 0;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        consumed += index;
        let tail = &rest[index + 1..];
        let end = tail.find(';').ok_or_else(|| {
            error(
                DigReadErrorKind::Xml,
                offset + consumed,
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
                offset + consumed,
                "entity",
                "only XML predefined and numeric entities are supported",
            )
        })?;
        if matches!(ch as u32, 0..=8 | 11..=12 | 14..=31 | 0xfffe..=0xffff) {
            return Err(error(
                DigReadErrorKind::Xml,
                offset + consumed,
                "entity",
                "invalid XML character reference",
            ));
        }
        output.push(ch);
        let skip = index + end + 2;
        consumed += skip;
        rest = &tail[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
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
                label: Some("R & D <λ> 💡".into())
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
    fn rejects_dtd_external_entities_invalid_utf8_and_malformed_entities() {
        for input in [
            "<!DOCTYPE circuit><circuit/>",
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
}
