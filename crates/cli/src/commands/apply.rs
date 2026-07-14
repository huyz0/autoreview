//! `autoreview apply` — applies a finding's suggested patch to the working
//! tree, gated by two deterministic patch-sanity checks per the plan's
//! "Patch-suggestion sanity check" section: `git apply --check` must accept
//! the patch against the current tree before it's ever actually applied,
//! and — the tree-sitter re-parse validation previously deferred at M2 time
//! — every Go/Java file the patch touches must still parse cleanly
//! afterward. `--check` catches a patch that's gone stale (the far more
//! common failure); the reparse catches the narrower case of a patch that
//! applies but leaves the file syntactically broken (a missing brace, an
//! unterminated string). A reparse failure auto-reverts the apply (`git
//! apply -R`) rather than leaving broken code in the working tree.

use std::path::Path;
use std::process::Command;

use autoreview_core::parses_cleanly;
use autoreview_schema::Finding;

use super::history::history_dir_for;

/// Extracts the repo-relative paths a unified diff touches, from its
/// `+++ b/<path>` header lines — good enough for the reparse check, which
/// only needs to know which files to re-read after applying.
fn patched_file_paths(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|line| line.strip_prefix("+++ b/"))
        .filter(|p| *p != "/dev/null")
        .map(str::to_string)
        .collect()
}

/// Searches this repo's recorded run reports (newest first) for a finding
/// with the given id — findings (and their suggestion patches) live in the
/// per-run `report.json` artifacts, not the SQLite index, which only stores
/// enough metadata to resolve an id back to its fingerprint (see
/// `HistoryStore::find_finding_by_id`).
fn find_finding_in_run_reports(history_dir: &Path, finding_id: &str) -> anyhow::Result<Option<Finding>> {
    let runs_dir = history_dir.join("runs");
    let mut run_dirs: Vec<_> = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return Ok(None),
    };
    run_dirs.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    run_dirs.reverse();

    for run_dir in run_dirs {
        let report_path = run_dir.join("report.json");
        let Ok(text) = std::fs::read_to_string(&report_path) else { continue };
        let Ok(report) = serde_json::from_str::<autoreview_schema::ReviewReport>(&text) else { continue };
        if let Some(finding) = report.findings.into_iter().find(|f| f.id == finding_id) {
            return Ok(Some(finding));
        }
    }
    Ok(None)
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
    /// The patch applied cleanly, but left a Go/Java file syntactically
    /// broken — reverted automatically, so the working tree is exactly as
    /// it was before `apply` ran.
    RevertedAfterParseFailure { path: String },
}

/// Writes `patch` to a scratch file and runs the two-step sanity-checked
/// apply: `git apply --check` must pass before anything touches the working
/// tree, then `git apply --3way` performs the real, mergeable apply.
pub fn apply_patch_to_repo(repo_root: &Path, patch: &str) -> anyhow::Result<ApplyOutcome> {
    let patch_path = std::env::temp_dir().join(format!("autoreview-apply-{}.patch", uuid::Uuid::new_v4()));
    std::fs::write(&patch_path, patch)?;
    let cleanup = || {
        let _ = std::fs::remove_file(&patch_path);
    };

    let check = Command::new("git").args(["apply", "--check"]).arg(&patch_path).current_dir(repo_root).output()?;
    if !check.status.success() {
        cleanup();
        return Ok(ApplyOutcome::FailedCheck { stderr: String::from_utf8_lossy(&check.stderr).trim().to_string() });
    }

    let apply = Command::new("git").args(["apply", "--3way"]).arg(&patch_path).current_dir(repo_root).output()?;
    if !apply.status.success() {
        cleanup();
        return Ok(ApplyOutcome::ApplyFailedAfterCheckPassed { stderr: String::from_utf8_lossy(&apply.stderr).trim().to_string() });
    }

    for relative_path in patched_file_paths(patch) {
        let full_path = repo_root.join(&relative_path);
        let Ok(content) = std::fs::read_to_string(&full_path) else { continue };
        if parses_cleanly(&full_path, &content) == Some(false) {
            let _ = Command::new("git").args(["apply", "-R"]).arg(&patch_path).current_dir(repo_root).status();
            cleanup();
            return Ok(ApplyOutcome::RevertedAfterParseFailure { path: relative_path });
        }
    }

    cleanup();
    Ok(ApplyOutcome::Applied)
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
            println!("`git apply --3way` failed for finding {finding_id} even though `--check` passed (unexpected):");
            println!("{stderr}");
            std::process::exit(1);
        }
        ApplyOutcome::RevertedAfterParseFailure { path } => {
            println!("Patch for finding {finding_id} applied cleanly but left {path} syntactically broken (failed a tree-sitter reparse) — reverted automatically.");
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
    fn a_patch_that_leaves_go_syntactically_broken_is_reverted_automatically() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        // This patch applies cleanly (context matches) but replaces the
        // closing brace with an unrelated line, leaving main.go unparseable.
        let patch = "diff --git a/main.go b/main.go\nindex 0000000..1111111 100644\n--- a/main.go\n+++ b/main.go\n@@ -1,3 +1,3 @@\n package main\n \n-func main() {}\n+func main() {\n";
        let outcome = apply_patch_to_repo(repo.path(), patch).unwrap();
        assert!(matches!(outcome, ApplyOutcome::RevertedAfterParseFailure { .. }), "expected RevertedAfterParseFailure, got {outcome:?}");
        let content = std::fs::read_to_string(repo.path().join("main.go")).unwrap();
        assert_eq!(content, "package main\n\nfunc main() {}\n", "file must be back to its original content after the auto-revert");
    }

    #[test]
    fn patched_file_paths_extracts_repo_relative_paths_from_a_unified_diff() {
        let patch = "diff --git a/a/main.go b/a/main.go\n--- a/a/main.go\n+++ b/a/main.go\n@@ -1 +1 @@\n-x\n+y\n";
        assert_eq!(patched_file_paths(patch), vec!["a/main.go".to_string()]);
    }

    #[test]
    fn patched_file_paths_skips_dev_null_for_deleted_files() {
        let patch = "diff --git a/gone.go b/gone.go\n--- a/gone.go\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        assert!(patched_file_paths(patch).is_empty());
    }
}
