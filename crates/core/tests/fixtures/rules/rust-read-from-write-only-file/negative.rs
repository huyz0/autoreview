use std::io::{self, Read, Write};
use std::path::Path;

fn access(path: &Path, data: &mut Vec<u8>) -> io::Result<()> {
    std::fs::File::create(path)?.write_all(b"header")?;
    std::fs::File::open(path)?.read_to_end(data)?;
    std::fs::File::create_new(path.with_extension("new"))?.read_to_end(data)?;
    Ok(())
}
