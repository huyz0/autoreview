//! Self-improving rule factory (M3): mine recurring agent findings (or,
//! opt-in, recurring human PR review comments via `mine_from_comments`)
//! into deterministic-rule candidates, then draft, bench, shadow, and
//! promote them. See `mine`'s submodule docs for the clustering algorithm
//! and `CandidateSeed`'s docs for exactly what "recurring" means here.

pub mod bench;
pub mod category_heuristics;
pub mod config_gap_report;
pub mod draft;
pub mod existing_rules;
pub mod mine;
pub mod mine_from_bitbucket_comments;
pub mod mine_from_bugfix_commits;
pub mod mine_from_code;
pub mod mine_from_comments;
pub mod mine_from_llm_patterns;
pub mod mine_from_suppressions;
pub mod shadow;

/// Verifies an identifier before using it as a candidate directory or rule filename.
pub fn validate_rule_path_id(id: &str) -> anyhow::Result<()> {
    let upper = id.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper.strip_prefix("COM").or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|tail| tail.len() == 1 && tail.as_bytes()[0].is_ascii_digit());
    anyhow::ensure!(
        !device && id.len() <= 128
            && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
            && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "rule or candidate ID must be a portable file name made of letters, digits, hyphens or underscores"
    );
    Ok(())
}

/// Creates or opens each directory of the repository-local rule store.
/// A symlink cannot redirect writes outside the checkout.
pub fn rule_store_directory(repo: &std::path::Path, parts: &[&str]) -> anyhow::Result<std::path::PathBuf> {
    let mut directory = repo.canonicalize()?.join(".autoreview");
    let mut next = std::iter::once("rules").chain(parts.iter().copied());
    loop {
        match std::fs::create_dir(&directory) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err.into()),
        }
        anyhow::ensure!(directory.symlink_metadata()?.is_dir(), "{} is not a regular rule store directory", directory.display());
        let Some(part) = next.next() else { return Ok(directory) };
        validate_rule_path_id(part)?;
        directory.push(part);
    }
}
