use std::io::{self, Write};

fn write_header(destination: impl Write) -> io::Result<()> {
    std::io::BufWriter::new(destination).write_all(b"header")
}
