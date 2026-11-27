use std::path::Path;

use autoreview_schema::{AgentFinding, ArtifactChecksConfig, Location, LocationRange, Side};

use super::artifact_policy::Rule;
use super::workflow_policy::review_workflows;

mod marked_yaml;
use marked_yaml::{workflow, Node};

pub(super) const SCRIPT_INPUT: Rule = Rule {
    id: "github-actions-script-input",
    category: "security",
    subcategory: "build.script-injection",
    tool: "autoreview-workflow-policy",
    title: "GitHub event text is substituted directly into a workflow script",
    reference: "https://docs.github.com/en/actions/concepts/security/script-injections",
    enabled_by: "artifactChecks.githubActionsScriptInputs",
};

pub(super) const WRITE_ALL: Rule = Rule {
    id: "github-actions-write-all",
    category: "security",
    subcategory: "build.least-privilege",
    tool: "autoreview-workflow-policy",
    title: "GitHub Actions job requests write-all token permissions",
    reference: "https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#permissions",
    enabled_by: "artifactChecks.githubActionsLeastPrivilege",
};

fn direct_event_input(script: &str) -> Option<String> {
    script.match_indices("${{").find_map(|(start, _)| {
        let (expression, _) = script[start + 3..].split_once("}}")?;
        let path = expression.trim().to_ascii_lowercase();
        let recognized = matches!(
            path.as_str(),
            "github.head_ref"
                | "github.event.comment.body"
                | "github.event.discussion.body"
                | "github.event.discussion.title"
                | "github.event.head_commit.author.email"
                | "github.event.head_commit.message"
                | "github.event.issue.body"
                | "github.event.issue.title"
                | "github.event.label.name"
                | "github.event.pull_request.body"
                | "github.event.pull_request.head.ref"
                | "github.event.pull_request.title"
                | "github.event.review.body"
        );
        recognized.then_some(path)
    })
}

struct WorkflowSource<'a> {
    path: &'a str,
    lines: Vec<&'a str>,
}

impl WorkflowSource<'_> {
    fn finding(&self, rule: &Rule, key: &Node, end: u32, message: String) -> AgentFinding {
        let range = LocationRange {
            start_line: key.line,
            end_line: Some(end),
            ..Default::default()
        };
        let snippet = self
            .lines
            .get(key.line.saturating_sub(1) as usize)
            .copied()
            .unwrap_or_default();
        rule.finding(self.path, range, snippet, message)
    }

    fn script_findings(&self, jobs: &Node) -> Vec<AgentFinding> {
        let mut findings = Vec::new();
        for (name, job) in jobs.fields() {
            let Some(name) = name.text() else { continue };
            let Some((_, steps)) = job.field("steps") else {
                continue;
            };
            for step in steps.sequence() {
                let Some((key, run)) = step.field("run") else {
                    continue;
                };
                let Some(input) = run.text().and_then(direct_event_input) else {
                    continue;
                };
                findings.push(self.finding(
                    &SCRIPT_INPUT, key, run.end_line,
                    format!("Job {name} substitutes {input} before the script runs. Pass event text through an environment variable and read it as data in the script."),
                ));
            }
        }
        findings
    }

    fn permission_findings(&self, workflow: &Node, jobs: &Node) -> Vec<AgentFinding> {
        let mut findings = Vec::new();
        let mut inheriting = Vec::new();
        for (job_key, job) in jobs
            .fields()
            .iter()
            .filter(|(_, job)| !job.fields().is_empty())
        {
            let Some(name) = job_key.text() else { continue };
            match job.field("permissions") {
                Some((key, value)) if value.text() == Some("write-all") => {
                    findings.push(self.finding(&WRITE_ALL, key, value.line,
                        format!("Job {name} requests write access to every available token scope. Declare the permissions required by this job.")));
                }
                None if !job
                    .fields()
                    .iter()
                    .any(|(key, _)| key.text() == Some("permissions")) =>
                {
                    inheriting.push(Location {
                        path: self.path.into(),
                        range: LocationRange {
                            start_line: job_key.line,
                            ..Default::default()
                        },
                        snippet: name.into(),
                        side: Side::New,
                    });
                }
                _ => {}
            }
        }
        if let Some((key, value)) = workflow
            .field("permissions")
            .filter(|(_, value)| !inheriting.is_empty() && value.text() == Some("write-all"))
        {
            let mut inherited = self.finding(&WRITE_ALL, key, value.line,
                "Jobs without a permissions override inherit write access to every available token scope. Declare the permissions required by those jobs.".into());
            inherited.related_locations = Some(inheriting);
            findings.push(inherited);
        }
        findings
    }
}

fn check_workflow(path: &str, source: &str, checks: &ArtifactChecksConfig) -> Vec<AgentFinding> {
    let Some(workflow) = workflow(source) else {
        return Vec::new();
    };
    let Some((_, jobs)) = workflow.field("jobs") else {
        return Vec::new();
    };
    let source = WorkflowSource {
        path,
        lines: source.lines().collect(),
    };
    let mut findings = Vec::new();
    if checks.github_actions_script_inputs {
        findings.extend(source.script_findings(jobs));
    }
    if checks.github_actions_least_privilege {
        findings.extend(source.permission_findings(&workflow, jobs));
    }
    findings
}

pub fn run_workflow_security(
    root: &Path,
    files: &[String],
    checks: &ArtifactChecksConfig,
) -> Vec<AgentFinding> {
    if !checks.github_actions_script_inputs && !checks.github_actions_least_privilege {
        return Vec::new();
    }
    review_workflows(root, files, |path, source| {
        check_workflow(path, source, checks)
    })
}
