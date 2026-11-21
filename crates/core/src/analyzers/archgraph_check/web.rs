mod resolve;
mod syntax;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use autoreview_archgraph::{shortest_path_to, ImportGraph};
use autoreview_schema::{
    AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side,
};

use resolve::{resolve, source_language};

#[derive(Default)]
struct WebGraph {
    graph: ImportGraph,
    imports: HashMap<String, Vec<Edge>>,
}

struct Edge {
    target: String,
    specifier: String,
    lines: std::ops::RangeInclusive<u32>,
}

pub(super) fn run(root: &Path, changed: &[String]) -> Vec<AgentFinding> {
    let entries: BTreeSet<_> = changed
        .iter()
        .filter(|path| source_language(Path::new(path)).is_some())
        .collect();
    if entries.is_empty() {
        return Vec::new();
    }
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    let graph = WebGraph::build(&root, entries.iter().map(|path| (*path).clone()).collect());
    graph.findings(entries)
}

impl WebGraph {
    fn build(root: &Path, mut pending: Vec<String>) -> Self {
        let mut web = Self::default();
        while let Some(path) = pending.pop() {
            if web.imports.contains_key(&path) {
                continue;
            }
            let imports = read_edges(root, &path);
            for edge in &imports {
                web.graph
                    .edges
                    .entry(path.clone())
                    .or_default()
                    .entry(edge.target.clone())
                    .or_default()
                    .insert(edge.specifier.clone());
                if !web.imports.contains_key(&edge.target) {
                    pending.push(edge.target.clone());
                }
            }
            web.imports.insert(path, imports);
        }
        web
    }

    fn findings(&self, changed: BTreeSet<&String>) -> Vec<AgentFinding> {
        let mut seen = HashSet::new();
        let mut findings = Vec::new();
        for file in changed {
            for edge in self.imports.get(file).into_iter().flatten() {
                let Some(cycle) = self.return_path(file, edge) else {
                    continue;
                };
                let canonical = canonical_cycle(&cycle);
                if seen.insert(canonical) {
                    findings.push(self.finding(file, edge, &cycle));
                }
            }
        }
        findings
    }

    fn return_path(&self, file: &str, edge: &Edge) -> Option<Vec<String>> {
        if edge.target == file {
            return Some(vec![file.to_owned(), file.to_owned()]);
        }
        let back = shortest_path_to(&self.graph, &edge.target, |target| target == file)?;
        Some(std::iter::once(file.to_owned()).chain(back).collect())
    }

    fn finding(&self, file: &str, first: &Edge, cycle: &[String]) -> AgentFinding {
        let related: Vec<_> = cycle
            .windows(2)
            .skip(1)
            .flat_map(|hop| {
                self.imports[&hop[0]]
                    .iter()
                    .filter(|edge| edge.target == hop[1])
                    .map(|edge| location(&hop[0], edge))
            })
            .collect();
        AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-archgraph".into(), rule_id: Some("web-import-cycle".into()), aspect: None, backend: None },
            category: "architecture".into(),
            severity: Severity::Medium,
            confidence: 0.9,
            title: "Circular JavaScript or TypeScript module dependency".into(),
            message: format!("These relative imports form a runtime module cycle: {}. Separate the shared dependency or change which module owns it.", cycle.join(" -> ")),
            location: location(file, first),
            related_locations: (!related.is_empty()).then_some(related),
            suggestion: None,
            tags: None,
            meta: None,
            suggested_patch: None,
        }
    }
}

fn read_edges(root: &Path, path: &str) -> Vec<Edge> {
    let absolute = root.join(path);
    let Some(language) = source_language(&absolute) else {
        return Vec::new();
    };
    if resolve::local_source(root, &absolute).as_deref() != Some(path) {
        return Vec::new();
    }
    let Ok(source) = std::fs::read_to_string(&absolute) else {
        return Vec::new();
    };
    let common_js = matches!(
        absolute.extension().and_then(|ext| ext.to_str()),
        Some("cjs" | "cts")
    );
    syntax::imports(language, &source, common_js)
        .into_iter()
        .filter_map(|(specifier, lines)| {
            let target = resolve(root, Path::new(path), &specifier)?;
            Some(Edge {
                target,
                specifier,
                lines,
            })
        })
        .collect()
}

fn canonical_cycle(cycle: &[String]) -> Vec<String> {
    let mut result = cycle[..cycle.len() - 1].to_vec();
    let first = result
        .iter()
        .enumerate()
        .min_by_key(|(_, path)| *path)
        .map_or(0, |(index, _)| index);
    result.rotate_left(first);
    result
}

fn location(path: &str, edge: &Edge) -> Location {
    Location {
        path: path.into(),
        range: LocationRange {
            start_line: *edge.lines.start(),
            end_line: Some(*edge.lines.end()),
            ..Default::default()
        },
        snippet: edge.specifier.clone(),
        side: Side::New,
    }
}
