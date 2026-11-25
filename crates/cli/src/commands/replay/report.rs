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
    cases: Vec<CaseResult>,
    pub(super) failures: Vec<CaseFailure>,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CaseFailure {
    name: String,
    rule: Rule,
    language: String,
    before_path: String,
    after_path: String,
    error: String,
}

pub(super) fn replay_report(repo: &Path, suite: &Suite) -> anyhow::Result<ReplayReport> {
    ensure!(!suite.cases.is_empty(), "the suite has no cases for the selected rules");
    let mut names = BTreeSet::new();
    let mut snapshots = BTreeSet::new();
    let mut cases = Vec::new();
    let mut failures = Vec::new();
    let mut cache = BTreeMap::new();
    for case in &suite.cases {
        let outcome: anyhow::Result<CaseResult> = (|| {
            ensure!(!case.name.trim().is_empty() && names.insert(&case.name),
                "case names must be unique and nonempty: '{}'", case.name);
            let single = Suite { cases: vec![case.clone()], gate: BTreeSet::new() };
            let result = replay_with_cache(repo, &single, false, &mut cache)?.pop().expect("a single replay case");
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
    Ok(ReplayReport { rules: scores(&cases), languages, evidence_kinds, cases, failures })
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

