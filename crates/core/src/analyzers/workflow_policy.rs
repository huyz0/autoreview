use std::collections::BTreeMap;
use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};
use serde_yaml::Value;

use super::ast_grep::{RuleCatalogEntry, RuleDefinition};

const ID: &str = "github-actions-mutable-reference";
const SUBCATEGORY: &str = "build.dependency-integrity";
const REFERENCE: &str = "https://docs.github.com/en/actions/reference/security/secure-use#using-third-party-actions";

pub(super) fn catalog_entry() -> RuleCatalogEntry {
    RuleCatalogEntry {
        id: ID.into(), kind: "artifact".into(), category: "security".into(), subcategories: vec![SUBCATEGORY.into()],
    }
}

pub(super) fn definition(id: &str) -> Option<RuleDefinition> {
    if id != ID {
        return None;
    }
    let entry = catalog_entry();
    let details = serde_json::json!({
        "id": ID, "kind": entry.kind, "category": entry.category,
        "enabledBy": "artifactChecks.immutableGithubActionRefs",
        "metadata": {"references": [REFERENCE], "subcategory": [SUBCATEGORY]},
    });
    Some(RuleDefinition {
        id: entry.id, source_label: "builtin".into(), kind: entry.kind, category: entry.category,
        semantic: false, yaml: serde_yaml::to_string(&details).ok()?,
    })
}

pub fn run_workflow_policy(root: &Path, files: &[String]) -> Vec<AgentFinding> {
    let Ok(root) = root.canonicalize() else { return Vec::new() };
    files.iter().filter(|path| workflow_path(Path::new(path))).flat_map(|path| {
        let requested = root.join(path);
        let Some(resolved) = requested.canonicalize().ok().filter(|file| file.starts_with(&root)) else {
            return Vec::new();
        };
        if !requested.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
            return Vec::new();
        }
        std::fs::read_to_string(resolved).ok().map_or_else(Vec::new, |source| check_workflow(path, &source))
    }).collect()
}

fn workflow_path(path: &Path) -> bool {
    path.parent() == Some(Path::new(".github/workflows"))
        && matches!(path.extension().and_then(|ext| ext.to_str()), Some("yml" | "yaml"))
}

fn check_workflow(path: &str, source: &str) -> Vec<AgentFinding> {
    let Ok(workflow) = serde_yaml::from_str::<Value>(source) else { return Vec::new() };
    let Some(jobs) = workflow.get("jobs").and_then(Value::as_mapping) else { return Vec::new() };
    let mut counts = BTreeMap::new();
    for job in jobs.values() {
        for reference in job_references(job).filter(|reference| mutable_github_ref(reference)) {
            *counts.entry(reference).or_insert(0usize) += 1;
        }
    }
    let mut sites = BTreeMap::<_, Vec<_>>::new();
    for (line, content) in source.lines().enumerate() {
        let Some(reference) = uses_on_line(content) else { continue };
        if counts.contains_key(reference.as_str()) {
            sites.entry(reference).or_default().push((line, content));
        }
    }
    let mut findings = Vec::new();
    for (reference, locations) in sites {
        // Match whole scalar values and count occurrences to avoid assigning
        // a parsed action to an unrelated key with the same spelling.
        if counts.get(reference.as_str()) != Some(&locations.len()) {
            continue;
        }
        findings.extend(locations.into_iter().map(|(line, source)| finding(path, &reference, line, source)));
    }
    findings.sort_by_key(|finding| finding.location.range.start_line);
    findings
}

fn job_references(job: &Value) -> impl Iterator<Item = &str> {
    let reusable_job = job.get("uses").and_then(Value::as_str);
    let steps = job.get("steps").and_then(Value::as_sequence).into_iter().flatten();
    reusable_job.into_iter().chain(steps.filter_map(|step| step.get("uses").and_then(Value::as_str)))
}

fn mutable_github_ref(value: &str) -> bool {
    let Some((repository, reference)) = value.rsplit_once('@') else { return false };
    let mut parts = repository.split('/');
    let valid_segment = |part: &str| {
        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else { return false };
    owner != "." && owner != ".." && valid_segment(owner) && valid_segment(repo)
        && !reference.is_empty()
        && !(reference.len() == 40 && reference.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn uses_on_line(line: &str) -> Option<String> {
    let line = line.trim_start();
    let key = line.strip_prefix("- ").unwrap_or(line).trim_start();
    let scalar = key.strip_prefix("uses:")?;
    serde_yaml::from_str::<String>(scalar).ok()
}

fn finding(path: &str, reference: &str, line: usize, source: &str) -> AgentFinding {
    AgentFinding {
        source: FindingSource {
            kind: FindingSourceKind::Analyzer,
            tool: "autoreview-workflow-policy".to_owned(),
            rule_id: Some(ID.to_owned()),
            aspect: None,
            backend: None,
        },
        category: "security".to_owned(),
        severity: Severity::Medium,
        confidence: 1.0,
        title: "GitHub Actions dependency uses a mutable reference".to_owned(),
        message: format!("Pin {reference} to the full commit SHA for the intended revision."),
        location: Location {
            path: path.to_owned(),
            range: LocationRange { start_line: line as u32 + 1, ..Default::default() },
            snippet: source.trim().to_owned(),
            side: Side::New,
        },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: Some(std::collections::HashMap::from([
            ("references".to_owned(), serde_json::json!([REFERENCE])),
            ("subcategory".to_owned(), serde_json::json!([SUBCATEGORY])),
        ])),
        suggested_patch: None,
    }
}
