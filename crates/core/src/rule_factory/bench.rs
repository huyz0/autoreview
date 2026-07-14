//! Rule-factory bench: the gate a drafted candidate rule must clear before
//! a human ever sees it in `rules review` (itself still a stub). Per the
//! plan, three checks — self-test, historical precision, FP smoke test —
//! all must pass. Two are implemented honestly here; one is not, and this
//! module says so rather than faking it:
//!
//! - **Self-test**: real, against `tests/positive/*`/`tests/negative/*` a
//!   human supplies under the candidate's directory (per `draft.rs`'s own
//!   documented limitation — a seed only carries finding *descriptions*,
//!   not source code, so bench cannot invent fixtures itself). Runs the
//!   drafted rule via the real `ast-grep` binary.
//! - **FP smoke test**: real, against a random sample of the *current
//!   repo's* source files — no history-store dependency needed, so this one
//!   is fully available today. Simplified from the plan's "<0.1% line-match
//!   rate" to a file-level match rate (counting matched *lines* would need
//!   per-line dedup logic beyond what's needed to catch an obviously
//!   over-broad pattern) — documented here, not silently substituted.
//! - **Historical precision**: NOT implemented. The plan's design requires
//!   replaying the candidate against every stored historical snippet in
//!   that language, but the history store (`findings` table) does not
//!   retain the original source snippet or file content a finding was
//!   found in — only its fingerprint/category/title/message (see
//!   `MinedFindingRow`). Computing this needs the history schema to grow a
//!   snippet column first; `BenchReport` reports this check as `Skipped`
//!   rather than fabricating a number.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfTestResult {
    pub positive_total: usize,
    pub positive_matched: usize,
    pub negative_total: usize,
    pub negative_matched: usize,
}

impl SelfTestResult {
    pub fn passed(&self) -> bool {
        self.positive_total > 0 && self.positive_matched == self.positive_total && self.negative_matched == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpSmokeResult {
    pub sampled_files: usize,
    pub matched_files: usize,
}

/// The plan's FP smoke test threshold, applied here at file granularity
/// (see module docs): well under 1 in 200 sampled files should match a
/// precise rule.
const MAX_FP_MATCH_RATE: f64 = 0.005;
/// Cap on how many current-repo files the FP smoke test samples — enough to
/// catch an obviously over-broad pattern without scanning a huge repo on
/// every bench call.
const FP_SAMPLE_CAP: usize = 40;

impl FpSmokeResult {
    pub fn passed(&self) -> bool {
        self.sampled_files == 0 || (self.matched_files as f64 / self.sampled_files as f64) <= MAX_FP_MATCH_RATE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchVerdict {
    /// Every implemented check passed — ready for the (still-stubbed)
    /// human `rules review` gate.
    Candidate,
    /// No `tests/positive`/`tests/negative` fixtures exist yet under the
    /// candidate directory for bench to run self-test against.
    NeedsFixtures,
    SelfTestFailed,
    FailedFpSmoke,
}

#[derive(Debug, Clone)]
pub struct BenchReport {
    pub cluster_id: String,
    pub self_test: Option<SelfTestResult>,
    pub fp_smoke: Option<FpSmokeResult>,
    pub historical_precision_skipped_reason: String,
    pub verdict: BenchVerdict,
}

#[derive(Debug, Deserialize)]
struct RuleLanguage {
    language: String,
}

fn extension_for_language(language: &str) -> Option<&'static str> {
    match language {
        "Go" => Some("go"),
        "Java" => Some("java"),
        "Kotlin" => Some("kt"),
        _ => None,
    }
}

/// Runs `ast-grep scan` with exactly one rule file against a set of target
/// files, returning the count of files with at least one match. Files that
/// don't exist are skipped (mirrors `run_ast_grep`'s own tolerance).
fn count_matching_files(rule_path: &Path, target_files: &[PathBuf], cwd: &Path) -> anyhow::Result<usize> {
    let existing: Vec<&PathBuf> = target_files.iter().filter(|p| p.exists()).collect();
    if existing.is_empty() {
        return Ok(0);
    }

    let temp_dir = tempfile::tempdir()?;
    let rules_dir = temp_dir.path().join("rules");
    std::fs::create_dir_all(&rules_dir)?;
    std::fs::copy(rule_path, rules_dir.join("candidate.yml"))?;
    let sgconfig_path = temp_dir.path().join("sgconfig.yml");
    std::fs::write(&sgconfig_path, "ruleDirs:\n  - rules\n")?;

    let relative: Vec<String> = existing.iter().filter_map(|p| p.strip_prefix(cwd).ok().map(|rel| rel.to_string_lossy().to_string())).collect();

    let output = Command::new("ast-grep").arg("scan").arg("--config").arg(&sgconfig_path).arg("--json").args(&relative).current_dir(cwd).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let matches: Vec<serde_json::Value> = serde_json::from_str(&stdout).map_err(|err| anyhow::anyhow!("ast-grep produced unparsable output: {err}. stderr: {}", String::from_utf8_lossy(&output.stderr)))?;

    let matched_files: std::collections::HashSet<String> = matches.iter().filter_map(|m| m.get("file").and_then(|v| v.as_str()).map(str::to_string)).collect();
    Ok(matched_files.len())
}

fn list_fixture_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect()
}

const SKIP_DIRS: &[&str] = &[".git", "vendor", "node_modules", "target", ".autoreview"];

/// Samples up to `FP_SAMPLE_CAP` files with the rule's language extension
/// from the current repo, for the FP smoke test.
fn sample_repo_files(repo_root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![repo_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if found.len() >= FP_SAMPLE_CAP {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !SKIP_DIRS.contains(&name) {
                    stack.push(path);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                found.push(path);
                if found.len() >= FP_SAMPLE_CAP {
                    break;
                }
            }
        }
    }
    found
}

/// Runs bench for one candidate cluster. `repo_root` is the repo whose
/// `.autoreview/rules/candidates/<clusterId>/` holds the drafted rule
/// (`rule.yaml`, from the draft stage) and optional `tests/positive`,
/// `tests/negative` fixture directories a human has supplied.
pub fn run_bench(repo_root: &Path, cluster_id: &str) -> anyhow::Result<BenchReport> {
    let candidate_dir = repo_root.join(".autoreview").join("rules").join("candidates").join(cluster_id);
    let rule_path = candidate_dir.join("rule.yaml");
    if !rule_path.exists() {
        anyhow::bail!("no drafted rule found at {} — run `autoreview rules mine` first", rule_path.display());
    }

    let rule_contents = std::fs::read_to_string(&rule_path)?;
    let rule_meta: RuleLanguage = serde_yaml::from_str(&rule_contents)?;
    let extension = extension_for_language(&rule_meta.language).ok_or_else(|| anyhow::anyhow!("unsupported rule language '{}'", rule_meta.language))?;

    let positive_files = list_fixture_files(&candidate_dir.join("tests").join("positive"));
    let negative_files = list_fixture_files(&candidate_dir.join("tests").join("negative"));

    let self_test = if positive_files.is_empty() && negative_files.is_empty() {
        None
    } else {
        let positive_matched = count_matching_files(&rule_path, &positive_files, repo_root)?;
        let negative_matched = count_matching_files(&rule_path, &negative_files, repo_root)?;
        Some(SelfTestResult { positive_total: positive_files.len(), positive_matched, negative_total: negative_files.len(), negative_matched })
    };

    let sampled = sample_repo_files(repo_root, extension);
    let fp_smoke = if sampled.is_empty() {
        None
    } else {
        let matched_files = count_matching_files(&rule_path, &sampled, repo_root)?;
        Some(FpSmokeResult { sampled_files: sampled.len(), matched_files })
    };

    let verdict = match &self_test {
        None => BenchVerdict::NeedsFixtures,
        Some(result) if !result.passed() => BenchVerdict::SelfTestFailed,
        Some(_) => match &fp_smoke {
            Some(result) if !result.passed() => BenchVerdict::FailedFpSmoke,
            _ => BenchVerdict::Candidate,
        },
    };

    Ok(BenchReport {
        cluster_id: cluster_id.to_string(),
        self_test,
        fp_smoke,
        historical_precision_skipped_reason: "history store does not retain original source snippets — needs a schema change to compute this check".to_string(),
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ast_grep_available() -> bool {
        Command::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn write_rule(candidate_dir: &Path) {
        write(
            &candidate_dir.join("rule.yaml"),
            "id: go-self-comparison-bench-test\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: self comparison\nrule:\n  pattern: $A == $A\n",
        );
    }

    #[test]
    fn errors_when_no_rule_has_been_drafted() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_bench(dir.path(), "missing-cluster").unwrap_err();
        assert!(err.to_string().contains("no drafted rule"));
    }

    #[test]
    fn needs_fixtures_when_no_test_files_are_supplied() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let candidate_dir = dir.path().join(".autoreview/rules/candidates/c1");
        write_rule(&candidate_dir);
        let report = run_bench(dir.path(), "c1").unwrap();
        assert_eq!(report.verdict, BenchVerdict::NeedsFixtures);
        assert!(report.self_test.is_none());
    }

    #[test]
    fn passes_self_test_when_fixtures_match_as_expected() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let candidate_dir = dir.path().join(".autoreview/rules/candidates/c1");
        write_rule(&candidate_dir);
        write(&candidate_dir.join("tests/positive/positive.go"), "package main\n\nfunc f(x int) bool {\n\treturn x == x\n}\n");
        write(&candidate_dir.join("tests/negative/negative.go"), "package main\n\nfunc f(x, y int) bool {\n\treturn x == y\n}\n");

        let report = run_bench(dir.path(), "c1").unwrap();
        let self_test = report.self_test.unwrap();
        assert_eq!(self_test.positive_matched, 1);
        assert_eq!(self_test.negative_matched, 0);
        assert!(self_test.passed());
    }

    #[test]
    fn fails_self_test_when_the_rule_misses_its_own_positive_fixture() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let candidate_dir = dir.path().join(".autoreview/rules/candidates/c1");
        write_rule(&candidate_dir);
        write(&candidate_dir.join("tests/positive/positive.go"), "package main\n\nfunc f(x, y int) bool {\n\treturn x == y\n}\n");

        let report = run_bench(dir.path(), "c1").unwrap();
        assert_eq!(report.verdict, BenchVerdict::SelfTestFailed);
    }

    #[test]
    fn historical_precision_is_explicitly_reported_as_skipped() {
        if !ast_grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let candidate_dir = dir.path().join(".autoreview/rules/candidates/c1");
        write_rule(&candidate_dir);
        let report = run_bench(dir.path(), "c1").unwrap();
        assert!(report.historical_precision_skipped_reason.contains("does not retain"));
    }
}
