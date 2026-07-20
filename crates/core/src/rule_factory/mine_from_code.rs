//! Mines candidate "usage convention" rules directly from the repo's own
//! Go source — a third input source alongside `mine::mine_candidates`
//! (agent-finding recurrence) and `mine_from_comments` (PR-comment
//! recurrence), but a genuinely different kind of signal: those two
//! cluster *existing labeled findings/comments* (something already flagged
//! as worth a human's attention); this module discovers patterns nobody
//! has flagged at all, purely from how consistently the codebase already
//! uses its own APIs.
//!
//! Technique: **call-pair co-occurrence mining**, the same family of idea
//! as academic "must-call-pair" specification miners (e.g. mining that
//! `Lock()` is almost always paired with `Unlock()`, or that `Query(...)`
//! is almost always paired with a `.Close()` on its result) — look at
//! every call site for method `A`, check whether method `B` also appears
//! nearby in the same file, and if `B` accompanies *almost every* call to
//! `A` across the whole repo, that pairing is a candidate convention: a
//! future call to `A` *without* `B` nearby is plausibly a real bug (a
//! leaked lock, an unclosed resource), not just a style nit.
//!
//! Deliberately approximate, matching this project's established stance
//! for its other line-scan analyzers (`complexity.rs`, `practices.rs`):
//! "nearby" means "within `WINDOW_LINES` lines in the same file," not
//! "in the same function" (no real function-boundary parsing here) — a
//! genuinely different function that happens to sit within the window can
//! produce a false co-occurrence. This is a precision cost the consistency
//! threshold is meant to absorb (a spurious neighbor is rare enough not to
//! dominate a real, repo-wide, >=90%-consistent pairing), not something
//! this pass tries to eliminate structurally. A human reviews every
//! candidate this produces before it's ever considered for a real rule —
//! same "cheap to be wrong, a human catches it" posture `mine_from_comments`
//! already takes for its own category-guessing heuristic.
//!
//! Scope, stated honestly: this is a discovery/inspection prototype, not
//! yet wired into the full mine -> draft -> bench -> shadow pipeline the
//! other two sources feed (`CandidateSeed`'s shape — `distinct_run_count`,
//! `member_fingerprints` — doesn't map cleanly onto "one repo-wide
//! consistency ratio," so forcing it through that type would be more
//! misleading than useful). `mine_call_pair_conventions` returns its
//! findings directly; turning a strong candidate into an actual
//! `kind: pattern`/`kind: taint` rule (e.g. "flag a call to `A` with no
//! `B` in the same function") is a real follow-on, not done here.

use std::collections::HashMap;
use std::path::Path;

/// How many lines after a call to `A` count as "nearby" when checking for
/// an accompanying call to `B` — see the module doc for why this is a
/// line-window, not a real function-boundary scan.
const WINDOW_LINES: usize = 15;

/// A discovered `A` -> `B` call-pairing convention.
#[derive(Debug, Clone, PartialEq)]
pub struct CallPairConvention {
    pub call_a: String,
    pub call_b: String,
    pub occurrences_of_a: usize,
    pub co_occurrences: usize,
    /// `co_occurrences as f64 / occurrences_of_a as f64`.
    pub consistency: f64,
    /// One `path:line` for an `A` call site that DID pair with `B` — lets
    /// a human spot-check the convention actually looks like what they'd
    /// expect before trusting it.
    pub example_location: String,
}

/// A `.methodName(` call occurrence: the method name and the line it's on.
struct CallSite {
    name: String,
    line: usize,
}

/// Extracts every `.identifier(` occurrence on each line — deliberately
/// permissive (matches a method call through any receiver shape,
/// `foo.Bar(`/`a.b.Bar(`/`(*x).Bar(`) since precision here isn't the goal;
/// the consistency threshold downstream is.
fn call_sites_in_file(content: &str) -> Vec<CallSite> {
    let mut sites = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'.' {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j > start && j < bytes.len() && bytes[j] == b'(' {
                    let name = &line[start..j];
                    // Skip names starting with a digit (not a valid
                    // identifier, would only match on malformed input) and
                    // empty matches.
                    if !name.is_empty() && !name.as_bytes()[0].is_ascii_digit() {
                        sites.push(CallSite { name: name.to_string(), line: idx + 1 });
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    sites
}

/// Walks every `.go` file under `repo_root` (not just changed files — this
/// is a whole-repo convention-mining pass, closer in spirit to
/// `mine_from_comments`'s whole-history PR scan than to a diff-scoped
/// analyzer) and returns every call-pair convention meeting
/// `min_occurrences`/`min_consistency`, most-consistent first.
pub fn mine_call_pair_conventions(repo_root: &Path, min_occurrences: usize, min_consistency: f64) -> Vec<CallPairConvention> {
    let mut occurrences_of: HashMap<String, usize> = HashMap::new();
    let mut co_occurrences: HashMap<(String, String), usize> = HashMap::new();
    let mut example_location: HashMap<(String, String), String> = HashMap::new();

    for path in go_files(repo_root) {
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let sites = call_sites_in_file(&content);
        let rel_path = path.strip_prefix(repo_root).unwrap_or(&path).display().to_string();

        for (i, site) in sites.iter().enumerate() {
            *occurrences_of.entry(site.name.clone()).or_insert(0) += 1;

            let mut seen_partners: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for other in &sites[i + 1..] {
                if other.line > site.line + WINDOW_LINES {
                    break;
                }
                if other.name == site.name || !seen_partners.insert(other.name.as_str()) {
                    continue;
                }
                let key = (site.name.clone(), other.name.clone());
                *co_occurrences.entry(key.clone()).or_insert(0) += 1;
                example_location.entry(key).or_insert_with(|| format!("{rel_path}:{}", site.line));
            }
        }
    }

    let mut conventions: Vec<CallPairConvention> = co_occurrences
        .into_iter()
        .filter_map(|((a, b), co_count)| {
            let total_a = *occurrences_of.get(&a)?;
            if total_a < min_occurrences {
                return None;
            }
            let consistency = co_count as f64 / total_a as f64;
            if consistency < min_consistency {
                return None;
            }
            let example = example_location.get(&(a.clone(), b.clone()))?.clone();
            Some(CallPairConvention { call_a: a, call_b: b, occurrences_of_a: total_a, co_occurrences: co_count, consistency, example_location: example })
        })
        .collect();

    conventions.sort_by(|x, y| y.consistency.partial_cmp(&x.consistency).unwrap_or(std::cmp::Ordering::Equal).then_with(|| y.occurrences_of_a.cmp(&x.occurrences_of_a)));
    conventions
}

fn go_files(repo_root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    collect_go_files(repo_root, &mut out);
    out
}

fn collect_go_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()).map(|n| n == ".git" || n == "vendor" || n == "node_modules").unwrap_or(false) {
                continue;
            }
            collect_go_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("go") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn call_sites_in_file_finds_method_calls() {
        let sites = call_sites_in_file("func f() {\n\tmu.Lock()\n\tdefer mu.Unlock()\n}\n");
        let names: Vec<&str> = sites.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Lock", "Unlock"]);
    }

    #[test]
    fn mines_a_strong_lock_unlock_convention() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write(
                &dir.path().join(format!("f{i}.go")),
                &format!("package main\n\nfunc f{i}() {{\n\tmu.Lock()\n\tdoWork()\n\tmu.Unlock()\n}}\n"),
            );
        }
        let conventions = mine_call_pair_conventions(dir.path(), 3, 0.9);
        let lock_unlock = conventions.iter().find(|c| c.call_a == "Lock" && c.call_b == "Unlock").expect("expected a Lock -> Unlock convention");
        assert_eq!(lock_unlock.occurrences_of_a, 5);
        assert_eq!(lock_unlock.co_occurrences, 5);
        assert_eq!(lock_unlock.consistency, 1.0);
    }

    #[test]
    fn an_inconsistent_pairing_is_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("f0.go"), "package main\n\nfunc f0() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        write(&dir.path().join("f1.go"), "package main\n\nfunc f1() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        // Two more Lock() calls with no accompanying Unlock() nearby —
        // consistency drops to 50%, below the 90% threshold.
        write(&dir.path().join("f2.go"), "package main\n\nfunc f2() {\n\tmu.Lock()\n\tdoWork()\n}\n");
        write(&dir.path().join("f3.go"), "package main\n\nfunc f3() {\n\tmu.Lock()\n\tdoWork()\n}\n");

        let conventions = mine_call_pair_conventions(dir.path(), 3, 0.9);
        assert!(!conventions.iter().any(|c| c.call_a == "Lock" && c.call_b == "Unlock"), "got: {conventions:#?}");
    }

    #[test]
    fn below_min_occurrences_is_not_reported_even_if_perfectly_consistent() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("f0.go"), "package main\n\nfunc f0() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        let conventions = mine_call_pair_conventions(dir.path(), 3, 0.9);
        assert!(conventions.is_empty(), "1 occurrence should be below the min_occurrences=3 floor, got: {conventions:#?}");
    }

    #[test]
    fn a_partner_outside_the_line_window_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut body = String::from("package main\n\nfunc f() {\n\tmu.Lock()\n");
        for i in 0..(WINDOW_LINES + 5) {
            body.push_str(&format!("\t_ = {i}\n"));
        }
        body.push_str("\tmu.Unlock()\n}\n");
        write(&dir.path().join("f0.go"), &body);
        write(&dir.path().join("f1.go"), "package main\n\nfunc f1() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        write(&dir.path().join("f2.go"), "package main\n\nfunc f2() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        write(&dir.path().join("f3.go"), "package main\n\nfunc f3() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");

        let conventions = mine_call_pair_conventions(dir.path(), 4, 0.9);
        // 4 Lock() occurrences total, only 3 have Unlock() within the
        // window -> 75% consistency, below the 90% threshold.
        assert!(!conventions.iter().any(|c| c.call_a == "Lock" && c.call_b == "Unlock"), "got: {conventions:#?}");
    }

    #[test]
    fn skips_vendor_and_git_directories() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("vendor/pkg/f.go"), "package pkg\n\nfunc f() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        write(&dir.path().join(".git/f.go"), "package pkg\n\nfunc f() {\n\tmu.Lock()\n\tmu.Unlock()\n}\n");
        let files = go_files(dir.path());
        assert!(files.is_empty(), "got: {files:#?}");
    }
}
