use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::cursor::Cursor;

pub(super) struct Message {
    pub decoded: String,
    pub range: Range<usize>,
}

pub(super) fn catalog(source: &str) -> Option<BTreeMap<Vec<String>, Message>> {
    let mut parser = Catalog {
        cursor: Cursor::new(source),
        messages: BTreeMap::new(),
    };
    parser.object(&mut Vec::new(), 0)?;
    parser.cursor.whitespace();
    (parser.cursor.at == source.len()).then_some(parser.messages)
}

struct Catalog<'s> {
    cursor: Cursor<'s>,
    messages: BTreeMap<Vec<String>, Message>,
}

impl Catalog<'_> {
    fn object(&mut self, path: &mut Vec<String>, depth: usize) -> Option<()> {
        (depth < 64).then_some(())?;
        self.cursor.expect(b'{')?;
        self.cursor.whitespace();
        if self.cursor.peek() == Some(b'}') {
            return self.cursor.expect(b'}');
        }
        let mut keys = BTreeSet::new();
        loop {
            let key = self.string()?.decoded;
            keys.insert(key.clone()).then_some(())?;
            self.cursor.whitespace();
            self.cursor.expect(b':')?;
            self.cursor.whitespace();
            path.push(key);
            self.entry(path, depth)?;
            path.pop();
            self.cursor.whitespace();
            if self.cursor.peek() != Some(b',') {
                break;
            }
            self.cursor.take()?;
            self.cursor.whitespace();
        }
        self.cursor.expect(b'}')
    }

    fn entry(&mut self, path: &mut Vec<String>, depth: usize) -> Option<()> {
        match self.cursor.peek()? {
            b'{' => self.object(path, depth + 1),
            b'"' => {
                let message = self.string()?;
                self.messages.insert(path.clone(), message);
                Some(())
            }
            _ => self.ignore_value(),
        }
    }

    fn ignore_value(&mut self) -> Option<()> {
        let rest = self.cursor.source.get(self.cursor.at..)?;
        let mut stream =
            serde_json::Deserializer::from_str(rest).into_iter::<serde::de::IgnoredAny>();
        stream.next()?.ok()?;
        self.cursor.at += stream.byte_offset();
        Some(())
    }

    fn string(&mut self) -> Option<Message> {
        let start = self.cursor.at;
        self.cursor.expect(b'"')?;
        while let Some(byte) = self.cursor.take() {
            match byte {
                b'\\' => {
                    self.cursor.take()?;
                }
                b'"' => {
                    let range = start..self.cursor.at;
                    let decoded =
                        serde_json::from_str(self.cursor.source.get(range.clone())?).ok()?;
                    return Some(Message { decoded, range });
                }
                _ => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::catalog;

    #[test]
    fn repeated_object_keys_do_not_combine_distinct_messages() {
        let source = r#"{"dialog":{"open":"Open"},"dialog":{"close":"Close"}}"#;
        assert!(catalog(source).is_none());
    }
}
