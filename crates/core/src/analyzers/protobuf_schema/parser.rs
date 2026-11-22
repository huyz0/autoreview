use std::{collections::BTreeMap, ops::Range};

#[derive(Default)]
pub(super) struct Message<'a> {
    pub span: Range<usize>,
    pub fields: BTreeMap<u32, Field<'a>>,
    pub reserved: Vec<(u32, u32)>,
}

pub(super) struct Field<'a> {
    pub span: Range<usize>,
    pub name: &'a str,
    pub kind: String,
    pub label: &'a str,
    pub oneof: Option<&'a str>,
}

type Token<'a> = (&'a str, usize);

pub(super) fn parse(source: &str) -> Option<BTreeMap<String, Message<'_>>> {
    let tokens = lex(source);
    let mut parser = Parser {
        tokens: &tokens,
        index: 0,
    };
    let mut package = String::new();
    let mut messages = BTreeMap::new();
    while let Some(token) = parser.peek() {
        match token {
            "package" => {
                parser.take()?;
                package = parser.dotted()?.trim_start_matches('.').to_owned();
                parser.expect(";")?;
            }
            "message" => parser.message(&package, &mut messages, 0)?,
            _ => parser.skip_statement()?,
        }
    }
    Some(messages)
}

struct Parser<'a, 't> {
    tokens: &'t [Token<'a>],
    index: usize,
}

impl<'a> Parser<'a, '_> {
    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.index).map(|(text, _)| *text)
    }

    fn take(&mut self) -> Option<Token<'a>> {
        let token = *self.tokens.get(self.index)?;
        self.index += 1;
        Some(token)
    }

    fn expect(&mut self, expected: &str) -> Option<usize> {
        let (token, position) = self.take()?;
        (token == expected).then_some(position)
    }

    fn consume(&mut self, expected: &str) -> bool {
        if self.peek() != Some(expected) {
            return false;
        }
        self.index += 1;
        true
    }

    fn name(&mut self) -> Option<&'a str> {
        let (name, _) = self.take()?;
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            .then_some(name)
    }

    fn dotted(&mut self) -> Option<String> {
        let mut name = String::new();
        if self.consume(".") {
            name.push('.');
        }
        name.push_str(self.name()?);
        while self.consume(".") {
            name.push('.');
            name.push_str(self.name()?);
        }
        Some(name)
    }

    fn message(
        &mut self,
        owner: &str,
        messages: &mut BTreeMap<String, Message<'a>>,
        depth: usize,
    ) -> Option<()> {
        if depth >= 128 {
            return None;
        }
        let start = self.expect("message")?;
        let name_start = self.tokens.get(self.index)?.1;
        let name = self.name()?;
        let qualified = if owner.is_empty() {
            name.to_owned()
        } else {
            format!("{owner}.{name}")
        };
        self.expect("{")?;
        let mut message = Message {
            span: start..name_start + name.len(),
            ..Default::default()
        };
        while self.peek()? != "}" {
            match self.peek()? {
                "message" => self.message(&qualified, messages, depth + 1)?,
                "reserved" => self.reserved(&mut message.reserved)?,
                "oneof" => self.oneof(&mut message)?,
                "enum" | "option" | "extend" | "extensions" | ";" => self.skip_statement()?,
                _ => self.field(&mut message, None)?,
            }
        }
        self.expect("}")?;
        if messages.insert(qualified, message).is_some() {
            return None;
        }
        Some(())
    }

    fn oneof(&mut self, message: &mut Message<'a>) -> Option<()> {
        self.expect("oneof")?;
        let group = self.name()?;
        self.expect("{")?;
        while self.peek()? != "}" {
            match self.peek()? {
                "option" | ";" => self.skip_statement()?,
                _ => self.field(message, Some(group))?,
            }
        }
        self.expect("}")?;
        Some(())
    }

    fn reserved(&mut self, ranges: &mut Vec<(u32, u32)>) -> Option<()> {
        self.expect("reserved")?;
        loop {
            let (value, _) = self.take()?;
            if let Some(start) = number(value) {
                let end = if self.consume("to") {
                    let (end, _) = self.take()?;
                    if end == "max" {
                        536_870_911
                    } else {
                        number(end)?
                    }
                } else {
                    start
                };
                ranges.push((start, end));
            }
            if !self.consume(",") {
                break;
            }
        }
        self.expect(";")?;
        Some(())
    }

    fn field(&mut self, message: &mut Message<'a>, oneof: Option<&'a str>) -> Option<()> {
        let (first, start) = *self.tokens.get(self.index)?;
        let label = match first {
            "required" | "optional" | "repeated" => self.take()?.0,
            _ => "",
        };
        let kind = self.field_type()?;
        let name = self.name()?;
        self.expect("=")?;
        let tag = number(self.take()?.0)?;
        if self.peek() == Some("[") {
            self.skip_balanced()?;
        }
        let end = self.expect(";")? + 1;
        let field = Field {
            span: start..end,
            name,
            kind,
            label,
            oneof,
        };
        message.fields.insert(tag, field).is_none().then_some(())
    }

    fn field_type(&mut self) -> Option<String> {
        if self.peek() == Some("map") && self.tokens.get(self.index + 1)?.0 == "<" {
            self.expect("map")?;
            self.expect("<")?;
            let key = self.dotted()?;
            self.expect(",")?;
            let value = self.dotted()?;
            self.expect(">")?;
            return Some(format!("map<{key},{value}>"));
        }
        self.dotted()
    }

    fn skip_statement(&mut self) -> Option<()> {
        let is_block = matches!(self.peek()?, "enum" | "service" | "extend");
        while let Some(next) = self.peek() {
            match next {
                ";" => {
                    self.take()?;
                    return Some(());
                }
                "{" if is_block => return self.skip_balanced(),
                "{" | "[" | "(" | "<" => self.skip_balanced()?,
                "}" => return None,
                _ => {
                    self.take()?;
                }
            }
        }
        None
    }

    fn skip_balanced(&mut self) -> Option<()> {
        let (opening, _) = self.take()?;
        let mut closing = vec![closing_token(opening)?];
        while let Some(expected) = closing.last() {
            let (token, _) = self.take()?;
            if token == *expected {
                closing.pop();
            } else if let Some(nested) = closing_token(token) {
                closing.push(nested);
            }
        }
        Some(())
    }
}

fn closing_token(token: &str) -> Option<&'static str> {
    match token {
        "{" => Some("}"),
        "[" => Some("]"),
        "(" => Some(")"),
        "<" => Some(">"),
        _ => None,
    }
}

fn number(value: &str) -> Option<u32> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else if value.len() > 1 && value.starts_with('0') {
        u32::from_str_radix(value, 8).ok()
    } else {
        value.parse().ok()
    }
}

fn lex(mut source: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut offset = 0;
    while !source.is_empty() {
        let trimmed = source.trim_start();
        offset += source.len() - trimmed.len();
        let Some(first) = trimmed.chars().next() else {
            break;
        };
        let (length, comment) = token_length(trimmed, first);
        if !comment {
            tokens.push((&trimmed[..length], offset));
        }
        source = &trimmed[length..];
        offset += length;
    }
    tokens
}

fn token_length(source: &str, first: char) -> (usize, bool) {
    if source.starts_with("//") {
        return (source.find('\n').unwrap_or(source.len()), true);
    }
    if let Some(comment) = source.strip_prefix("/*") {
        return (comment.find("*/").map_or(source.len(), |end| end + 4), true);
    }
    if matches!(first, '"' | '\'') {
        return (quoted_length(source, first), false);
    }
    if first.is_ascii_alphanumeric() || first == '_' {
        let end = source
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(source.len());
        return (end, false);
    }
    (first.len_utf8(), false)
}

fn quoted_length(source: &str, quote: char) -> usize {
    let mut escaped = false;
    for (index, ch) in source.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return index + 1;
        }
    }
    source.len()
}
