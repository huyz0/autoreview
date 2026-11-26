use super::*;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Evidence {
    kind: EvidenceKind,
    pub(super) source: String,
    pub(super) license: String,
    pub(super) rationale: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EvidenceKind {
    Historical,
    Synthetic,
}


#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReplayReport {
    pub(super) rules: Vec<RuleScore>,
    languages: Vec<LanguageScore>,
    evidence_kinds: Vec<EvidenceScore>,
    coverage: Vec<CoverageScore>,
    cases: Vec<CaseResult>,
    pub(super) failures: Vec<CaseFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    custom_rule_snapshots: Vec<super::packs::RuleSnapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GateReport {
    pub(super) base: String,
    pub(super) head: String,
    pub(super) evidence: Vec<RuleScore>,
    pub(super) passed: bool,
    pub(super) findings: Vec<AgentFinding>,
}

#[derive(Serialize)]
struct LanguageScore {
    language: String,
    rules: Vec<RuleScore>,
}

#[derive(Serialize)]
struct EvidenceScore {
    kind: String,
    rules: Vec<RuleScore>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoverageCounts {
    attempted: usize,
    completed: usize,
    incomplete: usize,
    failed: usize,
}

#[derive(Serialize)]
struct CoverageScore {
    rule: Rule,
    language: String,
    #[serde(flatten)]
    counts: CoverageCounts,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum FailureKind {
    IncompleteAnalysis,
    Error,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CaseFailure {
    name: String,
    rule: Rule,
    language: String,
    before_path: String,
    after_path: String,
    error: String,
    kind: FailureKind,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    analysis_coverage: Vec<autoreview_schema::AnalysisCoverageIssue>,
}

pub(super) fn replay_report(repo: &Path, suite: &Suite, frozen: &super::packs::FrozenRules) -> anyhow::Result<ReplayReport> {
    ensure!(!suite.cases.is_empty(), "the suite has no cases for the selected rules");
    let mut names = BTreeSet::new();
    let mut snapshots = BTreeSet::new();
    let mut cases = Vec::new();
    let mut failures = Vec::new();
    let mut cache = BTreeMap::new();
    for case in &suite.cases {
        let outcome: anyhow::Result<CaseResult> = (|| {
            if let Some(error) = frozen.errors.get(&case.rule.id) { anyhow::bail!("{error}"); }
            ensure!(!case.name.trim().is_empty() && names.insert(&case.name),
                "case names must be unique and nonempty: '{}'", case.name);
            let single = Suite { cases: vec![case.clone()], gate: BTreeSet::new() };
            let result = replay_with_cache(repo, &single, false, &mut cache, &frozen.packs)?.pop().expect("a single replay case");
            let keys: Vec<_> = [&result.before, &result.after].into_iter()
                .map(|side| (case.rule.clone(), side.commit.clone(), side.path.clone())).collect();
            for key in &keys {
                ensure!(!snapshots.contains(key),
                    "case '{}' repeats the same rule, commit and path", case.name);
            }
            snapshots.extend(keys);
            Ok(result)
        })();
        match outcome {
            Ok(result) => cases.push(result),
            Err(error) => failures.push(CaseFailure {
                name: case.name.clone(), rule: case.rule.clone(),
                language: case_language(&case.before.path, &case.after.path),
                before_path: case.before.path.clone(), after_path: case.after.path.clone(),
                kind: if error.downcast_ref::<super::rule::IncompleteAnalysis>().is_some() { FailureKind::IncompleteAnalysis } else { FailureKind::Error },
                analysis_coverage: error.downcast_ref::<super::rule::IncompleteAnalysis>().map_or_else(Vec::new, |incomplete| incomplete.0.clone()),
                error: format!("{error:#}"),
            }),
        }
    }
    let mut groups: BTreeMap<String, Vec<CaseResult>> = BTreeMap::new();
    let mut evidence_groups: BTreeMap<String, Vec<CaseResult>> = BTreeMap::new();
    for case in &cases {
        let language = case_language(&case.before.path, &case.after.path);
        groups.entry(language).or_default().push(case.clone());
        let kind = match case.evidence.as_ref().map(|evidence| &evidence.kind) {
            Some(EvidenceKind::Historical) => "historical",
            Some(EvidenceKind::Synthetic) => "synthetic",
            None => "unclassified",
        };
        evidence_groups.entry(kind.to_owned()).or_default().push(case.clone());
    }
    let languages = groups.into_iter().map(|(language, cases)| LanguageScore {
        language, rules: scores(&cases),
    }).collect();
    let evidence_kinds = evidence_groups.into_iter().map(|(kind, cases)| EvidenceScore {
        kind, rules: scores(&cases),
    }).collect();
    let coverage = coverage_scores(&cases, &failures);
    Ok(ReplayReport { rules: scores(&cases), languages, evidence_kinds, coverage, cases, failures, custom_rule_snapshots: frozen.snapshots.clone() })
}

fn case_language(before: &str, after: &str) -> String {
    let before = language_for_path(Path::new(before));
    let after = language_for_path(Path::new(after));
    if before == after {
        before.map_or_else(|| "unknown".to_owned(), |language| format!("{language:?}"))
    } else {
        "mixed".to_owned()
    }
}


fn coverage_scores(cases: &[CaseResult], failures: &[CaseFailure]) -> Vec<CoverageScore> {
    let mut groups = BTreeMap::<(Rule, String), CoverageCounts>::new();
    for case in cases {
        let counts = groups.entry((case.rule.clone(), case_language(&case.before.path, &case.after.path))).or_default();
        counts.attempted += 1;
        counts.completed += 1;
    }
    for failure in failures {
        let counts = groups.entry((failure.rule.clone(), failure.language.clone())).or_default();
        counts.attempted += 1;
        match failure.kind {
            FailureKind::IncompleteAnalysis => counts.incomplete += 1,
            FailureKind::Error => counts.failed += 1,
        }
    }
    groups.into_iter().map(|((rule, language), counts)| CoverageScore { rule, language, counts }).collect()
}
