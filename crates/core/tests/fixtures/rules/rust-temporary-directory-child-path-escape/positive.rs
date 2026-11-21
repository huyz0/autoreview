use std::io;
use std::path::PathBuf;

fn report_path() -> io::Result<PathBuf> {
    Ok(tempfile::tempdir()?.path().join("report.json"))
}
