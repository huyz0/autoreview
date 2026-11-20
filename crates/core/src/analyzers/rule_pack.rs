//! Shared plumbing between `ast_grep.rs`'s builtin-pack pathway and
//! `shadow_rules.rs`'s repo-local pathway — both write rule YAML files into
//! a fresh tempdir and invoke the same `ast-grep scan --config ... --json`
//! subprocess shape; this is that shared half. The tree-walk that decides
//! *which* files to write stays separate in each caller (`include_dir::Dir`
//! recursion vs. a flat `read_dir` are different enough types that forcing
//! them through one walker isn't worth it).

use std::path::Path;
use std::process::Command;

/// Writes only the ast-grep portion of an Autoreview pattern rule. Recent
/// ast-grep versions reject unknown top-level fields.
pub(crate) fn write_rule_file(dest_path: &Path, contents: &str) -> anyhow::Result<()> {
    let mut rule: serde_yaml::Mapping = serde_yaml::from_str(contents)?;
    for key in ["category", "semantic", "kind"] {
        rule.remove(serde_yaml::Value::String(key.to_string()));
    }
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest_path, serde_yaml::to_string(&rule)?)?;
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

    // Keep the combined argument size below Windows' process limit.
    // Bound both path count and length to cover quoting and command flags.
    const PATH_BUDGET: usize = 12_000;
    let mut matches = Vec::new();
    let mut batch = Vec::new();
    let mut size = 0;
    for path in relevant {
        let path_size = path.len() + 3;
        if size + path_size > PATH_BUDGET && !batch.is_empty() {
            let Some(found) = scan_batch(&sgconfig_path, repo_root, &batch)? else { return Ok(None) };
            matches.extend(found);
            batch.clear();
            size = 0;
        }
        batch.push(*path);
        size += path_size;
    }
    if !batch.is_empty() {
        let Some(found) = scan_batch(&sgconfig_path, repo_root, &batch)? else { return Ok(None) };
        matches.extend(found);
    }
    Ok(Some(matches))
}

fn scan_batch(sgconfig_path: &Path, repo_root: &Path, relevant: &[&str]) -> anyhow::Result<Option<Vec<serde_json::Value>>> {
    let output = match Command::new("ast-grep").arg("scan").arg("--config").arg(sgconfig_path).arg("--json").arg("--").args(relevant).current_dir(repo_root).output() {
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
