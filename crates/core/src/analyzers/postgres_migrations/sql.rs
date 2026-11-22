use std::ops::Range;

pub(super) fn concurrent_indexes(source: &str) -> Vec<Range<usize>> {
    let mut tokens = Tokens { source, at: 0 };
    let mut prefix = Vec::new();
    let mut findings = Vec::new();
    for token in &mut tokens {
        if token.text == ";" {
            prefix.clear();
        } else if prefix.len() < 4 {
            let is_concurrent = token.text.eq_ignore_ascii_case("concurrently");
            if is_concurrent && index_prefix(&prefix) {
                findings.push(prefix[0].start..token.end);
            }
            prefix.push(token);
        }
    }
    findings
}

fn index_prefix(prefix: &[Token<'_>]) -> bool {
    let matches = |token: &Token<'_>, keyword: &str| token.text.eq_ignore_ascii_case(keyword);
    match prefix {
        [create, index] => matches(create, "create") && matches(index, "index"),
        [create, unique, index] => {
            matches(create, "create") && matches(unique, "unique") && matches(index, "index")
        }
        _ => false,
    }
}

struct Token<'s> {
    text: &'s str,
    start: usize,
    end: usize,
}

struct Tokens<'s> {
    source: &'s str,
    at: usize,
}

impl<'s> Iterator for Tokens<'s> {
    type Item = Token<'s>;

    fn next(&mut self) -> Option<Self::Item> {
        self.skip_trivia();
        let start = self.at;
        let rest = self.source.get(start..)?;
        let byte = *rest.as_bytes().first()?;
        let length = match byte {
            b'\'' | b'"' => quoted(rest.as_bytes(), 0, false),
            b'e' | b'E' if rest.as_bytes().get(1) == Some(&b'\'') => quoted(rest.as_bytes(), 1, true),
            b'$' => dollar_string(rest).unwrap_or(1),
            byte if word(byte) => rest.bytes().take_while(|byte| word(*byte)).count(),
            _ => rest.chars().next()?.len_utf8(),
        };
        self.at += length;
        Some(Token { text: &self.source[start..self.at], start, end: self.at })
    }
}

impl Tokens<'_> {
    fn skip_trivia(&mut self) {
        loop {
            let rest = &self.source[self.at..];
            if rest.starts_with("--") {
                self.at += rest.find('\n').unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                self.at += block_comment(rest.as_bytes());
            } else {
                let blanks = rest.bytes().take_while(u8::is_ascii_whitespace).count();
                if blanks == 0 {
                    break;
                }
                self.at += blanks;
            }
        }
    }
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

fn block_comment(bytes: &[u8]) -> usize {
    let mut depth = 1;
    let mut at = 2;
    while let Some(pair) = bytes.get(at..at + 2) {
        match pair {
            b"/*" => { depth += 1; at += 2; }
            b"*/" => {
                depth -= 1;
                at += 2;
                if depth == 0 {
                    return at;
                }
            }
            _ => at += 1,
        }
    }
    bytes.len()
}

fn quoted(bytes: &[u8], start: usize, escapes: bool) -> usize {
    let quote = bytes[start];
    let mut at = start + 1;
    while let Some(&byte) = bytes.get(at) {
        at += 1;
        if escapes && byte == b'\\' {
            at += 1;
        } else if byte == quote {
            if bytes.get(at) != Some(&quote) {
                return at;
            }
            at += 1;
        }
    }
    bytes.len()
}

fn dollar_string(source: &str) -> Option<usize> {
    let delimiter_end = source[1..].find('$')? + 2;
    let tag = &source[1..delimiter_end - 1];
    if tag.starts_with(|ch: char| ch.is_ascii_digit())
        || !tag.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    let delimiter = &source[..delimiter_end];
    let after = &source[delimiter_end..];
    Some(after.find(delimiter).map_or(source.len(), |offset| delimiter_end * 2 + offset))
}

#[cfg(test)]
mod tests {
    use super::concurrent_indexes;

    #[test]
    fn skips_comments_and_quoted_bodies() {
        let source = r#"
-- CREATE INDEX CONCURRENTLY comment ON gone (col);
DO $ddl$ BEGIN RAISE NOTICE 'CREATE INDEX CONCURRENTLY'; END $ddl$;
SELECT E'a\'b; create index concurrently';
CREATE /* outer /* nested */ still outer */ INDEX idx ON things (n);
CREATE UNIQUE INDEX
  CONCURRENTLY idx2 ON things (n);
"#;
        let findings = concurrent_indexes(source);
        assert_eq!(findings.len(), 1);
        assert_eq!(&source[findings[0].clone()], "CREATE UNIQUE INDEX\n  CONCURRENTLY");
    }
}
