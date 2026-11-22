use std::collections::BTreeSet;

use super::cursor::Cursor;

#[derive(Default)]
pub(super) struct Signature {
    pub placeholders: BTreeSet<String>,
    pub missing_other: BTreeSet<String>,
}

pub(super) fn signature(source: &str) -> Option<Signature> {
    let mut parser = Parser {
        cursor: Cursor::new(source),
        signature: Signature::default(),
    };
    parser.message(false, false, 0)?;
    Some(parser.signature)
}

struct Parser<'s> {
    cursor: Cursor<'s>,
    signature: Signature,
}

impl Parser<'_> {
    fn message(&mut self, nested: bool, plural: bool, depth: usize) -> Option<()> {
        if depth >= 64 {
            return None;
        }
        while let Some(byte) = self.cursor.peek() {
            self.cursor.at += 1;
            match byte {
                b'{' => self.argument(depth)?,
                b'}' if nested => return Some(()),
                b'}' => return None,
                b'\'' => self.apostrophe(plural),
                _ => {}
            }
        }
        (!nested).then_some(())
    }

    fn argument(&mut self, depth: usize) -> Option<()> {
        let name = self.take_until_arg_delimiter()?.trim();
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return None;
        }
        let name = name.to_owned();
        self.signature.placeholders.insert(name.clone());
        if self.cursor.take()? == b'}' {
            return Some(());
        }
        let category = self.take_until_arg_delimiter()?.trim().to_owned();
        let plural = matches!(category.as_str(), "plural" | "selectordinal");
        let selector = plural || category == "select";
        if self.cursor.take()? == b'}' {
            return (!selector).then_some(());
        }
        if selector {
            self.choices(&name, plural, depth)
        } else {
            self.style()
        }
    }

    fn choices(&mut self, name: &str, plural: bool, depth: usize) -> Option<()> {
        self.cursor.whitespace();
        if plural
            && self
                .cursor
                .source
                .get(self.cursor.at..)?
                .starts_with("offset:")
        {
            self.cursor.at += "offset:".len();
            self.cursor.whitespace();
            let offset = self.cursor.at;
            self.cursor.take_while(|byte| byte.is_ascii_digit());
            if self.cursor.at == offset {
                return None;
            }
        }
        let mut other = false;
        let mut choices = 0;
        loop {
            self.cursor.whitespace();
            if self.cursor.peek()? == b'}' {
                self.cursor.at += 1;
                break;
            }
            let start = self.cursor.at;
            self.cursor
                .take_while(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'{' | b'}'));
            let selector = self.cursor.source.get(start..self.cursor.at)?;
            if selector.is_empty() {
                return None;
            }
            other |= selector == "other";
            self.cursor.whitespace();
            if self.cursor.take()? != b'{' {
                return None;
            }
            self.message(true, plural, depth + 1)?;
            choices += 1;
        }
        if !other {
            self.signature.missing_other.insert(name.to_owned());
        }
        (choices > 0).then_some(())
    }

    fn style(&mut self) -> Option<()> {
        while let Some(byte) = self.cursor.take() {
            match byte {
                b'}' => return Some(()),
                b'{' => return None,
                b'\'' => self.apostrophe(false),
                _ => {}
            }
        }
        None
    }

    fn apostrophe(&mut self, plural: bool) {
        if self.cursor.peek() == Some(b'\'') {
            self.cursor.at += 1;
            return;
        }
        if !matches!(self.cursor.peek(), Some(b'{' | b'}'))
            && !(plural && self.cursor.peek() == Some(b'#'))
        {
            return;
        }
        while let Some(byte) = self.cursor.take() {
            if byte == b'\'' {
                if self.cursor.peek() == Some(b'\'') {
                    self.cursor.at += 1;
                } else {
                    return;
                }
            }
        }
    }

    fn take_until_arg_delimiter(&mut self) -> Option<&str> {
        let start = self.cursor.at;
        self.cursor
            .take_while(|byte| !matches!(byte, b',' | b'{' | b'}'));
        matches!(self.cursor.peek(), Some(b',' | b'}'))
            .then(|| &self.cursor.source[start..self.cursor.at])
    }
}
