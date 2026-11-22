use std::collections::BTreeMap;
use std::ops::Range;

pub(super) struct Message {
    pub decoded: String,
    pub range: Range<usize>,
}

pub(super) fn catalog(source: &str) -> Option<BTreeMap<Vec<String>, Message>> {
    let mut parser = Catalog { source, at: 0, messages: BTreeMap::new() };
    parser.object(&mut Vec::new(), 0)?;
    parser.skip_space();
    (parser.at == source.len()).then_some(parser.messages)
}

struct Catalog<'s> {
    source: &'s str,
    at: usize,
    messages: BTreeMap<Vec<String>, Message>,
}

impl Catalog<'_> {
    fn object(&mut self, path: &mut Vec<String>, depth: usize) -> Option<()> {
        if depth >= 64 || self.take()? != b'{' {
            return None;
        }
        self.skip_space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Some(());
        }
        loop {
            let key = self.string()?;
            if self.take()? != b':' {
                return None;
            }
            self.skip_space();
            path.push(key.decoded);
            match self.peek()? {
                b'{' => self.object(path, depth + 1)?,
                b'"' => {
                    let message = self.string()?;
                    if self.messages.insert(path.clone(), message).is_some() {
                        return None;
                    }
                }
                _ => {
                    let mut stream = serde_json::Deserializer::from_str(self.source.get(self.at..)?)
                        .into_iter::<serde::de::IgnoredAny>();
                    stream.next()?.ok()?;
                    self.at += stream.byte_offset();
                }
            }
            path.pop();
            match self.take()? {
                b',' => {}
                b'}' => return Some(()),
                _ => return None,
            }
        }
    }

    fn string(&mut self) -> Option<Message> {
        self.skip_space();
        let start = self.at;
        if self.take()? != b'"' {
            return None;
        }
        while let Some(byte) = self.peek() {
            self.at += 1;
            match byte {
                b'\\' => { self.at += 1; }
                b'"' => {
                    let range = start..self.at;
                    let decoded = serde_json::from_str(self.source.get(range.clone())?).ok()?;
                    return Some(Message { decoded, range });
                }
                _ => {}
            }
        }
        None
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.at).copied()
    }

    fn take(&mut self) -> Option<u8> {
        self.skip_space();
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }
}
