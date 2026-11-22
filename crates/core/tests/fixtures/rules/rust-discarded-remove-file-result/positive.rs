use std::{fs::remove_file as erase, path::Path};

pub fn remove_stale(path: &Path) {
    let _ = erase(path);
}
