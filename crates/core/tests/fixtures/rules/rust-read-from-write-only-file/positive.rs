use std::io::{self, Read};
use std::path::Path;

fn read_header(path: &Path, data: &mut [u8]) -> io::Result<()> {
    std::fs::File::create(path).expect("open the data file").read_exact(data)
}
