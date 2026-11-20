use std::io;
use std::path::PathBuf;

fn create_path() -> io::Result<PathBuf> {
    Ok(tempfile::tempdir()?.path().to_path_buf())
}
