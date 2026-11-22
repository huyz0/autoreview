use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Signature {
    pub placeholders: BTreeSet<String>,
    pub missing_other: BTreeSet<String>,
}

pub(super) fn signature(source: &str) -> Option<Signature> {
    let mut parser = Parser { source, at: 0, signature: Signature::default() };
    parser.message(false, false, 0)?;
    Some(parser.signature)
}

struct Parser<'s> {
    source: &'s str,
    at: usize,
    signature: Signature,
}

impl Parser<'_> {
    fn message(&mut self, nested: bool, plural: bool, depth: usize) -> Option<()> {
        if depth >= 64 {
            return None;
        }
        while let Some(byte) = self.peek() {
            self.at += 1;
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
        if self.take()? == b'}' {
            return Some(());
        }
        let category = self.take_until_arg_delimiter()?.trim().to_owned();
        let plural = matches!(category.as_str(), "plural" | "selectordinal");
        let selector = plural || category == "select";
        if self.take()? == b'}' {
            return (!selector).then_some(());
        }
        if selector {
            self.choices(&name, plural, depth)
        } else {
            self.style()
        }
    }

    fn choices(&mut self, name: &str, plural: bool, depth: usize) -> Option<()> {
        self.whitespace();
        if plural && self.source.get(self.at..)?.starts_with("offset:") {
            self.at += "offset:".len();
            self.whitespace();
            let offset = self.at;
            self.take_while(|byte| byte.is_ascii_digit());
            if self.at == offset {
                return None;
            }
        }
        let mut other = false;
        let mut choices = 0;
        loop {
            self.whitespace();
            if self.peek()? == b'}' {
                self.at += 1;
                break;
            }
            let start = self.at;
            self.take_while(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'{' | b'}'));
            let selector = self.source.get(start..self.at)?;
            if selector.is_empty() {
                return None;
            }
            other |= selector == "other";
            self.whitespace();
            if self.take()? != b'{' {
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
        while let Some(byte) = self.take() {
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
        if self.peek() == Some(b'\'') {
            self.at += 1;
            return;
        }
        if !matches!(self.peek(), Some(b'{' | b'}')) && !(plural && self.peek() == Some(b'#')) {
            return;
        }
        while let Some(byte) = self.take() {
            if byte == b'\'' {
                if self.peek() == Some(b'\'') {
                    self.at += 1;
                } else {
                    return;
                }
            }
        }
    }

    fn take_until_arg_delimiter(&mut self) -> Option<&str> {
        let start = self.at;
        self.take_while(|byte| !matches!(byte, b',' | b'{' | b'}'));
        matches!(self.peek(), Some(b',' | b'}')).then(|| &self.source[start..self.at])
    }

    fn take_while(&mut self, predicate: impl Fn(u8) -> bool) {
        while self.peek().is_some_and(&predicate) {
            self.at += 1;
        }
    }

    fn whitespace(&mut self) {
        self.take_while(|byte| byte.is_ascii_whitespace());
    }

    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.at).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }
}
