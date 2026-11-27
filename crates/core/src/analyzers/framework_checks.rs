mod discarded_strings;
mod fetch;
mod go_cancel;
mod java;
mod jest;
mod node_tls;
pub(super) mod outbox;
mod prisma;
mod promise_value;
mod rabbitmq;
mod react;
mod web;

pub use outbox::run_outbox_policy;
pub use java::run_nullability_regressions;

use std::path::Path;

use autoreview_schema::{
    AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side,
};
use tree_sitter::{Node, Parser, Tree};

use super::ast_grep::{RuleCatalogEntry, RuleDefinition};

fn rules() -> impl Iterator<Item = &'static Rule> {
    [
        &go_cancel::RULE,
        &go_cancel::SPAN_RULE,
        &go_cancel::TRACE_PARENT_RULE,
        &go_cancel::TRANSACTION_RULE,
        &go_cancel::HTTP_RULE,
        &go_cancel::TLS_RULE,
        &go_cancel::RANGE_ASSERT_RULE,
        &java::SPRING_RULE,
        &java::HIBERNATE_RULE,
        &java::OTEL_RULE,
        &java::CONSTRUCTOR_RULE,
        &java::EXECUTOR_SHUTDOWN_RULE,
        &java::PAGINATION_RULE,
        &java::NULLABILITY_REGRESSION_RULE,
        &jest::RULE,
        &node_tls::RULE,
        &node_tls::JAVASCRIPT_RULE,
        &node_tls::TYPESCRIPT_RULE,
        &react::INTERVAL_RULE,
        &prisma::RULE,
        &rabbitmq::RULE,
    ].into_iter().chain(fetch::RULES).chain(discarded_strings::RULES).chain(promise_value::RULES)
}

pub(super) fn semantic_ids() -> impl Iterator<Item = String> {
    rules().map(|rule| rule.id.to_owned())
}

pub(super) fn catalog_entries() -> impl Iterator<Item = RuleCatalogEntry> {
    rules().map(|rule| RuleCatalogEntry {
        id: rule.id.into(),
        kind: "framework".into(),
        category: rule.category.into(),
        subcategories: rule.subcategories.iter().map(|label| (*label).into()).collect(),
    })
}

pub(super) fn definition(id: &str) -> Option<RuleDefinition> {
    let rule = rules().find(|rule| rule.id == id)?;
    let metadata = serde_json::json!({
        "id": rule.id, "kind": "framework", "category": rule.category, "semantic": true,
        "message": rule.message, "references": [rule.reference],
        "metadata": rule.metadata(),
    });
    Some(RuleDefinition {
        id: rule.id.into(),
        source_label: "builtin".into(),
        kind: "framework".into(),
        category: rule.category.into(),
        semantic: true,
        yaml: serde_yaml::to_string(&metadata).ok()?,
    })
}

pub fn run_framework_checks(root: &Path, changed: &[String]) -> Vec<AgentFinding> {
    let mut findings = go_cancel::run(root, changed);
    findings.extend(java::run(root, changed));
    findings.extend(discarded_strings::run(root, changed));
    findings.extend(web::run(root, changed, |path, source, file| {
        let mut web_findings = react::inspect(path, source, file);
        web_findings.extend(prisma::inspect(path, source, file));
        web_findings.extend(rabbitmq::inspect(path, source, file));
        web_findings.extend(jest::inspect(root, path, source, file));
        web_findings.extend(node_tls::inspect(path, source, file));
        web_findings.extend(fetch::inspect(path, source, file));
        web_findings.extend(promise_value::inspect(path, source, file));
        web_findings
    }));
    findings
}

struct Rule {
    id: &'static str,
    category: &'static str,
    subcategories: &'static [&'static str],
    title: &'static str,
    message: &'static str,
    reference: &'static str,
}

impl Rule {
    fn metadata(&self) -> std::collections::HashMap<String, serde_json::Value> {
        super::artifact_policy::rule_metadata(self.reference, self.subcategories)
    }

    fn finding(&self, anchor: Location, related: Vec<Location>) -> AgentFinding {
        AgentFinding {
            source: FindingSource {
                kind: FindingSourceKind::Analyzer,
                tool: "autoreview-frameworks".into(),
                rule_id: Some(self.id.into()),
                aspect: None,
                backend: None,
            },
            category: self.category.into(),
            severity: Severity::Medium,
            confidence: 0.8,
            title: self.title.into(),
            message: self.message.into(),
            location: anchor,
            related_locations: (!related.is_empty()).then_some(related),
            suggestion: None,
            tags: None,
            meta: Some(self.metadata()),
            suggested_patch: None,
        }
    }
}

fn location(path: &str, node: Node<'_>, source: &str) -> Location {
    Location {
        path: path.into(),
        range: LocationRange {
            start_line: node.start_position().row as u32 + 1,
            end_line: Some(node.end_position().row as u32 + 1),
            ..Default::default()
        },
        snippet: text(node, source).lines().next().unwrap_or("").into(),
        side: Side::New,
    }
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn run_sources(
    root: &Path,
    paths: &[String],
    accepts: impl Fn(autoreview_langsupport::Language) -> bool,
    check: impl Fn(&str, &str, Node<'_>, autoreview_langsupport::Language) -> Vec<AgentFinding>,
) -> Vec<AgentFinding> {
    let Ok(root) = root.canonicalize() else { return Vec::new() };
    let mut findings = Vec::new();
    for path in paths {
        let absolute = root.join(path);
        let Some(language) = autoreview_langsupport::language_for_path(&absolute).filter(|language| accepts(*language))
        else {
            continue;
        };
        let Some(mut parser) = autoreview_langsupport::parser_for(language) else { continue };
        let Some(resolved) = absolute.canonicalize().ok().filter(|file| file.starts_with(&root)) else { continue };
        if !absolute.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
            continue;
        }
        let Some((source, tree)) = parse_source(&mut parser, &resolved) else { continue };
        findings.extend(check(path, &source, tree.root_node(), language));
    }
    findings
}

fn parse_source(parser: &mut Parser, path: &Path) -> Option<(String, Tree)> {
    let source = std::fs::read_to_string(path).ok()?;
    let tree = parser.parse(&source, None)?;
    (!tree.root_node().has_error()).then_some((source, tree))
}

fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .collect::<Vec<_>>()
        .into_iter()
}

fn descendants(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut stack = vec![node];
    std::iter::from_fn(move || {
        let node = stack.pop()?;
        let mut cursor = node.walk();
        let start = stack.len();
        stack.extend(node.named_children(&mut cursor));
        stack[start..].reverse();
        Some(node)
    })
}
