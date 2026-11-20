use std::io;
use std::path::{Path, PathBuf};

fn use_path() -> io::Result<()> {
    let owner = tempfile::tempdir()?;
    let directory = owner.path().to_path_buf();
    std::fs::metadata(directory)?;
    let metadata = std::fs::metadata(tempfile::NamedTempFile::new()?.path().to_path_buf())?;
    let _ = tempfile::tempdir()?.path().to_owned();
    let _ = metadata.len();
    Ok(())
}

fn select_path(path: &Path) -> io::Result<PathBuf> {
    let outside = tempfile::tempdir()?.path().join(path);
    Ok(outside)
}

fn persisted_path() -> io::Result<PathBuf> {
    Ok(tempfile::tempdir()?.keep())
}
