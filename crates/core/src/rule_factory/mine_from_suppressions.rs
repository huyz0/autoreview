//! Mines candidate rule seeds from the repo's own **linter-suppression
//! comments** — a fifth input source, and the cheapest/safest of the six:
//! a pure filesystem walk over the repo's own already-checked-out source,
//! no git, no network, no auth. Each suppression (`// nolint`,
//! `@SuppressWarnings`, `// eslint-disable`, `# noqa`, ...) is a
//! human-flagged exception signal — someone looked at a real linter
//! finding and decided it didn't apply here, which is exactly the kind of
//! "we know about this pattern and have an opinion on it" evidence
//! `mine_from_comments.rs` already mines from PR review comments, just
//! recorded in the code itself instead of a review thread.
//!
//! `run_id` is deliberately the **file path**, not a commit or PR — a
//! different reuse of the field than every other source, worth stating
//! plainly rather than leaving implicit: recurrence here means "this
//! suppression shape shows up in >= 2 distinct files," not ">= 2 distinct
//! review sessions." Two occurrences of the same marker in one file are
//! evidence of a repeated *local* pattern, not a repo-wide convention —
//! `mine_candidates`' existing ">= 2 distinct runs" gate correctly refuses
//! to treat them as recurring on their own.

use std::path::{Path, PathBuf};

use crate::rule_factory::category_heuristics::guess_category;
use crate::storage::history_store::MinedFindingRow;

/// Recognized suppression markers, checked as a plain substring per line
/// (these are all distinctive enough — no plain-English word risks a
/// false match the way a bare keyword would). Not every marker maps to a
/// rule *kind* this project ships (Python's `# noqa` has no Python
/// analyzer here at all) — kept anyway, since a mined candidate from a
/// language this project doesn't yet check is still useful signal for a
/// human to see, and costs nothing to detect.
const SUPPRESSION_MARKERS: &[&str] = &["// nolint", "//nolint", "@SuppressWarnings", "// eslint-disable", "/* eslint-disable", "# noqa"];

/// Directory names never worth walking into — vendored/generated/build
/// output, which can be enormous and never contains a suppression comment
/// a human actually wrote.
const SKIP_DIRS: &[&str] = &[".git", "vendor", "node_modules", "dist", "build", "target", ".autoreview"];

const SOURCE_EXTENSIONS: &[&str] = &["go", "java", "kt", "kts", "js", "jsx", "ts", "tsx", "py"];

/// The first marker matched on `line`, if any — pure, tested directly
/// rather than only through the whole-file walk.
fn suppression_marker_in_line(line: &str) -> Option<&'static str> {
    SUPPRESSION_MARKERS.iter().copied().find(|marker| line.contains(marker))
}

/// A little context around the matching line — the line itself plus the
/// line immediately before it, where an explanatory comment (the actual
/// reason for the suppression) most often sits. Trimmed and joined with a
/// space so this reads as one continuous snippet for `guess_category`/
/// `title_similarity` to work over, the same "single text blob" shape
/// every other source's `message` already is.
fn extract_context(lines: &[&str], idx: usize) -> String {
    let start = idx.saturating_sub(1);
    lines[start..=idx].iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

fn is_source_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext))
}

fn walk_source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            walk_source_files(&path, out);
        } else if is_source_file(&path) {
            out.push(path);
        }
    }
}

/// Every suppression found in one file's content, as `MinedFindingRow`s —
/// `rel_path` is used for `fingerprint`/`run_id`/`title` so the result is
/// stable across machines (an absolute path would leak the scanning
/// machine's own directory layout into a committed seed file).
fn suppressions_in_file(rel_path: &str, content: &str) -> Vec<MinedFindingRow> {
    let lines: Vec<&str> = content.lines().collect();
    let mut rows = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let Some(marker) = suppression_marker_in_line(line) else { continue };
        let context = extract_context(&lines, idx);
        let line_number = idx + 1;
        rows.push(MinedFindingRow {
            fingerprint: format!("suppression-{rel_path}-{line_number}"),
            category: guess_category(&context),
            rule_id_or_aspect: "lint-suppression".to_string(),
            title: format!("{marker} suppression in {rel_path}:{line_number}"),
            message: context,
            run_id: rel_path.to_string(),
        });
    }
    rows
}

/// Walks every recognized source file under `repo_root` (skipping vendor/
/// build directories) and mines every suppression comment found into a
/// `MinedFindingRow`, ready to hand to `mine::mine_candidates` alongside
/// (or instead of) any other source. Best-effort: a file that fails to
/// read (rare — a symlink race, non-UTF8 content) is skipped rather than
/// failing the whole scan.
pub fn mine_from_suppressions(repo_root: &Path) -> Vec<MinedFindingRow> {
    let mut files = Vec::new();
    walk_source_files(repo_root, &mut files);

    let mut rows = Vec::new();
    for path in files {
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let rel_path = path.strip_prefix(repo_root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        rows.extend(suppressions_in_file(&rel_path, &content));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_common_suppression_markers() {
        for line in ["result, _ := risky() // nolint", "//nolint:errcheck", "@SuppressWarnings(\"unchecked\")", "// eslint-disable-next-line no-unused-vars", "x = f()  # noqa: E501"] {
            assert!(suppression_marker_in_line(line).is_some(), "expected {line:?} to be recognized");
        }
    }

    #[test]
    fn does_not_match_an_ordinary_line() {
        assert!(suppression_marker_in_line("result, err := risky()").is_none());
    }

    #[test]
    fn extract_context_includes_the_explanatory_line_above() {
        let lines = vec!["// safe: caller always checks the pointer first", "result, _ := risky() // nolint"];
        let ctx = extract_context(&lines, 1);
        assert!(ctx.contains("safe: caller always checks"), "got: {ctx}");
        assert!(ctx.contains("nolint"), "got: {ctx}");
    }

    #[test]
    fn suppressions_in_file_builds_the_expected_row_shape() {
        let rows = suppressions_in_file("pkg/handler.go", "package pkg\n\nresult, _ := risky() // nolint\n");
        assert_eq!(rows.len(), 1, "got: {rows:#?}");
        assert_eq!(rows[0].fingerprint, "suppression-pkg/handler.go-3");
        assert_eq!(rows[0].run_id, "pkg/handler.go");
        assert_eq!(rows[0].rule_id_or_aspect, "lint-suppression");
    }

    #[test]
    fn skips_a_file_with_no_suppressions() {
        assert!(suppressions_in_file("pkg/clean.go", "package pkg\n\nfunc f() {}\n").is_empty());
    }

    #[test]
    fn mines_real_suppressions_from_a_real_directory_tree_and_skips_vendor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg").join("a.go"), "package pkg\n\nresult, _ := risky() // nolint\n").unwrap();
        std::fs::create_dir_all(dir.path().join("vendor").join("dep")).unwrap();
        std::fs::write(dir.path().join("vendor").join("dep").join("b.go"), "package dep\n\nresult, _ := risky() // nolint\n").unwrap();

        let rows = mine_from_suppressions(dir.path());
        assert_eq!(rows.len(), 1, "got: {rows:#?} — vendor/ must be skipped");
        assert_eq!(rows[0].run_id, "pkg/a.go");
    }

    #[test]
    fn a_suppression_pattern_recurring_across_three_files_clusters_into_one_seed() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(dir.path().join(format!("{name}.go")), format!("package pkg\n\n// caller always validates first\nresult, _ := risky{name}() // nolint:errcheck\n")).unwrap();
        }

        let rows = mine_from_suppressions(dir.path());
        assert_eq!(rows.len(), 3, "got: {rows:#?}");

        let seeds = crate::rule_factory::mine::mine_candidates(rows);
        assert_eq!(seeds.len(), 1, "got: {seeds:#?}");
        assert_eq!(seeds[0].distinct_run_count, 3);
    }
}
