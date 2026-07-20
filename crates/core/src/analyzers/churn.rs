//! Divergent Change / Shotgun Surgery detection (session follow-up #4).
//! Both are commit-history smells, not structural ones — `symindex`'s
//! tree-sitter model has no notion of "this file across time," so this
//! lives in `core` instead, alongside `context/mod.rs`'s existing
//! `git log` shell-out (same `Command::new("git")` pattern, no new
//! dependency).
//!
//! - **Shotgun Surgery**: a single change forced into many small edits
//!   scattered across many files. Detected from the *current* diff alone —
//!   `DiffFacts`/`FileChange` already has per-file added/deleted line
//!   counts, so this is a diff-shape heuristic (many files, each touched
//!   only a little), not a history walk.
//! - **Divergent Change**: one file that gets modified for many unrelated
//!   reasons over time. Detected by walking one changed file's commit
//!   history and looking at which *other* directories were touched
//!   alongside it in each commit — if those co-change partners are
//!   scattered across many distinct directories with no single dominant
//!   collaborator, that's a sign that whatever cause is behind each edit,
//!   it's a different one every time. Deliberately syntactic/heuristic
//!   (directory-level, not semantic "reason" clustering) per the project's
//!   precision-over-recall philosophy — a human still has to confirm the
//!   file really does mix unrelated responsibilities.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

use crate::triage::signals::FileChange;

const SHOTGUN_MIN_FILES: usize = 8;
const SHOTGUN_MAX_LINES_PER_FILE: u32 = 5;

const DIVERGENT_MIN_COMMITS: usize = 6;
const DIVERGENT_MIN_DISTINCT_DIRS: usize = 4;
/// No single co-change partner directory may account for more than this
/// fraction of the file's commits — otherwise there's one dominant,
/// legitimate collaborator, not diffuse/unrelated coupling.
const DIVERGENT_MAX_DOMINANT_SHARE: f64 = 0.5;
const DIVERGENT_LOG_LIMIT: usize = 30;
/// `divergent_change_for_file` pays at least one `git log --follow`
/// subprocess per file, plus one `git show` per commit it finds (up to
/// `DIVERGENT_LOG_LIMIT` more each) — unlike the language-scoped gates
/// elsewhere in Stage 1, this cost is language-agnostic (co-change history
/// applies to any file) so it can't be pre-filtered by language the way
/// `ast_grep.rs`'s rule pack is. What it CAN be bounded by is the sheer
/// number of changed files: an unusually large diff (a vendored-dependency
/// bump, a mass rename) would otherwise spawn a subprocess tree proportional
/// to `changed_files.len() * DIVERGENT_LOG_LIMIT`. Capping to the first N
/// changed files keeps the worst case bounded without touching the common
/// case (the vast majority of diffs stay well under this).
const DIVERGENT_MAX_FILES_PER_RUN: usize = 25;

fn make_finding(rule_id: &str, category: &str, severity: Severity, path: &str, title: String, message: String, related: Vec<Location>) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-churn".to_string(), rule_id: Some(rule_id.to_string()), aspect: None, backend: None },
        category: category.to_string(),
        severity,
        confidence: 1.0,
        title,
        message,
        location: Location { path: path.to_string(), range: LocationRange::default(), snippet: String::new(), side: Side::New },
        related_locations: if related.is_empty() { None } else { Some(related) },
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// Flags the diff as a whole (one finding, anchored on the first file with
/// the rest listed as related locations) when it spans many files and no
/// single file carries a substantial change — the shape of "the same tiny
/// edit, copy-pasted across the codebase" that Shotgun Surgery describes.
pub fn detect_shotgun_surgery(files: &[FileChange]) -> Vec<AgentFinding> {
    if files.len() < SHOTGUN_MIN_FILES {
        return Vec::new();
    }
    let all_small = files.iter().all(|f| f.additions + f.deletions <= SHOTGUN_MAX_LINES_PER_FILE);
    if !all_small {
        return Vec::new();
    }

    let Some(first) = files.first() else { return Vec::new() };
    let related = files[1..]
        .iter()
        .map(|f| Location { path: f.path.clone(), range: LocationRange::default(), snippet: String::new(), side: Side::New })
        .collect();

    vec![make_finding(
        "shotgun-surgery",
        "design",
        Severity::Medium,
        &first.path,
        format!("Shotgun Surgery: {} files each changed by only a few lines", files.len()),
        "This diff makes the same small edit across a large number of files, which usually means one concept is duplicated everywhere instead of centralized in one place. Consider whether this change is better expressed as a single edit behind a shared abstraction.".to_string(),
        related,
    )]
}

fn run_git(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).current_dir(repo_root).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn top_level_dir(path: &str) -> String {
    match path.split('/').next() {
        Some(dir) if path.contains('/') => dir.to_string(),
        _ => "<root>".to_string(),
    }
}

/// One changed file's commit history, clustered by co-change partner
/// directory. Returns `None` when there isn't enough history to judge
/// (a brand-new or lightly-touched file isn't evidence of anything).
fn divergent_change_for_file(repo_root: &Path, path: &str) -> Option<AgentFinding> {
    let log = run_git(repo_root, &["log", "--follow", &format!("-{DIVERGENT_LOG_LIMIT}"), "--pretty=format:%H", "--", path])?;
    let commits: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();
    if commits.len() < DIVERGENT_MIN_COMMITS {
        return None;
    }

    let own_dir = top_level_dir(path);
    let mut dir_commit_counts: HashMap<String, usize> = HashMap::new();
    let mut usable_commits = 0usize;

    for commit in &commits {
        let Some(names) = run_git(repo_root, &["show", "--name-only", "--pretty=format:", commit]) else { continue };
        let dirs: HashSet<String> = names.lines().filter(|l| !l.is_empty()).map(top_level_dir).filter(|d| *d != own_dir).collect();
        if dirs.is_empty() {
            continue;
        }
        usable_commits += 1;
        for dir in dirs {
            *dir_commit_counts.entry(dir).or_insert(0) += 1;
        }
    }

    if usable_commits < DIVERGENT_MIN_COMMITS || dir_commit_counts.len() < DIVERGENT_MIN_DISTINCT_DIRS {
        return None;
    }

    let max_share = dir_commit_counts.values().copied().max().unwrap_or(0) as f64 / usable_commits as f64;
    if max_share > DIVERGENT_MAX_DOMINANT_SHARE {
        return None;
    }

    let mut dirs: Vec<&String> = dir_commit_counts.keys().collect();
    dirs.sort();
    let dir_list = dirs.iter().map(|d| d.as_str()).collect::<Vec<_>>().join(", ");

    Some(make_finding(
        "divergent-change",
        "design",
        Severity::Medium,
        path,
        "Divergent Change: file's commit history is scattered across many unrelated areas".to_string(),
        format!(
            "Across its last {usable_commits} commits with co-changes, this file was touched alongside {} distinct, unrelated areas ({dir_list}) with no single dominant collaborator. That pattern usually means the file mixes multiple responsibilities that each change for a different reason — consider splitting it along those seams.",
            dir_commit_counts.len()
        ),
        Vec::new(),
    ))
}

/// Runs the Divergent Change check for each changed file, using its git
/// history (not just the current diff). Silently skips files with too
/// little history to judge, and any repo where `git log`/`git show` fail
/// (e.g. a shallow clone or non-git checkout) rather than erroring.
pub fn run_divergent_change_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    let capped = &changed_files[..changed_files.len().min(DIVERGENT_MAX_FILES_PER_RUN)];
    capped.iter().filter_map(|path| divergent_change_for_file(repo_root, path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_many_small_files_as_shotgun_surgery() {
        let files: Vec<FileChange> = (0..10).map(|i| FileChange { path: format!("f{i}.go"), additions: 2, deletions: 1 }).collect();
        let findings = detect_shotgun_surgery(&files);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("shotgun-surgery"));
        assert_eq!(findings[0].related_locations.as_ref().unwrap().len(), 9);
    }

    #[test]
    fn does_not_flag_too_few_files() {
        let files: Vec<FileChange> = (0..3).map(|i| FileChange { path: format!("f{i}.go"), additions: 2, deletions: 1 }).collect();
        assert!(detect_shotgun_surgery(&files).is_empty());
    }

    #[test]
    fn does_not_flag_many_files_when_one_change_is_substantial() {
        let mut files: Vec<FileChange> = (0..9).map(|i| FileChange { path: format!("f{i}.go"), additions: 2, deletions: 1 }).collect();
        files.push(FileChange { path: "big.go".to_string(), additions: 200, deletions: 50 });
        assert!(detect_shotgun_surgery(&files).is_empty());
    }

    #[test]
    fn flags_divergent_change_when_co_change_partners_are_scattered() {
        let repo = autoreview_test_support::init_repo(&[("shared.go", "package shared\n")]);
        repo.commit(&[("shared.go", "package shared\nvar a = 1\n"), ("auth/a.go", "package auth\n")], "auth change");
        repo.commit(&[("shared.go", "package shared\nvar a = 2\n"), ("billing/b.go", "package billing\n")], "billing change");
        repo.commit(&[("shared.go", "package shared\nvar a = 3\n"), ("ui/u.go", "package ui\n")], "ui change");
        repo.commit(&[("shared.go", "package shared\nvar a = 4\n"), ("metrics/m.go", "package metrics\n")], "metrics change");
        repo.commit(&[("shared.go", "package shared\nvar a = 5\n"), ("auth/a2.go", "package auth\n")], "auth change 2");
        repo.commit(&[("shared.go", "package shared\nvar a = 6\n"), ("billing/b2.go", "package billing\n")], "billing change 2");

        let findings = run_divergent_change_check(repo.path(), &["shared.go".to_string()]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("divergent-change"));
    }

    #[test]
    fn does_not_flag_a_file_with_one_consistent_collaborator() {
        let repo = autoreview_test_support::init_repo(&[("shared.go", "package shared\n")]);
        for i in 0..6 {
            repo.commit(&[("shared.go", format!("package shared\nvar a = {i}\n").as_str()), ("auth/a.go", "package auth\n")], "auth change");
        }

        let findings = run_divergent_change_check(repo.path(), &["shared.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_file_with_too_little_history() {
        let repo = autoreview_test_support::init_repo(&[("shared.go", "package shared\n")]);
        repo.commit(&[("shared.go", "package shared\nvar a = 1\n"), ("auth/a.go", "package auth\n")], "auth change");

        let findings = run_divergent_change_check(repo.path(), &["shared.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn skips_files_past_the_max_files_per_run_cap() {
        let repo = autoreview_test_support::init_repo(&[("shared.go", "package shared\n")]);
        repo.commit(&[("shared.go", "package shared\nvar a = 1\n"), ("auth/a.go", "package auth\n")], "auth change");
        repo.commit(&[("shared.go", "package shared\nvar a = 2\n"), ("billing/b.go", "package billing\n")], "billing change");
        repo.commit(&[("shared.go", "package shared\nvar a = 3\n"), ("ui/u.go", "package ui\n")], "ui change");
        repo.commit(&[("shared.go", "package shared\nvar a = 4\n"), ("metrics/m.go", "package metrics\n")], "metrics change");
        repo.commit(&[("shared.go", "package shared\nvar a = 5\n"), ("auth/a2.go", "package auth\n")], "auth change 2");
        repo.commit(&[("shared.go", "package shared\nvar a = 6\n"), ("billing/b2.go", "package billing\n")], "billing change 2");

        // shared.go would be flagged if checked on its own (proven by
        // `flags_divergent_change_when_co_change_partners_are_scattered`
        // above) — placing it past the cap, behind DIVERGENT_MAX_FILES_PER_RUN
        // filler entries, must suppress that finding.
        let mut changed: Vec<String> = (0..DIVERGENT_MAX_FILES_PER_RUN).map(|i| format!("filler{i}.go")).collect();
        changed.push("shared.go".to_string());
        assert!(changed.len() > DIVERGENT_MAX_FILES_PER_RUN);

        let findings = run_divergent_change_check(repo.path(), &changed);
        assert!(findings.is_empty(), "shared.go sits past the cap and should never be checked, got: {findings:#?}");
    }
}
