pub(super) struct Cursor<'s> {
    pub source: &'s str,
    pub at: usize,
}

impl<'s> Cursor<'s> {
    pub fn new(source: &'s str) -> Self {
        Self { source, at: 0 }
    }

    pub fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.at).copied()
    }

    pub fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }

    pub fn take_while(&mut self, predicate: impl Fn(u8) -> bool) {
        while self.peek().is_some_and(&predicate) {
            self.at += 1;
        }
    }

    pub fn whitespace(&mut self) {
        self.take_while(|byte| byte.is_ascii_whitespace());
    }

    pub fn expect(&mut self, byte: u8) -> Option<()> {
        (self.take()? == byte).then_some(())
    }
}
