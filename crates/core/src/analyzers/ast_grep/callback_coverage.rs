use std::collections::HashMap;
use std::path::Path;

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::{AgentFinding, AnalysisCoverageIssue};
use tree_sitter::{Point, Tree};

const RULES: &[&str] = &[
    "kotlin-runblocking-in-suspend-function",
    "kotlin-thread-sleep-in-suspend",
    "kotlin-suspend-swallowed-by-runcatching",
    "kotlin-suspend-call-in-finally-without-noncancellable",
];
const DETAIL: &str = "The callback's execution timing and identity were not resolved; review whether its body executes in this suspend function.";

pub(super) fn annotate(root: &Path, findings: &mut [AgentFinding]) -> Vec<AnalysisCoverageIssue> {
    let mut trees: HashMap<String, Option<Tree>> = HashMap::new();
    let mut coverage = Vec::new();
    for finding in findings {
        let Some(rule) = finding.source.rule_id.as_deref().filter(|rule| RULES.contains(rule)) else { continue };
        if finding.meta.as_ref().is_some_and(|meta| meta.contains_key("rulePackId")) {
            continue;
        }
        let tree = trees.entry(finding.location.path.clone()).or_insert_with(|| {
            let source = std::fs::read_to_string(root.join(&finding.location.path)).ok()?;
            let tree = parser_for(Language::Kotlin)?.parse(source, None)?;
            (!tree.root_node().has_error()).then_some(tree)
        });
        if callback_context(tree.as_ref(), finding) == Some(false) {
            continue;
        }
        finding.confidence = finding.confidence.min(0.5);
        finding.message.push_str(" Callback execution timing is unresolved; verify the execution context.");
        coverage.push(AnalysisCoverageIssue {
            analyzer: "ast-grep".into(), reason: "callback_execution_unresolved".into(),
            scope: "function".into(), path: Some(finding.location.path.clone()),
            line: Some(finding.location.range.start_line), rule: Some(rule.into()), detail: DETAIL.into(),
        });
    }
    coverage
}

fn callback_context(tree: Option<&Tree>, finding: &AgentFinding) -> Option<bool> {
    let position = Point::new(
        finding.location.range.start_line.checked_sub(1)? as usize,
        finding.location.range.start_col?.checked_sub(1)? as usize,
    );
    let node = tree?.root_node().descendant_for_point_range(position, position)?;
    for ancestor in std::iter::successors(Some(node), |node| node.parent()) {
        match ancestor.kind() {
            "lambda_literal" => return Some(true),
            "function_declaration" | "anonymous_function" => return Some(false),
            _ => {}
        }
    }
    None
}
