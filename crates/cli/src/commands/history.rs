//! Shared repo-history location logic (fingerprint, cache dir, hostname) —
//! used by both `diff` (writes history) and `feedback` (reads + appends to
//! it), so the two commands agree on where history lives without duplicating
//! the derivation.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

pub fn repo_fingerprint(repo_root: &Path, remote_url: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(remote_url.unwrap_or(&repo_root.to_string_lossy()).as_bytes());
    let digest = hasher.finalize();
    format!("{digest:x}")[..16].to_string()
}

pub fn resolve_remote_url(repo_root: &Path) -> Option<String> {
    let output = Command::new("git").args(["remote", "get-url", "origin"]).current_dir(repo_root).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn history_dir_for(repo_root: &Path) -> PathBuf {
    let remote_url = resolve_remote_url(repo_root);
    let fingerprint = repo_fingerprint(repo_root, remote_url.as_deref());
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("autoreview").join(fingerprint)
}

pub fn hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    if let Ok(output) = Command::new("hostname").output() {
        if output.status.success() {
            let h = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
    }
    "unknown-host".to_string()
}
