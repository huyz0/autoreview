use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::process::Command;

use anyhow::{ensure, Context};
use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::AgentFinding;
use serde::{Deserialize, Serialize};

use super::diff_lines::ChangedLines;
use super::revision::{checkout_snapshot, resolve_commit};

const SUITE_PATH: &str = ".autoreview/replay.yaml";
const MIN_GATE_PAIRS: usize = 3;
const MIN_GATE_PRECISION: usize = 95;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Tool {
    AstGrep,
    AutoreviewComplexity,
    AutoreviewDataflow,
    AutoreviewDuplication,
    AutoreviewRust,
}

impl Tool {
    fn scan(self, source: &Path, paths: &[String]) -> anyhow::Result<Vec<AgentFinding>> {
        let findings = match self {
            Self::AstGrep => {
                ensure!(
                    Command::new("ast-grep")
                        .arg("--version")
                        .output()
                        .is_ok_and(|output| output.status.success()),
                    "ast-grep is required for this rule"
                );
                autoreview_core::run_ast_grep(source, paths, &[])?
            }
            Self::AutoreviewComplexity => autoreview_core::run_complexity_check(source, paths, &[]),
            Self::AutoreviewDataflow => autoreview_core::run_dataflow_check(source, paths, &[]),
            Self::AutoreviewDuplication => autoreview_core::run_duplication_check(source, paths),
            Self::AutoreviewRust => autoreview_core::run_rust_review(source, paths),
        };
        Ok(findings)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    tool: Tool,
    id: String,
}

impl Rule {
    fn matches(&self, finding: &AgentFinding) -> bool {
        finding.source.rule_id.as_deref() == Some(&self.id)
    }

    fn check_replay_support(&self) -> anyhow::Result<()> {
        let definition = autoreview_core::find_rule_definition(&self.id, &[]);
        let kind = definition.as_ref().map(|rule| rule.kind.as_str());
        let supported = match self.tool {
            Tool::AstGrep => kind == Some("pattern"),
            Tool::AutoreviewComplexity => kind == Some("threshold"),
            Tool::AutoreviewDataflow => {
                matches!(kind, Some("taint" | "call-sequence"))
                    || matches!(
                        self.id.as_str(),
                        "typed-nil-interface-return"
                            | "append-shared-backing-array"
                            | "npe-risk-from-helper-return"
                            | "loopvar-capture-pre-1.22"
                            | "loopvar-address-pre-1.22"
                    )
            }
            Tool::AutoreviewDuplication => self.id == "duplicate-code-block",
            Tool::AutoreviewRust => autoreview_core::rust_review_rule(&self.id).is_some(),
        };
        ensure!(
            supported,
            "rule '{}' is unavailable for replay with {:?}",
            self.id,
            self.tool
        );
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Suite {
    cases: Vec<Case>,
    #[serde(default)]
    gate: BTreeSet<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    rule: Rule,
    before: Label,
    after: Label,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Label {
    commit: String,
    path: String,
    lines: Vec<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotResult {
    commit: String,
    path: String,
    matched_lines: Vec<u32>,
    unexpected_lines: Vec<u32>,
    missed_lines: Vec<u32>,
}

#[derive(Serialize)]
struct CaseResult {
    name: String,
    rule: Rule,
    before: SnapshotResult,
    after: SnapshotResult,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Score {
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    precision: Option<f64>,
    recall: Option<f64>,
    distinct_fix_pairs: usize,
}

#[derive(Serialize)]
struct RuleScore {
    rule: Rule,
    #[serde(flatten)]
    score: Score,
}

fn compare_lines(
    actual: Vec<u32>,
    expected: &[u32],
    commit: String,
    path: String,
) -> SnapshotResult {
    let mut remaining = expected.to_vec();
    let mut matched_lines = Vec::new();
    let mut unexpected_lines = Vec::new();
    for line in actual {
        if let Some(index) = remaining.iter().position(|expected| *expected == line) {
            remaining.remove(index);
            matched_lines.push(line);
        } else {
            unexpected_lines.push(line);
        }
    }
    matched_lines.sort_unstable();
    unexpected_lines.sort_unstable();
    remaining.sort_unstable();
    SnapshotResult {
        commit,
        path,
        matched_lines,
        unexpected_lines,
        missed_lines: remaining,
    }
}

fn replay_side(
    repo: &Path,
    snapshots: &mut BTreeMap<String, tempfile::TempDir>,
    rule: &Rule,
    label: &Label,
    pinned: bool,
) -> anyhow::Result<SnapshotResult> {
    let path = Path::new(&label.path);
    ensure!(
        !label.path.is_empty()
            && !label.path.contains('\\')
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "expected a repository-relative path, got '{}'",
        label.path
    );
    let commit = resolve_commit(repo, &label.commit)?;
    ensure!(
        !pinned || commit == label.commit,
        "gate replay commit '{}' must use its full object ID ({commit})",
        label.commit
    );
    if !snapshots.contains_key(&commit) {
        if snapshots.len() == 2 {
            snapshots.pop_first();
        }
        snapshots.insert(commit.clone(), checkout_snapshot(repo, &commit)?);
    }
    let snapshot = snapshots[&commit].path();
    let file = snapshot.join(path);
    ensure!(
        file.symlink_metadata().is_ok_and(|meta| meta.is_file())
            && file.canonicalize()?.starts_with(snapshot.canonicalize()?),
        "'{}' must be a regular source file in {commit}",
        label.path
    );
    let source = std::fs::read_to_string(&file)?;
    if rule.tool != Tool::AutoreviewDuplication {
        ensure!(
            autoreview_core::parses_cleanly(path, &source) == Some(true),
            "'{}' must parse successfully in {commit}",
            label.path
        );
    }
    ensure!(
        label
            .lines
            .iter()
            .all(|line| (1..=source.lines().count()).contains(&(*line as usize))),
        "label contains a line outside '{}' at {commit}",
        label.path
    );
    let lines = rule
        .tool
        .scan(snapshot, std::slice::from_ref(&label.path))?
        .into_iter()
        .filter(|finding| finding.location.path == label.path && rule.matches(finding))
        .map(|finding| finding.location.range.start_line)
        .collect();
    Ok(compare_lines(
        lines,
        &label.lines,
        commit,
        label.path.clone(),
    ))
}

fn replay(repo: &Path, suite: &Suite, gate_only: bool) -> anyhow::Result<Vec<CaseResult>> {
    let mut cases = Vec::new();
    let mut snapshots = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut labelled_snapshots = BTreeSet::new();
    for case in &suite.cases {
        if gate_only && !suite.gate.contains(&case.rule) {
            continue;
        }
        ensure!(
            !case.name.trim().is_empty() && names.insert(case.name.as_str()),
            "case names must be unique and nonempty: '{}'",
            case.name
        );
        case.rule
            .check_replay_support()
            .with_context(|| format!("case '{}'", case.name))?;
        let before = replay_side(repo, &mut snapshots, &case.rule, &case.before, gate_only)
            .with_context(|| format!("case '{}' before", case.name))?;
        let after = replay_side(repo, &mut snapshots, &case.rule, &case.after, gate_only)
            .with_context(|| format!("case '{}' after", case.name))?;
        ensure!(
            before.commit != after.commit,
            "case '{}' must compare different commits",
            case.name
        );
        let ancestor = Command::new("git")
            .args(["merge-base", "--is-ancestor", &before.commit, &after.commit])
            .current_dir(repo)
            .status()?;
        ensure!(
            ancestor.success(),
            "case '{}' must name an after commit descended from its before commit",
            case.name
        );
        for side in [&before, &after] {
            ensure!(
                labelled_snapshots.insert((
                    case.rule.clone(),
                    side.commit.clone(),
                    side.path.clone()
                )),
                "case '{}' repeats the same rule, commit and path",
                case.name
            );
        }
        cases.push(CaseResult {
            name: case.name.clone(),
            rule: case.rule.clone(),
            before,
            after,
        });
    }
    ensure!(
        !cases.is_empty(),
        "the suite has no cases for the selected rules"
    );
    Ok(cases)
}

fn scores(cases: &[CaseResult]) -> Vec<RuleScore> {
    #[derive(Default)]
    struct Tally<'a> {
        score: Score,
        fixes: BTreeSet<(&'a str, &'a str)>,
    }
    let mut totals: BTreeMap<&Rule, Tally<'_>> = BTreeMap::new();
    for case in cases {
        let Tally { score, fixes } = totals.entry(&case.rule).or_default();
        for side in [&case.before, &case.after] {
            score.true_positives += side.matched_lines.len();
            score.false_positives += side.unexpected_lines.len();
            score.false_negatives += side.missed_lines.len();
        }
        if !case.before.matched_lines.is_empty()
            && case.after.matched_lines.is_empty()
            && case.after.missed_lines.is_empty()
            && case.after.unexpected_lines.is_empty()
        {
            fixes.insert((&case.before.commit, &case.after.commit));
        }
    }
    totals
        .into_iter()
        .map(|(rule, Tally { mut score, fixes })| {
            let emitted = score.true_positives + score.false_positives;
            let positives = score.true_positives + score.false_negatives;
            score.precision = (emitted > 0).then(|| score.true_positives as f64 / emitted as f64);
            score.recall = (positives > 0).then(|| score.true_positives as f64 / positives as f64);
            score.distinct_fix_pairs = fixes.len();
            RuleScore {
                rule: rule.clone(),
                score,
            }
        })
        .collect()
}

#[derive(Serialize)]
struct ReplayReport {
    rules: Vec<RuleScore>,
    cases: Vec<CaseResult>,
}

pub(crate) fn run_replay(repo: &Path, suite: &Path) -> anyhow::Result<()> {
    let suite: Suite = serde_yaml::from_slice(&std::fs::read(repo.join(suite))?)?;
    let cases = replay(repo, &suite, false)?;
    let report = ReplayReport {
        rules: scores(&cases),
        cases,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        report
            .rules
            .iter()
            .all(|entry| entry.score.false_negatives == 0 && entry.score.false_positives == 0),
        "rule replay did not match the expected findings"
    );
    Ok(())
}

fn validate_gate_rule(rule: &Rule) -> anyhow::Result<Vec<Language>> {
    if rule.tool == Tool::AutoreviewRust {
        let definition = autoreview_core::rust_review_rule(&rule.id)
            .with_context(|| format!("gate rule '{}' is not a built-in Rust review rule", rule.id))?;
        ensure!(!definition.semantic && matches!(definition.category, "correctness" | "security"),
            "gate rule '{}' must be a non-heuristic built-in correctness or security rule", rule.id);
        return Ok(vec![Language::Rust]);
    }
    let definition = autoreview_core::find_rule_definition(&rule.id, &[])
        .with_context(|| format!("gate rule '{}' is not a built-in rule", rule.id))?;
    let correct_tool = match rule.tool {
        Tool::AstGrep => definition.kind == "pattern",
        Tool::AutoreviewDataflow => matches!(definition.kind.as_str(), "taint" | "call-sequence"),
        _ => false,
    };
    ensure!(correct_tool && !definition.semantic && matches!(definition.category.as_str(), "correctness" | "security"),
        "gate rule '{}' must be a non-heuristic built-in correctness or security rule backed by ast-grep or dataflow", rule.id);
    #[derive(Deserialize)]
    struct RuleLanguage {
        language: String,
    }
    let language: RuleLanguage = serde_yaml::from_str(&definition.yaml)?;
    match language.language.as_str() {
        "Go" => Ok(vec![Language::Go]),
        "Java" => Ok(vec![Language::Java]),
        "Kotlin" => Ok(vec![Language::Kotlin]),
        "TypeScript" => Ok(vec![Language::TypeScript, Language::Tsx]),
        "Tsx" => Ok(vec![Language::Tsx]),
        "JavaScript" => Ok(vec![Language::JavaScript]),
        "Rust" => Ok(vec![Language::Rust]),
        _ => anyhow::bail!("gate rule '{}' has an unsupported language", rule.id),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GateReport {
    base: String,
    head: String,
    evidence: Vec<RuleScore>,
    passed: bool,
    findings: Vec<AgentFinding>,
}

pub(crate) fn run_gate(repo: &Path, base: &str, head: &str) -> anyhow::Result<()> {
    let base = resolve_commit(repo, base)?;
    let head = resolve_commit(repo, head)?;
    let policy = Command::new("git")
        .args(["show", &format!("{base}:{SUITE_PATH}")])
        .current_dir(repo)
        .output()?;
    ensure!(
        policy.status.success(),
        "the base commit {base} must contain {SUITE_PATH}"
    );
    let suite: Suite =
        serde_yaml::from_slice(&policy.stdout).context("invalid gate policy in base commit")?;
    ensure!(
        !suite.gate.is_empty(),
        "the base policy must name at least one gate rule"
    );
    let mut tool_languages = BTreeMap::<Tool, Vec<Language>>::new();
    for rule in &suite.gate {
        tool_languages
            .entry(rule.tool)
            .or_default()
            .extend(validate_gate_rule(rule)?);
    }
    let evidence = scores(&replay(repo, &suite, true)?);
    for rule in &suite.gate {
        let qualified = evidence
            .iter()
            .find(|entry| &entry.rule == rule)
            .is_some_and(|entry| {
                let score = &entry.score;
                score.distinct_fix_pairs >= MIN_GATE_PAIRS
                    && score.false_negatives == 0
                    && score
                        .precision
                        .is_some_and(|precision| precision >= MIN_GATE_PRECISION as f64 / 100.0)
            });
        if !qualified {
            println!("{}", serde_json::to_string_pretty(&evidence)?);
            anyhow::bail!("gate rule '{}' needs {MIN_GATE_PAIRS} distinct verified fix pairs, at least {MIN_GATE_PRECISION}% labelled precision, and zero missed labels", rule.id);
        }
    }

    let facts = autoreview_core::collect_diff_facts(&repo.to_string_lossy(), &base, &head, None)?;
    let lines = ChangedLines::load(repo, &base, &head)?;
    let snapshot = checkout_snapshot(repo, &head)?;
    let root = snapshot.path().canonicalize()?;
    let mut findings = Vec::new();
    for (tool, languages) in tool_languages {
        let mut paths = Vec::new();
        for file in &facts.files {
            let relative = Path::new(&file.path);
            if !language_for_path(relative).is_some_and(|language| languages.contains(&language)) {
                continue;
            }
            let file_path = snapshot.path().join(relative);
            match file_path.symlink_metadata() {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Ok(meta) if meta.is_file() && file_path.canonicalize()?.starts_with(&root) => {}
                _ => anyhow::bail!("cannot analyze '{}' as a regular source file", file.path),
            }
            let source = std::fs::read_to_string(&file_path)?;
            ensure!(
                autoreview_core::parses_cleanly(relative, &source) == Some(true),
                "cannot qualify gate coverage for '{}': source does not parse",
                file.path
            );
            paths.push(file.path.clone());
        }
        if paths.is_empty() {
            continue;
        }
        findings.extend(
            tool.scan(snapshot.path(), &paths)?
                .into_iter()
                .filter(|finding| {
                    lines.contains_finding(finding)
                        && suite
                            .gate
                            .iter()
                            .any(|rule| rule.tool == tool && rule.matches(finding))
                }),
        );
    }
    findings.sort_by(|left, right| {
        (
            &left.location.path,
            left.location.range.start_line,
            &left.source.rule_id,
        )
            .cmp(&(
                &right.location.path,
                right.location.range.start_line,
                &right.source.rule_id,
            ))
    });
    let report = GateReport {
        base,
        head,
        evidence,
        passed: findings.is_empty(),
        findings,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        report.passed,
        "merge gate found {} issue(s) on changed lines",
        report.findings.len()
    );
    Ok(())
}
