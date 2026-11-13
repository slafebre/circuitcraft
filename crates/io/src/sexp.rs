//! S-expressions, as KiCad files are written: lists in parentheses of bare words, quoted strings
//! and more lists.
//!
//! A file is read one top-level item at a time ([`Reader::next_item`]), so the memory in use
//! follows the largest item, not the file. Nesting and the size of one item are bounded.

use crate::import::ImportError;
use std::borrow::Cow;

/// How deep lists may nest. KiCad nests about ten deep.
pub(crate) const MAX_DEPTH: usize = 64;

/// The most words and lists one top-level item may hold: a zone poured with a million corners
/// has four million.
pub(crate) const MAX_NODES: usize = 8_000_000;

/// A word (bare or quoted) or a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node<'a> {
    Atom(Cow<'a, str>),
    List(Vec<Node<'a>>),
}

impl<'a> Node<'a> {
    /// The parts of a list; none for a word.
    fn parts(&self) -> &[Node<'a>] {
        match self {
            Node::Atom(_) => &[],
            Node::List(parts) => parts,
        }
    }

    /// This node as a word.
    pub(crate) fn word(&self) -> Option<&str> {
        match self {
            Node::Atom(word) => Some(word),
            Node::List(_) => None,
        }
    }

    /// The word a list starts with: what kind of thing it is.
    pub(crate) fn head(&self) -> Option<&str> {
        self.parts().first().and_then(Node::word)
    }

    /// What follows the head.
    pub(crate) fn args(&self) -> &[Node<'a>] {
        self.parts().get(1..).unwrap_or_default()
    }

    /// The `i`-th argument, if it is a word.
    pub(crate) fn text(&self, i: usize) -> Option<&str> {
        self.args().get(i).and_then(Node::word)
    }

    /// The `i`-th argument, if it is a finite number.
    pub(crate) fn number(&self, i: usize) -> Option<f64> {
        self.text(i).and_then(|word| word.parse::<f64>().ok()).filter(|n| n.is_finite())
    }

    /// The lists among the arguments that start with `name`.
    pub(crate) fn all<'n>(&'n self, name: &'n str) -> impl Iterator<Item = &'n Node<'a>> + 'n {
        self.args().iter().filter(move |part| part.head() == Some(name))
    }

    /// The first list among the arguments that starts with `name`.
    pub(crate) fn child(&self, name: &str) -> Option<&Node<'a>> {
        self.args().iter().find(|part| part.head() == Some(name))
    }

    /// Whether one of the arguments is the bare word `word`.
    pub(crate) fn flag(&self, word: &str) -> bool {
        self.args().iter().any(|part| part.word() == Some(word))
    }
}

/// Reads the items of one outer list, one at a time.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    text: &'a str,
    at: usize,
    /// How many words and lists the item being read has so far.
    nodes: usize,
}

impl<'a> Reader<'a> {
    /// Starts reading `text`, which must be one list; returns the word it starts with.
    pub(crate) fn open(text: &'a str) -> Result<(Reader<'a>, Cow<'a, str>), ImportError> {
        let mut reader = Reader { text: text.strip_prefix('\u{feff}').unwrap_or(text), at: 0, nodes: 0 };
        reader.skip_space();
        if reader.peek() != Some(b'(') {
            return Err(reader.error("expected `(`"));
        }
        reader.at += 1;
        reader.skip_space();
        match reader.atom()? {
            Some(head) => Ok((reader, head)),
            None => Err(reader.error("expected a word after `(`")),
        }
    }

    /// The next item of the outer list, or `None` at its end. Anything after the end is an error.
    pub(crate) fn next_item(&mut self) -> Result<Option<Node<'a>>, ImportError> {
        self.nodes = 0;
        self.skip_space();
        if self.peek() == Some(b')') {
            self.at += 1;
            self.skip_space();
            return if self.peek().is_none() { Ok(None) } else { Err(self.error("text after the end of the file's list")) };
        }
        self.node(0).map(Some)
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    /// An error at the current place, with its line.
    fn error(&self, message: &str) -> ImportError {
        let before = self.text.as_bytes().get(..self.at.min(self.text.len())).unwrap_or_default();
        let line = before.iter().filter(|&&b| b == b'\n').count().saturating_add(1);
        ImportError::Syntax { line, message: message.to_owned() }
    }

    /// A word or a list, `depth` lists deep.
    fn node(&mut self, depth: usize) -> Result<Node<'a>, ImportError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(self.error("one item of the file is too large to read"));
        }
        if self.peek() != Some(b'(') {
            return match self.atom()? {
                Some(word) => Ok(Node::Atom(word)),
                None => Err(self.error("unexpected end of the file")),
            };
        }
        if depth >= MAX_DEPTH {
            return Err(self.error("lists nested too deeply"));
        }
        self.at += 1;
        let mut parts = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                None => return Err(self.error("a list is not closed")),
                Some(b')') => {
                    self.at += 1;
                    return Ok(Node::List(parts));
                }
                Some(_) => parts.push(self.node(depth + 1)?),
            }
        }
    }

    /// A bare or quoted word at the current place; `None` at a parenthesis or the end.
    fn atom(&mut self) -> Result<Option<Cow<'a, str>>, ImportError> {
        match self.peek() {
            None | Some(b'(' | b')') => Ok(None),
            Some(b'"') => self.quoted().map(Some),
            Some(_) => {
                let start = self.at;
                while self.peek().is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'(' | b')' | b'"')) {
                    self.at += 1;
                }
                // Both ends are at ASCII bytes, so the slice is on character boundaries.
                Ok(self.text.get(start..self.at).map(Cow::Borrowed))
            }
        }
    }

    /// A quoted string, with `\"`, `\\`, `\n`, `\r` and `\t` read as what they stand for.
    fn quoted(&mut self) -> Result<Cow<'a, str>, ImportError> {
        self.at += 1;
        let start = self.at;
        let mut owned: Option<String> = None;
        let mut copied = start;
        loop {
            match self.peek() {
                None => return Err(self.error("a quoted string is not closed")),
                Some(b'"') => {
                    let rest = self.text.get(copied..self.at).unwrap_or_default();
                    self.at += 1;
                    return Ok(match owned {
                        Some(mut s) => {
                            s.push_str(rest);
                            Cow::Owned(s)
                        }
                        None => Cow::Borrowed(rest),
                    });
                }
                Some(b'\\') => {
                    let s = owned.get_or_insert_with(String::new);
                    s.push_str(self.text.get(copied..self.at).unwrap_or_default());
                    let Some(escaped) = self.text.get(self.at + 1..).and_then(|rest| rest.chars().next()) else {
                        return Err(self.error("a quoted string is not closed"));
                    };
                    s.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                    self.at += 1 + escaped.len_utf8();
                    copied = self.at;
                }
                Some(_) => self.at += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(text: &str) -> Result<(String, Vec<Node<'_>>), ImportError> {
        let (mut reader, head) = Reader::open(text)?;
        let mut found = Vec::new();
        while let Some(item) = reader.next_item()? {
            found.push(item);
        }
        Ok((head.into_owned(), found))
    }

    #[test]
    fn reads_words_strings_and_lists() {
        let (head, found) = items("\u{feff}(kicad_pcb (version 20221018)\n  (net 1 \"Net-(D1-A)\") (at 1.5 -2) hide (a (b (c))))\n").expect("read");
        assert_eq!(head, "kicad_pcb");
        assert_eq!(found.len(), 5);
        let net = &found[1];
        assert_eq!((net.head(), net.number(0), net.text(1)), (Some("net"), Some(1.0), Some("Net-(D1-A)")));
        assert_eq!((found[2].number(0), found[2].number(1), found[2].number(2)), (Some(1.5), Some(-2.0), None));
        assert_eq!(found[3].word(), Some("hide"));
        assert_eq!(found[4].child("b").and_then(|b| b.child("c")).map(|c| c.args().len()), Some(0));
        assert!(found[4].child("z").is_none() && !found[4].flag("b") && found[2].text(0) == Some("1.5"));
    }

    #[test]
    fn quoted_strings_keep_what_they_say() {
        let (_, found) = items(r#"(x "a \"b\" \\ c\nd" "(not a list)" "" "é\t" (flag "yes" yes))"#).expect("read");
        let words: Vec<_> = found.iter().filter_map(Node::word).collect();
        assert_eq!(words, ["a \"b\" \\ c\nd", "(not a list)", "", "é\t"]);
        assert!(found[4].flag("yes"));
        assert_eq!(found[4].all("flag").count(), 0);
        // Numbers that are not finite are no numbers.
        let (_, found) = items("(x (n nan inf 1e999 -0.5 abc))").expect("read");
        assert_eq!((0..5).map(|i| found[0].number(i)).collect::<Vec<_>>(), [None, None, None, Some(-0.5), None]);
    }

    #[test]
    fn broken_text_is_an_error_with_its_line() {
        for (text, line) in [
            ("", 1),
            ("kicad_pcb", 1),
            ("()", 1),
            ("(a\n(b\n", 3),
            ("(a \"open", 1),
            ("(a \"open\\", 1),
            ("(a) b", 1),
            ("(a\n\n(b))\n(c)", 4),
            ("(a (b\n", 2),
        ] {
            let err = items(text).err();
            assert!(matches!(err, Some(ImportError::Syntax { line: l, .. }) if l == line), "{text:?}: {err:?}");
        }
        // Nesting and the size of one item are bounded; a hostile file is refused, not followed.
        let deep = format!("(a {}", "(".repeat(100_000));
        assert!(matches!(items(&deep), Err(ImportError::Syntax { message, .. }) if message.contains("nested")));
        let wide = format!("(a (b {}))", "x ".repeat(MAX_NODES + 1));
        assert!(matches!(items(&wide), Err(ImportError::Syntax { message, .. }) if message.contains("too large")));
        // Many small items are fine: the bound is per item.
        let many = format!("(a {})", "(b 1) ".repeat(100_000));
        assert_eq!(items(&many).map(|(_, found)| found.len()).ok(), Some(100_000));
    }
}
