use std::io;
use std::path::{Path, PathBuf};

fn report_paths(other: &Path) -> io::Result<()> {
    let owner = tempfile::tempdir()?;
    let _retained = owner.path().join("report.json");
    let _persisted = tempfile::tempdir()?.keep().join("report.json");
    let _absolute = tempfile::tempdir()?.path().join("/srv/report.json");
    let _prefixed = tempfile::tempdir()?.path().join(r"C:\report.json");
    let _relative_to_drive = tempfile::tempdir()?.path().join("C:report.json");
    let _parent = tempfile::tempdir()?.path().join("..");
    let _nested = tempfile::tempdir()?.path().join("nested/report.json");
    let _device = tempfile::tempdir()?.path().join("NUL.txt");
    let _dynamic = tempfile::tempdir()?.path().join(other);
    let _file = tempfile::NamedTempFile::new()?.path().join("report.json");
    let _contents = std::fs::read(tempfile::tempdir()?.path().join("report.json"))?;
    let _ = tempfile::tempdir()?.path().join("report.json");
    Ok(())
}

fn snapshot() -> io::Result<PathBuf> {
    Ok(tempfile::tempdir()?.path().to_path_buf())
}
