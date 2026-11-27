use std::path::Path;
use std::str::Chars;

use autoreview_schema::{AgentFinding, ArtifactChecksConfig, Location, LocationRange, Side};
use yaml_rust2::parser::{Event, Parser};

use super::artifact_policy::{read_snapshot_file, Rule};
use super::workflow_policy::workflow_path;

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

enum Value {
    Text(String),
    Mapping(Vec<(Node, Node)>),
    Sequence(Vec<Node>),
    Unsupported,
}

struct Node {
    value: Value,
    line: u32,
    end_line: u32,
}

impl Node {
    fn text(&self) -> Option<&str> {
        match &self.value {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    fn fields(&self) -> &[(Node, Node)] {
        match &self.value {
            Value::Mapping(fields) => fields,
            _ => &[],
        }
    }

    fn field(&self, name: &str) -> Option<(&Node, &Node)> {
        let mut matches = self.fields().iter().filter(|(key, _)| key.text() == Some(name));
        let (key, value) = matches.next()?;
        matches.next().is_none().then_some((key, value))
    }

    fn sequence(&self) -> &[Node] {
        match &self.value {
            Value::Sequence(nodes) => nodes,
            _ => &[],
        }
    }
}

fn parse_node(parser: &mut Parser<Chars<'_>>, depth: usize) -> Option<Node> {
    if depth >= 64 {
        return None;
    }
    let (event, marker) = parser.next_token().ok()?;
    let value = match event {
        Event::Scalar(text, _, _, None) => Value::Text(text),
        Event::MappingStart(_, _) => {
            let mut fields = Vec::new();
            while parser.peek().ok()?.0 != Event::MappingEnd {
                fields.push((parse_node(parser, depth + 1)?, parse_node(parser, depth + 1)?));
            }
            parser.next_token().ok()?;
            Value::Mapping(fields)
        }
        Event::SequenceStart(_, _) => {
            let mut nodes = Vec::new();
            while parser.peek().ok()?.0 != Event::SequenceEnd {
                nodes.push(parse_node(parser, depth + 1)?);
            }
            parser.next_token().ok()?;
            Value::Sequence(nodes)
        }
        Event::Alias(_) | Event::Scalar(..) => Value::Unsupported,
        _ => return None,
    };
    let line = marker.line() as u32;
    let end_line = (parser.peek().ok()?.1.line() as u32).saturating_sub(1).max(line);
    Some(Node { value, line, end_line })
}

fn workflow(source: &str) -> Option<Node> {
    let mut parser = Parser::new_from_str(source);
    if parser.next_token().ok()?.0 != Event::StreamStart
        || parser.next_token().ok()?.0 != Event::DocumentStart
    {
        return None;
    }
    let root = parse_node(&mut parser, 0)?;
    (parser.next_token().ok()?.0 == Event::DocumentEnd
        && parser.next_token().ok()?.0 == Event::StreamEnd)
        .then_some(root)
}

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

fn finding(
    rule: &Rule, path: &str, source: &[&str], key: &Node, end: u32, message: String,
) -> AgentFinding {
    let range = LocationRange { start_line: key.line, end_line: Some(end), ..Default::default() };
    let snippet = source.get(key.line.saturating_sub(1) as usize).copied().unwrap_or_default();
    rule.finding(path, range, snippet, message)
}

fn check_workflow(path: &str, source: &str, checks: &ArtifactChecksConfig) -> Vec<AgentFinding> {
    let Some(workflow) = workflow(source) else { return Vec::new() };
    let Some((_, jobs)) = workflow.field("jobs") else { return Vec::new() };
    let lines: Vec<_> = source.lines().collect();
    let mut findings = Vec::new();
    let mut inheriting_jobs = Vec::new();
    for (job_key, job) in jobs.fields() {
        let Some(name) = job_key.text() else { continue };
        if checks.github_actions_script_inputs {
            for step in job.field("steps").into_iter().flat_map(|(_, steps)| steps.sequence()) {
                let Some((key, run)) = step.field("run") else { continue };
                let Some(input) = run.text().and_then(direct_event_input) else { continue };
                findings.push(finding(
                    &SCRIPT_INPUT, path, &lines, key, run.end_line,
                    format!("Job {name} substitutes {input} before the script runs. Pass event text through an environment variable and read it as data in the script."),
                ));
            }
        }
        if checks.github_actions_least_privilege && !job.fields().is_empty() {
            match job.field("permissions") {
                Some((key, value)) if value.text() == Some("write-all") => {
                    findings.push(finding(&WRITE_ALL, path, &lines, key, value.line,
                        format!("Job {name} requests write access to every available token scope. Declare the permissions required by this job.")));
                }
                None if !job.fields().iter().any(|(key, _)| key.text() == Some("permissions")) => {
                    inheriting_jobs.push(Location {
                        path: path.into(),
                        range: LocationRange { start_line: job_key.line, ..Default::default() },
                        snippet: name.into(),
                        side: Side::New,
                    });
                }
                _ => {}
            }
        }
    }
    if let Some((key, value)) = workflow.field("permissions")
        .filter(|(_, value)| !inheriting_jobs.is_empty() && value.text() == Some("write-all"))
    {
        let mut inherited = finding(&WRITE_ALL, path, &lines, key, value.line,
            "Jobs without a permissions override inherit write access to every available token scope. Declare the permissions required by those jobs.".into());
        inherited.related_locations = Some(inheriting_jobs);
        findings.push(inherited);
    }
    findings.sort_by_key(|finding| finding.location.range.start_line);
    findings
}

pub fn run_workflow_security(
    root: &Path, files: &[String], checks: &ArtifactChecksConfig,
) -> Vec<AgentFinding> {
    if !checks.github_actions_script_inputs && !checks.github_actions_least_privilege {
        return Vec::new();
    }
    let Ok(root) = root.canonicalize() else { return Vec::new() };
    files.iter().filter(|path| workflow_path(Path::new(path))).flat_map(|path| {
        read_snapshot_file(&root, Path::new(path))
            .map_or_else(Vec::new, |source| check_workflow(path, &source, checks))
    }).collect()
}
