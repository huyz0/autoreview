//! `autoreview apply` — applies a finding's suggested patch to the working
//! tree, gated by two deterministic patch-sanity checks per the plan's
//! "Patch-suggestion sanity check" section: `git apply --check` must accept
//! the patch against the current tree before it's ever actually applied,
//! and — the tree-sitter re-parse validation previously deferred at M2 time
//! — supported source files must still parse after applying the patch in
//! a scratch directory. The actual checkout is written only after both
//! checks pass.

use std::path::{Component, Path};
use std::process::Command;

use autoreview_core::parses_cleanly;

use super::history::{find_finding_in_run_reports, history_dir_for};

fn patched_file_paths(repo_root: &Path, patch_path: &Path, reverse: bool) -> anyhow::Result<Vec<String>> {
    let mut command = Command::new("git");
    command.current_dir(repo_root).args(["apply", "--numstat", "-z"]);
    if reverse {
        command.arg("--reverse");
    }
    let output = command.arg(patch_path).output()?;
    anyhow::ensure!(output.status.success(), "cannot read patch paths: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = std::str::from_utf8(&output.stdout)?;
    stdout.split_terminator('\0').map(|record| {
        let path = record.splitn(3, '\t').nth(2).ok_or_else(|| anyhow::anyhow!("invalid patch path record"))?;
        anyhow::ensure!(!path.is_empty() && Path::new(path).components().all(|part| matches!(part, Component::Normal(_))), "invalid patch path: {path}");
        Ok(path.to_owned())
    }).collect()
}

/// Outcome of trying to apply one patch to a repo — a plain enum (rather than
/// printing/exiting inline) so the sanity-check + apply logic is directly
/// unit-testable against a real git repo, independent of the id-lookup and
/// CLI-output plumbing around it.
#[derive(Debug)]
pub enum ApplyOutcome {
    Applied,
    FailedCheck { stderr: String },
    ApplyFailedAfterCheckPassed { stderr: String },
    RejectedAfterParseFailure { path: String },
}

pub fn apply_patch_to_repo(repo_root: &Path, patch: &str) -> anyhow::Result<ApplyOutcome> {
    let scratch = tempfile::tempdir()?;
    let patch_path = scratch.path().join("suggestion.patch");
    std::fs::write(&patch_path, patch)?;

    let check = Command::new("git").args(["apply", "--check"]).arg(&patch_path).current_dir(repo_root).output()?;
    if !check.status.success() {
        return Ok(ApplyOutcome::FailedCheck { stderr: String::from_utf8_lossy(&check.stderr).trim().to_string() });
    }

    let paths = patched_file_paths(repo_root, &patch_path, false)?;
    let old_paths = patched_file_paths(repo_root, &patch_path, true)?;
    let work = scratch.path().join("work");
    std::fs::create_dir(&work)?;
    let init = Command::new("git").args(["init", "--quiet", "--template="]).current_dir(&work).output()?;
    anyhow::ensure!(init.status.success(), "cannot initialize patch validation directory: {}", String::from_utf8_lossy(&init.stderr));
    let canonical_root = repo_root.canonicalize()?;
    for path in old_paths.iter().chain(&paths) {
        let source = repo_root.join(path);
        let metadata = match source.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        anyhow::ensure!(metadata.is_file() && source.canonicalize()?.starts_with(&canonical_root), "cannot validate patch path {path} as a regular repository file");
        let destination = work.join(path);
        std::fs::create_dir_all(destination.parent().unwrap())?;
        std::fs::copy(source, destination)?;
    }

    let trial = Command::new("git").args(["apply"]).arg(&patch_path).current_dir(&work).output()?;
    anyhow::ensure!(trial.status.success(), "patch validation failed: {}", String::from_utf8_lossy(&trial.stderr));
    for path in paths {
        let file = work.join(&path);
        if file.symlink_metadata().is_ok_and(|metadata| metadata.is_file()) {
            let Ok(content) = std::fs::read_to_string(&file) else { continue };
            if parses_cleanly(&file, &content) == Some(false) {
                return Ok(ApplyOutcome::RejectedAfterParseFailure { path });
            }
        }
    }

    let apply = Command::new("git").arg("apply").arg(&patch_path).current_dir(repo_root).output()?;
    if apply.status.success() {
        Ok(ApplyOutcome::Applied)
    } else {
        Ok(ApplyOutcome::ApplyFailedAfterCheckPassed { stderr: String::from_utf8_lossy(&apply.stderr).trim().to_string() })
    }
}

pub fn run_apply(repo_root: &Path, finding_id: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let finding = match find_finding_in_run_reports(&history_dir, finding_id)? {
        Some(f) => f,
        None => {
            println!("No finding with id '{finding_id}' was found in this repo's recorded run reports at {}.", history_dir.display());
            println!("`apply` only works on a finding id printed by a previous `autoreview diff` run on this machine.");
            std::process::exit(1);
        }
    };

    let Some(suggestion) = &finding.suggestion else {
        println!("Finding {finding_id} ({}) has no suggested fix.", finding.title);
        return Ok(());
    };
    let Some(patch) = &suggestion.patch else {
        println!("Finding {finding_id}'s suggestion has no patch to apply: \"{}\"", suggestion.description);
        return Ok(());
    };

    match apply_patch_to_repo(repo_root, patch)? {
        ApplyOutcome::Applied => {
            println!("Applied patch for finding {finding_id} ({}): {}", finding.title, suggestion.description);
            println!("Review the change with `git diff` before committing.");
        }
        ApplyOutcome::FailedCheck { stderr } => {
            println!("Patch for finding {finding_id} failed the sanity check (`git apply --check`) — not applying.");
            println!("This is exactly what downgrades a patch to needs-review: the suggestion no longer cleanly applies (likely the code has moved on since the review that generated it).");
            println!("{stderr}");
            std::process::exit(1);
        }
        ApplyOutcome::ApplyFailedAfterCheckPassed { stderr } => {
            println!("`git apply` failed for finding {finding_id} after the sanity checks passed:");
            println!("{stderr}");
            std::process::exit(1);
        }
        ApplyOutcome::RejectedAfterParseFailure { path } => {
            println!("Patch for finding {finding_id} would leave {path} syntactically broken (failed a tree-sitter reparse).");
            println!("This suggestion should have been marked needs-review rather than safe-autofix; not applying.");
            std::process::exit(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_test_support::init_repo;

    fn make_patch(old: &str, new: &str) -> String {
        format!(
            "diff --git a/main.go b/main.go\nindex 0000000..1111111 100644\n--- a/main.go\n+++ b/main.go\n@@ -1,3 +1,3 @@\n package main\n \n-func main() {{{old}}}\n+func main() {{{new}}}\n"
        )
    }

    #[test]
    fn applies_a_clean_patch_to_the_working_tree() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        let patch = make_patch("", " println(\"hi\") ");
        let outcome = apply_patch_to_repo(repo.path(), &patch).unwrap();
        assert!(matches!(outcome, ApplyOutcome::Applied), "expected Applied, got {outcome:?}");
        let content = std::fs::read_to_string(repo.path().join("main.go")).unwrap();
        assert!(content.contains("println(\"hi\")"), "file should contain the applied change, got:\n{content}");
    }

    #[test]
    fn a_patch_against_stale_context_fails_the_check_and_leaves_the_file_untouched() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {\n\tprintln(\"already different\")\n}\n")]);
        let patch = make_patch("", " println(\"hi\") ");
        let outcome = apply_patch_to_repo(repo.path(), &patch).unwrap();
        assert!(matches!(outcome, ApplyOutcome::FailedCheck { .. }), "expected FailedCheck, got {outcome:?}");
        let content = std::fs::read_to_string(repo.path().join("main.go")).unwrap();
        assert!(content.contains("already different"), "file must be untouched when the sanity check fails, got:\n{content}");
    }

    #[test]
    fn a_patch_that_breaks_go_preserves_the_worktree_and_index() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        // This patch applies cleanly (context matches) but replaces the
        // closing brace with an unrelated line, leaving main.go unparseable.
        let patch = "diff --git a/main.go b/main.go\nindex 0000000..1111111 100644\n--- a/main.go\n+++ b/main.go\n@@ -1,3 +1,3 @@\n package main\n \n-func main() {}\n+func main() {\n";
        let outcome = apply_patch_to_repo(repo.path(), patch).unwrap();
        assert!(matches!(outcome, ApplyOutcome::RejectedAfterParseFailure { .. }), "expected RejectedAfterParseFailure, got {outcome:?}");
        let content = std::fs::read_to_string(repo.path().join("main.go")).unwrap();
        assert_eq!(content, "package main\n\nfunc main() {}\n");
        let index = Command::new("git").args(["diff", "--cached", "--exit-code"]).current_dir(repo.path()).output().unwrap();
        assert!(index.status.success(), "failed apply changed the index");
    }
}
