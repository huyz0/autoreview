use std::io::{self, Write};

fn write_header(destination: impl Write) -> io::Result<()> {
    let mut writer = std::io::BufWriter::new(destination);
    writer.write_all(b"header")?;
    writer.flush()
}

fn no_buffer(destination: impl Write) -> io::Result<()> {
    std::io::BufWriter::with_capacity(0, destination).write_all(b"header")
}
