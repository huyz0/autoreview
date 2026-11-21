mod flow;
mod package;

use std::collections::BTreeSet;
use std::path::Path;

use autoreview_schema::AgentFinding;

use super::{location, Rule};

pub(super) const RULE: Rule = Rule {
    id: "go-context-cancel-on-all-paths",
    category: "correctness",
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
