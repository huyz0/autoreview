mod archive;
mod flow;
mod http;
mod jwt;
mod package;
mod range_assert;
mod span;
mod tls;
mod transactions;

use std::collections::BTreeSet;
use std::path::Path;

use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{location, Rule};
pub(super) use http::RULE as HTTP_RULE;
pub(super) use archive::RULE as ARCHIVE_RULE;
pub(super) use jwt::RULE as JWT_RULE;
pub(super) use range_assert::RULE as RANGE_ASSERT_RULE;
pub(super) use span::RULE as SPAN_RULE;
pub(super) use span::TRACE_PARENT_RULE;
pub(super) use tls::RULE as TLS_RULE;
pub(super) use transactions::RULE as TRANSACTION_RULE;

pub(super) const RULE: Rule = Rule {
    id: "go-context-cancel-on-all-paths",
    category: "correctness",
    subcategories: &["reliability.cancellation"],
    title: "Context cancellation is missing on a return path",
    message: "At least one path returns without calling, deferring, or returning the cancellation function. Arrange cleanup after the context is created, or return the cancel function to transfer ownership.",
    reference: "https://go.dev/doc/database/cancel-operations",
};

pub(super) fn run(root: &Path, paths: &[String]) -> Vec<AgentFinding> {
    let directories: BTreeSet<_> = paths
        .iter()
        .filter(|path| path.ends_with(".go"))
        .filter_map(|path| Path::new(path).parent())
        .collect();
    directories
        .into_iter()
        .flat_map(|directory| {
            let files = package::load(root, directory);
            let names: BTreeSet<_> = files.iter().map(|file| &file.package).collect();
            let mut findings = Vec::new();
            for name in names {
                let peers: Vec<_> = files.iter().filter(|file| &file.package == name).collect();
                findings.extend(package_findings(&peers, paths));
            }
            findings
        })
        .collect()
}

fn package_findings(peers: &[&package::File], paths: &[String]) -> Vec<AgentFinding> {
    let helpers = package::summaries(peers);
    let mut findings = Vec::new();
    for &file in peers {
        if paths.contains(&file.path) {
            findings.extend(span::findings(file));
            findings.extend(http::findings(file));
            findings.extend(archive::findings(file));
            findings.extend(jwt::findings(file));
            findings.extend(tls::findings(file, peers));
            findings.extend(transactions::findings(file));
            findings.extend(range_assert::findings(file, peers));
        }
        for (binding, exit, used) in package::leaks(file, &helpers) {
            let sources: Vec<_> = helpers.evidence(used).collect();
            if !paths.contains(&file.path)
                && !sources.iter().any(|(peer, _)| paths.contains(&peer.path))
            {
                continue;
            }
            let mut related = vec![location(&file.path, exit, &file.source)];
            related.extend(
                sources
                    .into_iter()
                    .map(|(peer, site)| location(&peer.path, site, &peer.source)),
            );
            findings.push(RULE.finding(location(&file.path, binding, &file.source), related));
        }
    }
    findings
}

fn two_result_call(assignment: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let left = assignment.child_by_field_name("left")?;
    let right = assignment.child_by_field_name("right")?;
    if left.named_child_count() != 2 || right.named_child_count() != 1 {
        return None;
    }
    let binding = left
        .named_child(1)
        .filter(|node| node.kind() == "identifier" || node.kind() == "blank_identifier")?;
    let call = right
        .named_child(0)
        .filter(|node| node.kind() == "call_expression")?;
    Some((binding, call))
}
