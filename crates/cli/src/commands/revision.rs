use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn repository_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git").args(["rev-parse", "--show-toplevel"]).current_dir(cwd).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(Path::new(std::str::from_utf8(&output.stdout).ok()?.trim_end_matches(['\r', '\n'])).components().collect())
}

pub(super) fn resolve_commit(repo_root: &Path, reference: &str) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--end-of-options", &format!("{reference}^{{commit}}")])
        .current_dir(repo_root)
        .output()?;
    anyhow::ensure!(output.status.success(), "could not resolve Git ref '{reference}': {}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

pub(super) fn compare_protobuf(
    repo: &Path, head_root: &Path, base: &str, head: &str, globs: &[String],
) -> anyhow::Result<Vec<autoreview_schema::AgentFinding>> {
    // Include both sides of renames so message identity survives a file move.
    let paths = git_path_value(repo, &["diff", "--name-only", "--no-renames", "-z", &format!("{base}...{head}"), "--"])?;
    let changed: Vec<_> = paths.split('\0').filter(|path| path.ends_with(".proto")).map(str::to_owned).collect();
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let snapshot = merge_base_snapshot(repo, base, head)?;
    autoreview_core::run_protobuf_policy(snapshot.path(), head_root, &changed, globs)
}

pub(super) fn compare_java_nullability(
    repo: &Path, head_root: &Path, base: &str, head: &str, paths: &[String],
) -> anyhow::Result<Vec<autoreview_schema::AgentFinding>> {
    if !paths.iter().any(|path| path.ends_with(".java")) {
        return Ok(Vec::new());
    }
    let snapshot = merge_base_snapshot(repo, base, head)?;
    Ok(autoreview_core::run_nullability_regressions(snapshot.path(), head_root, paths))
}

fn merge_base_snapshot(repo: &Path, base: &str, head: &str) -> anyhow::Result<tempfile::TempDir> {
    let merge_base = git_path_value(repo, &["merge-base", base, head])?;
    checkout_snapshot(repo, &merge_base)
}

/// External analyzers need a filesystem, and some need Git history. Share
/// the local object store but give the review its own HEAD, index and files.
pub(super) fn checkout_snapshot(repo_root: &Path, head: &str) -> anyhow::Result<tempfile::TempDir> {
    let snapshot = tempfile::tempdir()?;
    let git_dir = snapshot.path().join(".git");
    let object_format = git_path_value(repo_root, &["rev-parse", "--show-object-format"])?;
    let object_dir = git_path_value(repo_root, &["rev-parse", "--path-format=absolute", "--git-path", "objects"])?;
    let shallow_file = git_path_value(repo_root, &["rev-parse", "--path-format=absolute", "--git-path", "shallow"])?;

    let init = snapshot_git()
        .current_dir(snapshot.path())
        .args(["init", "--quiet", "--template=", &format!("--object-format={object_format}")])
        .output()?;
    anyhow::ensure!(init.status.success(), "could not initialize review checkout: {}", String::from_utf8_lossy(&init.stderr).trim());
    std::fs::write(git_dir.join("objects/info/alternates"), format!("{object_dir}\n"))?;
    match std::fs::copy(shallow_file, git_dir.join("shallow")) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }

    let checkout = snapshot_git()
        .current_dir(snapshot.path())
        .args(["-c", "core.autocrlf=false", "-c", "core.longpaths=true", "-c"])
        .arg(format!("core.hooksPath={}", git_dir.join("disabled-hooks").display()))
        .args(["checkout", "--detach", "--quiet", head, "--"])
        .output()?;
    anyhow::ensure!(checkout.status.success(), "could not check out review commit {head}: {}", String::from_utf8_lossy(&checkout.stderr).trim());
    Ok(snapshot)
}

fn git_path_value(repo_root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git").args(args).current_dir(repo_root).output()?;
    anyhow::ensure!(output.status.success(), "git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    Ok(String::from_utf8(output.stdout)?.trim_end_matches(['\r', '\n']).to_owned())
}

fn snapshot_git() -> Command {
    let mut git = Command::new("git");
    for name in ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"] {
        git.env_remove(name);
    }
    git
}
