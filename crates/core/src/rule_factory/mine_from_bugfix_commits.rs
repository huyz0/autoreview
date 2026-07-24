//! Mines candidate rule seeds from the repo's own **bug-fix commits** — a
//! fourth input source for `rule_factory::mine`'s clustering, and the
//! first with zero auth/network dependency at all: it only ever reads
//! this repo's own local `git log`/`git diff`, so it works identically
//! against any git host (or no remote at all).
//!
//! Real prior art for this technique: academic "bug-fix pattern mining"
//! (e.g. Pan et al.'s catalog of 27 recurring bug-fix patterns mined from
//! open-source Java project histories, and the "Coming" tool for mining
//! change-pattern instances from git commits) — the underlying intuition
//! is the same one `mine_from_comments.rs` already applies to PR review
//! comments, just against a different, complementary signal: what a
//! reviewer *said* out loud versus what an author silently corrected in
//! a follow-up commit, which can surface real recurring issues a repo
//! with a thin review culture (or fixes that happened before a PR was
//! ever opened) would otherwise miss entirely.
//!
//! One important divergence from every other mining source, worth stating
//! plainly: `message` here is a real diff hunk, not prose describing an
//! issue. That's a genuine asset for `draft.rs`'s eventual prompt (real
//! code beats a paraphrase when drafting an ast-grep pattern), but a real
//! risk for `mine_candidates`'s trigram-Jaccard clustering, which can
//! cluster on incidental lexical overlap in common code shapes (`if err
//! != nil`, brace/keyword noise) rather than shared semantic intent. Kept
//! in check by capping `MAX_DIFF_CHARS` (so a handful of common tokens
//! don't dominate the shingle set) and by `run_id = commit sha`, which
//! still requires the *same* diff shape to recur across >= 2 genuinely
//! distinct commits before it clusters into anything.
//!
//! Confirmed empirically against a real seeded repo while building this:
//! `mine_candidates`'s clustering only compares each new item against a
//! cluster's *first* (representative) member, not its nearest member —
//! so three genuinely similar fixes to files at different points in a
//! larger file (different hunk line numbers/context) can fail to fully
//! cluster even though each is pairwise similar to its neighbor, because
//! the third drifts too far from the *first* one specifically. Real
//! fixes to structurally-uniform code (a repeated small function shape,
//! one fix per file) cluster cleanly; fixes to increasingly different
//! parts of one large file are the case most likely to under-cluster.
//! Not something this module works around — it's an existing property of
//! `mine_candidates` shared by every source, just more visible here since
//! diff text varies more between instances than PR-comment prose does.

use std::path::Path;
use std::process::Command;

use crate::rule_factory::category_heuristics::guess_category;
use crate::storage::history_store::MinedFindingRow;

/// Whole-word match only — a raw substring check would also match
/// "prefix"/"suffix"/"affix" for "fix" and "debug"/"debugging" for "bug",
/// a false-positive class this source is more exposed to than
/// `mine_from_comments.rs::guess_category`'s own keyword list (whose
/// words are already fairly substring-safe).
const BUGFIX_KEYWORDS: &[&str] = &["fix", "fixed", "fixes", "bug", "bugfix", "patch", "patched"];
/// Bounds how much of one file's diff in one commit becomes a
/// `MinedFindingRow::message` — see the module doc's clustering-risk note
/// for why this stays small rather than capturing a whole real-world diff.
const MAX_DIFF_CHARS: usize = 1500;
/// Bounds how far back `git log` is asked to scan — an unbounded scan
/// against a large, old repo could take a long time and mine mostly
/// irrelevant ancient history; matches the same "recent window, not the
/// whole history" posture `mine_from_comments.rs`'s `lookback_prs` takes
/// for PRs.
const DEFAULT_MAX_COMMITS: usize = 200;

fn contains_whole_word(text: &str, keyword: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| word.eq_ignore_ascii_case(keyword))
}

/// Whether a commit subject line looks like a bug fix — pure, tested
/// against literal strings so the false-positive-avoidance logic (whole-
/// word, not substring) is directly verifiable.
fn is_bugfix_subject(subject: &str) -> bool {
    BUGFIX_KEYWORDS.iter().any(|k| contains_whole_word(subject, k))
}

fn run_git(repo_root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git").args(args).current_dir(repo_root).output()?;
    if !output.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Lists up to `max_commits` most recent commits as `(sha, subject)`
/// pairs — `%x1f` (the ASCII unit separator) delimits the two fields
/// rather than a printable character, so a subject line containing a
/// space, colon, or pipe can never be mistaken for the delimiter.
fn list_recent_commits(repo_root: &Path, max_commits: usize) -> anyhow::Result<Vec<(String, String)>> {
    let max_str = max_commits.to_string();
    let stdout = run_git(repo_root, &["log", "-n", &max_str, "--pretty=format:%H%x1f%s"])?;
    Ok(stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\u{1f}');
            let sha = parts.next()?.to_string();
            let subject = parts.next()?.to_string();
            Some((sha, subject))
        })
        .collect())
}

fn changed_files_for_commit(repo_root: &Path, sha: &str) -> anyhow::Result<Vec<String>> {
    let stdout = run_git(repo_root, &["diff-tree", "--no-commit-id", "--name-only", "-r", sha])?;
    Ok(stdout.lines().map(str::to_string).collect())
}

/// Deliberately `git diff <sha>~1 <sha>`, not `git show <sha>` — `git
/// show` prefixes the diff with commit metadata (the full commit hash,
/// author, timestamp), all high-entropy text that's completely different
/// between any two commits regardless of how similar their actual code
/// change is. Left in, that preamble would dilute `title_similarity`'s
/// trigram-overlap ratio enough to stop two genuinely similar fixes from
/// ever clustering — confirmed empirically against a real seeded repo
/// (three near-identical nil-check fixes failed to cluster with `git
/// show`'s output, clustered correctly once switched to `git diff`).
/// Fails (and the caller skips the row) for a root commit with no
/// parent — an acceptable, rare edge case, not worth special-casing.
fn diff_for_file_in_commit(repo_root: &Path, sha: &str, path: &str) -> anyhow::Result<String> {
    let stdout = run_git(repo_root, &["diff", &format!("{sha}~1"), sha, "--", path])?;
    Ok(stdout.chars().take(MAX_DIFF_CHARS).collect())
}

/// Builds one `MinedFindingRow` per `(commit, changed file)` pair —
/// deliberately not one row per commit, since a multi-file commit's files
/// are independent pieces of evidence, and a real recurring pattern is
/// more likely to show up as "this same kind of fix, to this kind of
/// file, across several commits" than "this exact multi-file commit
/// recurred." `run_id = sha`, so `mine_candidates`'s existing ">= 2
/// distinct runs" gate becomes ">= 2 distinct commits" for free, the same
/// way it becomes ">= 2 distinct PRs" for `mine_from_comments.rs`.
fn diff_text_to_mined_row(sha: &str, subject: &str, path: &str, diff: &str) -> MinedFindingRow {
    MinedFindingRow {
        fingerprint: format!("bugfix-{sha}-{path}"),
        category: guess_category(subject),
        rule_id_or_aspect: "bugfix-commit".to_string(),
        title: subject.to_string(),
        message: diff.to_string(),
        run_id: sha.to_string(),
    }
}

/// Mines up to `max_commits` most recent commits' bug-fix-shaped diffs
/// into `MinedFindingRow`s. Pure local git — no auth, no network, works
/// even with no remote configured at all. A commit whose diff can't be
/// read (rare — a merge commit with an unusual shape, a binary file) is
/// skipped rather than failing the whole scan, same best-effort posture
/// `mine_from_comments.rs` takes for one PR's comments failing to fetch.
pub fn mine_from_bugfix_commits(repo_root: &Path, max_commits: usize) -> anyhow::Result<Vec<MinedFindingRow>> {
    let mut rows = Vec::new();
    for (sha, subject) in list_recent_commits(repo_root, max_commits)? {
        if !is_bugfix_subject(&subject) {
            continue;
        }
        let Ok(files) = changed_files_for_commit(repo_root, &sha) else { continue };
        for path in files {
            let Ok(diff) = diff_for_file_in_commit(repo_root, &sha, &path) else { continue };
            if diff.trim().is_empty() {
                continue;
            }
            rows.push(diff_text_to_mined_row(&sha, &subject, &path, &diff));
        }
    }
    Ok(rows)
}

/// The default `git log` scan depth — `commands::rules` reaches for this
/// rather than hardcoding `200` a second time.
pub const DEFAULT_MAX_COMMITS_SCANNED: usize = DEFAULT_MAX_COMMITS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_bugfix_shaped_subjects() {
        for subject in ["Fix null pointer in handler", "fixed a race condition", "bugfix: off-by-one in loop", "patch the memory leak"] {
            assert!(is_bugfix_subject(subject), "expected {subject:?} to be recognized");
        }
    }

    #[test]
    fn does_not_match_prefix_suffix_or_debug_as_a_substring() {
        for subject in ["Add a prefix option to the logger", "Refactor debug output", "Support custom suffix strings", "Improve traffic throughput"] {
            assert!(!is_bugfix_subject(subject), "expected {subject:?} NOT to be recognized (substring false positive)");
        }
    }

    #[test]
    fn recognizes_a_bugfix_keyword_regardless_of_case() {
        assert!(is_bugfix_subject("FIX: crash on empty input"));
    }

    #[test]
    fn diff_text_to_mined_row_builds_the_expected_shape() {
        let row = diff_text_to_mined_row("abc123", "Fix null check", "main.go", "-if x != nil {\n+if x == nil {\n");
        assert_eq!(row.fingerprint, "bugfix-abc123-main.go");
        assert_eq!(row.run_id, "abc123");
        assert_eq!(row.rule_id_or_aspect, "bugfix-commit");
        assert_eq!(row.title, "Fix null check");
        assert_eq!(row.category, "correctness");
    }

    #[test]
    fn mines_a_real_bugfix_commit_from_a_real_git_repo() {
        let repo = autoreview_test_support::init_repo(&[("main.go", "package main\n\nfunc f(x *int) {\n\tprintln(*x)\n}\n")]);
        repo.commit(&[("main.go", "package main\n\nfunc f(x *int) {\n\tif x == nil {\n\t\treturn\n\t}\n\tprintln(*x)\n}\n")], "Fix nil pointer dereference in f");
        repo.commit(&[("README.md", "# hello\n")], "Add a readme");

        let rows = mine_from_bugfix_commits(repo.path(), 10).unwrap();
        assert_eq!(rows.len(), 1, "got: {rows:#?}");
        assert_eq!(rows[0].title, "Fix nil pointer dereference in f");
        assert!(rows[0].message.contains("x == nil"), "got: {}", rows[0].message);
    }

    #[test]
    fn does_not_mine_a_non_bugfix_commit() {
        let repo = autoreview_test_support::init_repo(&[("main.go", "package main\n")]);
        repo.commit(&[("main.go", "package main\n\nfunc f() {}\n")], "Add helper function f");

        let rows = mine_from_bugfix_commits(repo.path(), 10).unwrap();
        assert!(rows.is_empty(), "got: {rows:#?}");
    }

    #[test]
    fn a_realistic_chain_of_three_similar_fixes_clusters_into_one_seed() {
        // Real end-to-end check through the shared mine_candidates
        // clustering algorithm, not just this module's own row-shape —
        // confirms the `git diff` (not `git show`) fix above actually
        // produces text lean enough for three independent, structurally-
        // uniform bug fixes to cluster.
        let repo = autoreview_test_support::init_repo(&[
            ("handler_a.go", "package main\n\nfunc handlea(v *int) {\n\tprintln(*v)\n}\n"),
            ("handler_b.go", "package main\n\nfunc handleb(v *int) {\n\tprintln(*v)\n}\n"),
            ("handler_c.go", "package main\n\nfunc handlec(v *int) {\n\tprintln(*v)\n}\n"),
        ]);
        repo.commit(&[("handler_a.go", "package main\n\nfunc handlea(v *int) {\n\tif v == nil {\n\t\treturn\n\t}\n\tprintln(*v)\n}\n")], "Fix nil pointer dereference in handlea");
        repo.commit(&[("handler_b.go", "package main\n\nfunc handleb(v *int) {\n\tif v == nil {\n\t\treturn\n\t}\n\tprintln(*v)\n}\n")], "Fix nil pointer dereference in handleb");
        repo.commit(&[("handler_c.go", "package main\n\nfunc handlec(v *int) {\n\tif v == nil {\n\t\treturn\n\t}\n\tprintln(*v)\n}\n")], "Fix nil pointer dereference in handlec");
        repo.commit(&[("README.md", "# example\n")], "Add project readme");

        let rows = mine_from_bugfix_commits(repo.path(), 200).unwrap();
        assert_eq!(rows.len(), 3, "got: {rows:#?}");

        let seeds = crate::rule_factory::mine::mine_candidates(rows);
        assert_eq!(seeds.len(), 1, "got: {seeds:#?}");
        assert_eq!(seeds[0].distinct_run_count, 3);
    }
}
