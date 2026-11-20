use std::io::{self, Read, Write};
use std::path::Path;

fn access(path: &Path, data: &mut [u8]) -> io::Result<()> {
    std::fs::File::open(path)?.read_exact(data)?;
    std::fs::File::open(path)?.flush()?;
    std::fs::OpenOptions::new().append(true).open(path)?.write_all(data)?;
    std::fs::File::create_new(path.with_extension("new"))?.write_all(data)
}
