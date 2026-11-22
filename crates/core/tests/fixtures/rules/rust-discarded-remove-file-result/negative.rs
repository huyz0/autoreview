use std::fs::remove_file as purge;

pub fn purge_cache(path: &str) -> std::io::Result<()> {
    purge(path)
}

mod dry_run {
    pub fn record(_path: &str) {}
}

pub fn plan_purge(path: &str) {
    use crate::dry_run::record as purge;
    purge(path);
}
