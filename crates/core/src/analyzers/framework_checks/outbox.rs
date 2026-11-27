mod calls;
mod flow;

use std::collections::BTreeMap;
use std::path::Path;

use autoreview_langsupport::language_for_path;
use autoreview_schema::{AgentFinding, OutboxOperationConfig};

use super::super::artifact_policy::Rule;
use super::web::imported_operations::FileCalls;
use super::web::scoped_policy::{self, Scope};
use super::web::is_function;
use super::{descendants, location, Node};

pub(in crate::analyzers) const RULE: Rule = Rule {
    id: "outbox-direct-database-and-broker-write",
    category: "correctness",
    subcategory: "messaging.atomic-publication",
    tool: "autoreview-outbox",
    title: "Database write and event publication run independently",
    reference: "https://docs.aws.amazon.com/prescriptive-guidance/latest/cloud-design-patterns/transactional-outbox.html",
    enabled_by: "artifactChecks.outboxOperations",
};

pub fn run_outbox_policy(
    root: &Path,
    changed: &[String],
    configs: &[OutboxOperationConfig],
) -> anyhow::Result<Vec<AgentFinding>> {
    scoped_policy::run(root, changed, configs.iter().map(|config| Scope::new(&config.paths, config)), inspect)
}

fn inspect(path: &str, source: &str, file: Node<'_>, policies: &[Scope<'_, OutboxOperationConfig>]) -> Vec<AgentFinding> {
    let Some(language) = language_for_path(Path::new(path)) else {
        return Vec::new();
    };
    let file_calls = FileCalls::new(file, source);
    let functions: Vec<_> = descendants(file)
        .filter(|node| {
            is_function(*node)
                && node
                    .child_by_field_name("body")
                    .is_some_and(|body| body.kind() == "statement_block")
        })
        .collect();

    let mut pairs = BTreeMap::new();
    for policy in policies.iter().filter(|policy| policy.matches(path)) {
        let Some(endpoints) = calls::resolve(&file_calls, policy.config) else {
            continue;
        };
        for &function in &functions {
            if !endpoints.has_both_in(function) {
                continue;
            }
            let cfg =
                autoreview_dataflow::lower::keyed_variant(language, source.as_bytes(), function);
            pairs.extend(flow::pairs(&cfg, &endpoints));
        }
    }
    make_findings(path, source, pairs)
}

fn make_findings(
    path: &str,
    source: &str,
    pairs: BTreeMap<usize, (Node<'_>, Node<'_>)>,
) -> Vec<AgentFinding> {
    pairs.into_values().map(|(publish, write)| {
        let anchor = location(path, publish, source);
        let mut finding = RULE.finding(path, anchor.range, &anchor.snippet,
            "This broker publication and the linked database write can run in the same operation. Persist the event in the transaction and publish it from the outbox.".into());
        finding.related_locations = Some(vec![location(path, write, source)]);
        finding
    }).collect()
}
