mod basics;
mod collections;
mod design;
mod mocking;
mod panics;
mod select_io;
mod sql;
mod tasks;
mod test_state;
pub(super) mod syntax;

use std::collections::HashMap;
use std::path::Path;

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::{
    AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side,
};
use tree_sitter::Node;

use self::syntax::{descendants, Source};

pub struct RustReviewRule {
    pub id: &'static str,
    pub category: &'static str,
    pub severity: Severity,
    pub semantic: bool,
    pub title: &'static str,
    pub message: &'static str,
    pub reference: &'static str,
    pub cwe: Option<&'static str>,
}

impl RustReviewRule {
    pub(super) fn subcategories(&self) -> &'static [&'static str] {
        match self.id {
            "rust-vec-set-len-before-initialization" => &["memory.initialization"],
            "rust-timeout-consumes-join-handle" => &["reliability.cancellation", "concurrency.task-lifecycle"],
            "rust-sqlx-pool-write-during-transaction" => &["persistence.transaction-boundaries", "persistence.data-integrity"],
            "rust-tokio-select-restarts-read-exact" => &["concurrency.cancellation-safety"],
            _ => &[],
        }
    }
}

pub fn rust_review_rule(id: &str) -> Option<&'static RustReviewRule> {
    rules().find(|rule| rule.id == id)
}

pub fn rust_review_semantic_ids() -> impl Iterator<Item = &'static str> {
    rules().filter(|rule| rule.semantic).map(|rule| rule.id)
}

pub(super) fn rules() -> impl Iterator<Item = &'static RustReviewRule> {
    basics::RULES
        .iter()
        .chain(collections::RULES)
        .chain(sql::RULES)
        .chain(mocking::RULES)
        .chain(panics::RULES)
        .chain(design::RULES)
        .chain(tasks::RULES)
        .chain(test_state::RULES)
        .copied()
        .chain(std::iter::once(&select_io::RULE))
}

struct Review<'tree, 'text> {
    source: Source<'tree, 'text>,
    path: &'text str,
    findings: Vec<AgentFinding>,
}

impl Review<'_, '_> {
    fn location(&self, anchor: Node<'_>) -> Location {
        let start = anchor.start_position();
        let end = anchor.end_position();
        Location {
            path: self.path.to_owned(),
            range: LocationRange {
                start_line: (start.row + 1) as u32,
                start_col: Some((start.column + 1) as u32),
                end_line: Some((end.row + 1) as u32),
                end_col: Some((end.column + 1) as u32),
            },
            snippet: self
                .source
                .text(anchor)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
            side: Side::New,
        }
    }

    fn report(&mut self, rule: &'static RustReviewRule, anchor: Node<'_>) {
        self.report_related(rule, anchor, &[]);
    }

    fn report_related(
        &mut self,
        rule: &'static RustReviewRule,
        anchor: Node<'_>,
        related: &[Node<'_>],
    ) {
        let mut meta =
            HashMap::from([("references".to_owned(), serde_json::json!([rule.reference]))]);
        if let Some(cwe) = rule.cwe {
            meta.insert("cwe".to_owned(), serde_json::json!([cwe]));
        }
        if !rule.subcategories().is_empty() {
            meta.insert("subcategory".to_owned(), serde_json::json!(rule.subcategories()));
        }
        self.findings.push(AgentFinding {
            source: FindingSource {
                kind: FindingSourceKind::Analyzer,
                tool: "autoreview-rust".to_owned(),
                rule_id: Some(rule.id.to_owned()),
                aspect: None,
                backend: None,
            },
            category: rule.category.to_owned(),
            severity: rule.severity,
            confidence: if rule.semantic { 0.75 } else { 1.0 },
            title: rule.title.to_owned(),
            message: rule.message.to_owned(),
            location: self.location(anchor),
            related_locations: (!related.is_empty())
                .then(|| related.iter().map(|part| self.location(*part)).collect()),
            suggestion: None,
            tags: None,
            meta: Some(meta),
            suggested_patch: None,
        });
    }
}

fn analyze_file(path: &str, text: &str) -> Vec<AgentFinding> {
    let Some(tree) = parser_for(Language::Rust).and_then(|mut parser| parser.parse(text, None))
    else {
        return Vec::new();
    };
    let root = tree.root_node();
    // Error recovery can hide a shadowing binding or attach an attribute to
    // the wrong item. Only make scope-based findings on a complete tree.
    if root.has_error() {
        return Vec::new();
    }
    let mut review = Review {
        source: Source::new(root, text),
        path,
        findings: Vec::new(),
    };
    for node in descendants(root) {
        basics::check(&mut review, node);
        collections::check(&mut review, node);
        sql::check(&mut review, node);
        mocking::check(&mut review, node);
        panics::check(&mut review, node);
        design::check(&mut review, node);
        tasks::check(&mut review, node);
        select_io::check(&mut review, node);
        test_state::check(&mut review, node);
    }
    review.findings
}

pub fn run_rust_review(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    let Ok(root) = repo_root.canonicalize() else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for path in changed_files.iter().filter(|path| path.ends_with(".rs")) {
        let requested = repo_root.join(path);
        let Ok(source) = requested.canonicalize() else {
            continue;
        };
        if source.starts_with(&root)
            && requested
                .symlink_metadata()
                .is_ok_and(|meta| meta.is_file())
        {
            let Ok(text) = std::fs::read_to_string(source) else {
                continue;
            };
            findings.extend(analyze_file(path, &text));
        }
    }
    findings
}

pub(super) fn contains_inline_tests(text: &str) -> bool {
    let Some(tree) = parser_for(Language::Rust).and_then(|mut parser| parser.parse(text, None))
    else {
        return false;
    };
    let found = descendants(tree.root_node())
        .filter(|node| node.kind() == "attribute_item")
        .any(|node| {
            let attr: String = text[node.byte_range()]
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect();
            matches!(
                attr.as_str(),
                "#[test]" | "#[cfg(test)]" | "#[tokio::test]" | "#[async_std::test]"
            ) || attr.starts_with("#[tokio::test(")
                || attr.starts_with("#[async_std::test(")
        });
    found
}
