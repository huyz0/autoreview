//! Shared plumbing between `ast_grep.rs`'s builtin-pack pathway and
//! `shadow_rules.rs`'s repo-local pathway — both write rule YAML files into
//! a fresh tempdir and invoke the same `ast-grep scan --config ... --json`
//! subprocess shape; this is that shared half. The tree-walk that decides
//! *which* files to write stays separate in each caller (`include_dir::Dir`
//! recursion vs. a flat `read_dir` are different enough types that forcing
//! them through one walker isn't worth it).

use std::path::Path;
use std::process::Command;

/// Writes one rule file's contents to `dest_path`, creating parent
/// directories as needed.
pub(crate) fn write_rule_file(dest_path: &Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest_path, contents)?;
    Ok(())
}

/// Writes `sgconfig.yml` pointing at `temp_dir/rules` (which the caller must
/// have already populated) and invokes `ast-grep scan --json` against
/// `relevant`. Returns `Ok(None)` when the `ast-grep` binary isn't on
/// PATH — the caller's graceful-degradation signal, matching both
/// `run_ast_grep` and `run_shadow_rules`'s existing contract of returning
/// an empty finding list (not an error) in that case.
pub(crate) fn run_ast_grep_scan(temp_dir: &Path, repo_root: &Path, relevant: &[&str]) -> anyhow::Result<Option<Vec<serde_json::Value>>> {
    // sgconfig.yml must live outside `rules/` — ruleDirs scans every YAML
    // file in that directory recursively, so a config file placed inside it
    // gets misinterpreted as a rule file too.
    let sgconfig_path = temp_dir.join("sgconfig.yml");
    std::fs::write(&sgconfig_path, "ruleDirs:\n  - rules\n")?;

    let output = match Command::new("ast-grep").arg("scan").arg("--config").arg(&sgconfig_path).arg("--json").args(relevant).current_dir(repo_root).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    // ast-grep exits non-zero when error-severity rules match — that's
    // signal, not failure. Only a genuinely unparsable stdout means the
    // tool itself broke (bad rule file, crash, etc).
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str(&stdout) {
        Ok(matches) => Ok(Some(matches)),
        Err(err) => anyhow::bail!("ast-grep produced unparsable output: {err}. stderr: {}", String::from_utf8_lossy(&output.stderr)),
    }
}
