//! A small XML reader, enough for the files EAGLE writes: elements with attributes and text,
//! the five named character references and numeric ones, comments, CDATA and a document type
//! line. It reads a whole document into a tree.
//!
//! A document that defines entities of its own is refused: they are how a small file makes a
//! reader produce a huge one, and EAGLE writes none. Nesting and the number of elements are
//! bounded.

use crate::import::ImportError;
use std::borrow::Cow;

/// How deep elements may nest. EAGLE nests about ten deep.
pub(crate) const MAX_DEPTH: usize = 64;

/// The most elements one document may hold.
pub(crate) const MAX_ELEMENTS: usize = 4_000_000;

/// An element: its name, attributes, child elements and text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Element<'a> {
    pub(crate) name: &'a str,
    attributes: Vec<(&'a str, Cow<'a, str>)>,
    pub(crate) children: Vec<Element<'a>>,
    /// The character data directly inside the element, joined.
    pub(crate) text: String,
}

impl<'a> Element<'a> {
    /// The value of an attribute.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(n, _)| *n == name).map(|(_, value)| value.as_ref())
    }

    /// An attribute as a finite number.
    pub(crate) fn number(&self, name: &str) -> Option<f64> {
        self.attr(name).and_then(|v| v.trim().parse::<f64>().ok()).filter(|n| n.is_finite())
    }

    /// The first child element called `name`.
    pub(crate) fn child(&self, name: &str) -> Option<&Element<'a>> {
        self.children.iter().find(|c| c.name == name)
    }

    /// How much the element holds: itself, its attributes, its text and everything under it,
    /// with one more for every 64 bytes of an attribute's value or of text. What a reader makes
    /// of an element each time it is used is in proportion to this.
    pub(crate) fn weight(&self) -> usize {
        let attributes = self.attributes.iter().map(|(_, value)| 1 + value.len() / 64).fold(0_usize, usize::saturating_add);
        let own = 1_usize.saturating_add(attributes).saturating_add(self.text.len() / 64);
        self.children.iter().map(Element::weight).fold(own, usize::saturating_add)
    }

    /// The child elements called `name`.
    pub(crate) fn all<'e>(&'e self, name: &'e str) -> impl Iterator<Item = &'e Element<'a>> + 'e {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// Reads a document and returns its root element.
pub(crate) fn parse(text: &str) -> Result<Element<'_>, ImportError> {
    let mut reader = Reader { text: text.strip_prefix('\u{feff}').unwrap_or(text), at: 0, elements: 0 };
    reader.skip_misc()?;
    if reader.peek() != Some(b'<') {
        return Err(reader.error("expected an element"));
    }
    let root = reader.element(0)?;
    reader.skip_misc()?;
    if reader.peek().is_some() { Err(reader.error("text after the end of the document")) } else { Ok(root) }
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
    elements: usize,
}

impl<'a> Reader<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn rest(&self) -> &'a str {
        self.text.get(self.at..).unwrap_or_default()
    }

    fn error(&self, message: &str) -> ImportError {
        let before = self.text.as_bytes().get(..self.at.min(self.text.len())).unwrap_or_default();
        let line = before.iter().filter(|&&b| b == b'\n').count().saturating_add(1);
        ImportError::Syntax { line, message: message.to_owned() }
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    /// Moves past `end`, which must come; `what` names the thing left open otherwise.
    fn skip_past(&mut self, end: &str, what: &str) -> Result<&'a str, ImportError> {
        let rest = self.rest();
        let Some(i) = rest.find(end) else { return Err(self.error(&format!("{what} is not closed"))) };
        self.at += i + end.len();
        Ok(rest.get(..i).unwrap_or_default())
    }

    /// Skips white space, comments, the XML declaration and the document type line, between
    /// elements. Returns whether it skipped anything that is not white space.
    fn skip_misc(&mut self) -> Result<(), ImportError> {
        loop {
            self.skip_space();
            let rest = self.rest();
            if rest.starts_with("<!--") {
                self.skip_past("-->", "a comment")?;
            } else if rest.starts_with("<?") {
                self.skip_past("?>", "a processing instruction")?;
            } else if rest.starts_with("<!DOCTYPE") {
                let declaration = self.skip_past(">", "the document type")?;
                if declaration.contains('[') {
                    return Err(self.error("a document that defines its own entities is not read"));
                }
            } else {
                return Ok(());
            }
        }
    }

    /// A name: up to white space, `=`, `/` or `>`.
    fn name(&mut self) -> Result<&'a str, ImportError> {
        let start = self.at;
        while self.peek().is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'/' | b'>' | b'<')) {
            self.at += 1;
        }
        match self.text.get(start..self.at) {
            Some(name) if !name.is_empty() => Ok(name),
            _ => Err(self.error("expected a name")),
        }
    }

    /// The element that starts at the current `<`, `depth` elements deep.
    fn element(&mut self, depth: usize) -> Result<Element<'a>, ImportError> {
        self.elements += 1;
        if self.elements > MAX_ELEMENTS {
            return Err(self.error("the document has too many elements to read"));
        }
        if depth >= MAX_DEPTH {
            return Err(self.error("elements nested too deeply"));
        }
        self.at += 1;
        let mut element = Element { name: self.name()?, ..Element::default() };
        loop {
            self.skip_space();
            match self.peek() {
                Some(b'/') if self.rest().starts_with("/>") => {
                    self.at += 2;
                    return Ok(element);
                }
                Some(b'>') => {
                    self.at += 1;
                    break;
                }
                Some(_) => element.attributes.push(self.attribute()?),
                None => return Err(self.error("an element is not closed")),
            }
        }
        self.content(&mut element, depth)?;
        Ok(element)
    }

    /// `name="value"`.
    fn attribute(&mut self) -> Result<(&'a str, Cow<'a, str>), ImportError> {
        let name = self.name()?;
        self.skip_space();
        if self.peek() != Some(b'=') {
            return Err(self.error("expected `=` after an attribute's name"));
        }
        self.at += 1;
        self.skip_space();
        let quote = match self.peek() {
            Some(q @ (b'"' | b'\'')) => q,
            _ => return Err(self.error("expected a quoted value")),
        };
        self.at += 1;
        let raw = self.skip_past(if quote == b'"' { "\"" } else { "'" }, "an attribute's value")?;
        Ok((name, self.unescaped(raw)?))
    }

    /// What is inside an element, up to and including its closing tag.
    fn content(&mut self, element: &mut Element<'a>, depth: usize) -> Result<(), ImportError> {
        loop {
            let rest = self.rest();
            let Some(i) = rest.find('<') else { return Err(self.error("an element is not closed")) };
            let text = rest.get(..i).unwrap_or_default();
            if !text.trim().is_empty() {
                element.text.push_str(&self.unescaped(text)?);
            }
            self.at += i;
            let rest = self.rest();
            if rest.starts_with("</") {
                self.at += 2;
                let closing = self.name()?;
                self.skip_space();
                if closing != element.name || self.peek() != Some(b'>') {
                    return Err(self.error(&format!("expected the end of `{}`", element.name)));
                }
                self.at += 1;
                return Ok(());
            } else if rest.starts_with("<!--") {
                self.skip_past("-->", "a comment")?;
            } else if rest.starts_with("<![CDATA[") {
                self.at += "<![CDATA[".len();
                element.text.push_str(self.skip_past("]]>", "a CDATA section")?);
            } else if rest.starts_with("<?") {
                self.skip_past("?>", "a processing instruction")?;
            } else {
                element.children.push(self.element(depth + 1)?);
            }
        }
    }

    /// Text with its character references read: `&amp;`, `&#65;`, `&#x41;`.
    fn unescaped(&self, raw: &'a str) -> Result<Cow<'a, str>, ImportError> {
        if !raw.contains('&') {
            return Ok(Cow::Borrowed(raw));
        }
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(i) = rest.find('&') {
            out.push_str(rest.get(..i).unwrap_or_default());
            let after = rest.get(i + 1..).unwrap_or_default();
            let Some(end) = after.find(';').filter(|&end| end <= 10) else { return Err(self.error("a character reference is not closed")) };
            let name = after.get(..end).unwrap_or_default();
            let character = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let code = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => name.strip_prefix('#').and_then(|decimal| decimal.parse::<u32>().ok()),
                    };
                    code.and_then(char::from_u32)
                }
            };
            out.push(character.ok_or_else(|| self.error(&format!("unknown character reference `&{name};`")))?);
            rest = after.get(end + 1..).unwrap_or_default();
        }
        out.push_str(rest);
        Ok(Cow::Owned(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_elements_attributes_and_text() {
        let doc = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE eagle SYSTEM \"eagle.dtd\">\n<!-- a comment -->\n\
            <eagle version=\"9.6.2\">\n  <drawing>\n    <layer number='1' name=\"Top\"/>\n    <text x=\"1.5\" y=\"-2\">R&amp;D &lt;v1&gt; &#65;&#x42;<!-- no -->\n</text>\n\
            <note><![CDATA[<raw> & text]]></note>\n  </drawing>\n</eagle>\n<!-- after -->\n";
        let root = parse(doc).expect("parse");
        assert_eq!((root.name, root.attr("version"), root.number("version")), ("eagle", Some("9.6.2"), None));
        let drawing = root.child("drawing").expect("drawing");
        assert_eq!(drawing.children.len(), 3);
        let layer = drawing.child("layer").expect("layer");
        assert_eq!((layer.attr("number"), layer.number("number"), layer.attr("name"), layer.attr("nope")), (Some("1"), Some(1.0), Some("Top"), None));
        let text = drawing.child("text").expect("text");
        assert_eq!((text.number("x"), text.number("y"), text.text.as_str()), (Some(1.5), Some(-2.0), "R&D <v1> AB"));
        assert_eq!(drawing.child("note").map(|n| n.text.as_str()), Some("<raw> & text"));
        assert_eq!(drawing.all("layer").count(), 1);
        assert_eq!(parse("<a b=\"&quot;x&apos;\"/>").expect("parse").attr("b"), Some("\"x'"));
        // An element weighs one, and one more for each attribute, child and 64 bytes of text.
        assert_eq!((layer.weight(), text.weight(), drawing.weight()), (3, 3, 8));
        let long = format!("<a b=\"{}\">{}</a>", "y".repeat(128), "x".repeat(640));
        assert_eq!(parse(&long).expect("parse").weight(), 1 + (1 + 2) + 10);
    }

    #[test]
    fn broken_documents_are_errors_with_their_line() {
        for (doc, line) in [
            ("", 1),
            ("text", 1),
            ("<a>", 1),
            ("<a>\n<b>\n</a>", 3),
            ("<a></b>", 1),
            ("<a b></a>", 1),
            ("<a b=c/>", 1),
            ("<a b=\"c/>", 1),
            ("<a>&nope;</a>", 1),
            ("<a>&#xD800;</a>", 1),
            ("<a>&amp</a>", 1),
            ("<a/><b/>", 1),
            ("<a/>\n\ntext", 3),
            ("<!-- open", 1),
            ("<a><![CDATA[open</a>", 1),
            ("< a/>", 1),
        ] {
            let err = parse(doc).err();
            assert!(matches!(err, Some(ImportError::Syntax { line: l, .. }) if l == line), "{doc:?}: {err:?}");
        }
    }

    #[test]
    fn hostile_documents_are_refused_not_followed() {
        // Entities of the document's own: the way a few bytes turn into gigabytes.
        let bomb = "<!DOCTYPE a [<!ENTITY x \"xxxxxxxxxx\"><!ENTITY y \"&x;&x;&x;&x;\">]><a>&y;</a>";
        assert!(matches!(parse(bomb), Err(ImportError::Syntax { message, .. }) if message.contains("entities")));
        let deep = "<a>".repeat(100_000);
        assert!(matches!(parse(&deep), Err(ImportError::Syntax { message, .. }) if message.contains("nested")));
        let wide = format!("<a>{}</a>", "<b/>".repeat(MAX_ELEMENTS));
        assert!(matches!(parse(&wide), Err(ImportError::Syntax { message, .. }) if message.contains("too many")));
        let many = format!("<a>{}</a>", "<b c=\"1\"/>".repeat(100_000));
        assert_eq!(parse(&many).map(|a| a.children.len()).ok(), Some(100_000));
    }
}
