mod go_cancel;
mod java;
mod jest;
mod prisma;
mod rabbitmq;
mod react;
mod web;

use std::path::Path;

use autoreview_schema::{
    AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side,
};
use tree_sitter::{Node, Parser, Tree};

use super::ast_grep::{RuleCatalogEntry, RuleDefinition};

fn rules() -> &'static [&'static Rule] {
    &[
        &go_cancel::RULE,
        &go_cancel::SPAN_RULE,
        &go_cancel::TRACE_PARENT_RULE,
        &go_cancel::TRANSACTION_RULE,
        &go_cancel::HTTP_RULE,
        &java::SPRING_RULE,
        &java::HIBERNATE_RULE,
        &java::OTEL_RULE,
        &java::CONSTRUCTOR_RULE,
        &java::PAGINATION_RULE,
        &jest::RULE,
        &react::INTERVAL_RULE,
        &prisma::RULE,
        &rabbitmq::RULE,
    ]
}

pub(super) fn semantic_ids() -> impl Iterator<Item = String> {
    rules().iter().map(|rule| rule.id.to_owned())
}

pub(super) fn catalog_entries() -> impl Iterator<Item = RuleCatalogEntry> {
    rules().iter().map(|rule| RuleCatalogEntry {
        id: rule.id.into(),
        kind: "framework".into(),
        category: rule.category.into(),
        subcategories: vec![rule.subcategory.into()],
    })
}

pub(super) fn definition(id: &str) -> Option<RuleDefinition> {
    let rule = rules().iter().find(|rule| rule.id == id)?;
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
    findings.extend(web::run(root, changed, |path, source, file| {
        let mut web_findings = react::inspect(path, source, file);
        web_findings.extend(prisma::inspect(path, source, file));
        web_findings.extend(rabbitmq::inspect(path, source, file));
        web_findings.extend(jest::inspect(root, path, source, file));
        web_findings
    }));
    findings
}

struct Rule {
    id: &'static str,
    category: &'static str,
    subcategory: &'static str,
    title: &'static str,
    message: &'static str,
    reference: &'static str,
}

impl Rule {
    fn metadata(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::from([
            ("references".into(), serde_json::json!([self.reference])),
            ("subcategory".into(), serde_json::json!([self.subcategory])),
        ])
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
