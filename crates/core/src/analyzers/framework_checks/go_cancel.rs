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
    let mut findings = Vec::new();
    for directory in directories {
        let files = package::load(root, directory);
        for name in files
            .iter()
            .map(|file| &file.package)
            .collect::<BTreeSet<_>>()
        {
            let package: Vec<_> = files.iter().filter(|file| &file.package == name).collect();
            let helpers = package::summaries(&package);
            for file in &package {
                for (binding, exit, used) in package::leaks(file, &helpers) {
                    let evidence = used
                        .iter()
                        .filter_map(|name| helpers.helper_files.get(name));
                    let mut related = vec![location(&file.path, exit, &file.source)];
                    let relevant = paths.contains(&file.path)
                        || evidence.clone().any(|(peer, _)| paths.contains(&peer.path));
                    if !relevant {
                        continue;
                    }
                    related.extend(
                        evidence.map(|(peer, site)| location(&peer.path, *site, &peer.source)),
                    );
                    findings
                        .push(RULE.finding(location(&file.path, binding, &file.source), related));
                }
            }
        }
    }
    findings
}
