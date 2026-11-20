use std::io::{self, Write};
use std::path::Path;

fn write_header(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.write_all(b"header")
}
