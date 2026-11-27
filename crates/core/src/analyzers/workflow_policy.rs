use std::collections::BTreeMap;
use std::path::Path;

use autoreview_schema::{AgentFinding, LocationRange};
use serde_yaml::Value;

use super::artifact_policy::{read_snapshot_file, Rule};

pub(super) const RULE: Rule = Rule {
    id: "github-actions-mutable-reference",
    category: "security",
    subcategory: "build.dependency-integrity",
    tool: "autoreview-workflow-policy",
    title: "GitHub Actions dependency uses a mutable reference",
    reference: "https://docs.github.com/en/actions/reference/security/secure-use#using-third-party-actions",
    enabled_by: "artifactChecks.immutableGithubActionRefs",
};

pub fn run_workflow_policy(root: &Path, files: &[String]) -> Vec<AgentFinding> {
    review_workflows(root, files, check_workflow)
}

pub(super) fn review_workflows(
    root: &Path,
    files: &[String],
    analyze: impl Fn(&str, &str) -> Vec<AgentFinding>,
) -> Vec<AgentFinding> {
    let Ok(root) = root.canonicalize() else { return Vec::new() };
    files.iter().filter(|path| workflow_path(Path::new(path))).flat_map(|path| {
        read_snapshot_file(&root, Path::new(path))
            .map_or_else(Vec::new, |source| analyze(path, &source))
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
    RULE.finding(path, LocationRange { start_line: line as u32 + 1, ..Default::default() },
        source, format!("Pin {reference} to the full commit SHA for the intended revision."))
}
