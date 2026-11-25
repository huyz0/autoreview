use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use autoreview_dataflow::call_order::{check_keyed_units_with_coverage, keyed_flow_exhausted, KeyedExport, KeyedFunction, KeyedImport, KeyedUnit};
use autoreview_dataflow::lower;
use autoreview_dataflow::cfg::{Cfg, Stmt};
use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::AnalysisCoverageIssue;
use tree_sitter::Node;

use super::cache::{ParseCache, ParsedFile, SummaryCaches};
use super::dispatch::profile;
use super::dsl::call_order_finding;
use super::functions::web_result_functions;
use super::{call_order_rules::CallOrderRuleDef, AgentFinding};

mod go;
mod jvm;
mod rust;
mod web;

const UNIT_LIMIT: usize = 64;

struct Source {
    language: Language,
    parsed: ParsedFile,
    imports: Vec<KeyedImport>,
    exports: Vec<KeyedExport>,
}

type LoweredFile<'tree> = (usize, Vec<(Node<'tree>, Cfg<Stmt>)>);

struct ImportSource<'a> {
    resolver: &'a Resolver<'a>,
    path: &'a Path,
    node: Node<'a>,
    source: &'a str,
}

struct Resolver<'a> {
    root: PathBuf,
    cache: &'a ParseCache,
    packages: &'a SummaryCaches<'a>,
}

pub(super) fn run(
    root: &Path,
    paths: &[String],
    rules: &[CallOrderRuleDef],
    caches: &SummaryCaches<'_>,
) -> (Vec<AgentFinding>, Vec<AnalysisCoverageIssue>) {
    let rules: Vec<_> = rules
        .iter()
        .filter_map(|rule| {
            let declared = Language::from_rule_name(&rule.language)?;
            rule.spec.uses_path_engine().then_some((rule, declared))
        })
        .collect();
    if rules.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let Ok(root) = root.canonicalize() else {
        return (Vec::new(), vec![issue(None, "source_root_unavailable", "repository", "The source root could not be resolved; call-sequence analysis was skipped.")]);
    };
    let resolver = Resolver {
        root,
        cache: caches.parse,
        packages: caches,
    };
    let entries: BTreeSet<_> = paths
        .iter()
        .filter_map(|path| {
            let language = language_for_path(Path::new(path))?;
            rules
                .iter()
                .any(|(_, declared)| language.matches_rule_language(*declared))
                .then(|| resolver.relative(&resolver.root.join(path)))
                .flatten()
        })
        .collect();
    let (sources, mut coverage) = resolver.load(&entries);
    let findings = run_sources(&sources, &entries, &rules, &mut coverage);
    (findings, coverage)
}

fn run_sources(
    sources: &BTreeMap<String, Source>,
    entries: &BTreeSet<String>,
    rules: &[(&CallOrderRuleDef, Language)],
    coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<AgentFinding> {
    let lowered: Vec<_> = sources
        .values()
        .map(|file| {
            let (extract, _) = profile(file.language);
            let mut nodes = extract(&file.parsed.1);
            let ordinary = nodes.len();
            if matches!(file.language, Language::JavaScript | Language::TypeScript | Language::Tsx)
                && rules.iter().any(|(rule, language)| rule.spec.tracks_result_use() && file.language.matches_rule_language(*language))
            {
                nodes.extend(web_result_functions(&file.parsed.1));
            }
            let graphs = nodes.into_iter()
                .map(|node| (node, lower::keyed_variant(file.language, file.parsed.0.as_bytes(), node)))
                .collect::<Vec<_>>();
            (ordinary, graphs)
        })
        .collect();
    record_flow_limits(sources, &lowered, rules, coverage);
    let functions: Vec<_> = lowered
        .iter()
        .map(|(ordinary, file)| {
            let functions = file.iter()
                .map(|(node, graph)| KeyedFunction { node: *node, graph })
                .collect::<Vec<_>>();
            (*ordinary, functions)
        })
        .collect();
    let mut findings = Vec::new();
    for &(rule, declared) in rules {
        let units: Vec<_> = sources
            .iter()
            .zip(&functions)
            .map(|((path, file), (ordinary, functions))| KeyedUnit {
                path,
                source: file.parsed.0.as_bytes(),
                functions: if rule.spec.tracks_result_use() { functions } else { &functions[..*ordinary] },
                imports: &file.imports,
                exports: &file.exports,
                entry: entries.contains(path) && file.language.matches_rule_language(declared),
            })
            .collect();
        let result = check_keyed_units_with_coverage(&rule.spec, &units);
        record_rule_limits(coverage, &rule.id, result.limitations);
        findings.extend(result.hits.iter().map(|hit| {
            let path = hit.path.as_deref().unwrap_or_default();
            let origin = hit
                .entry
                .as_ref()
                .and_then(|entry| entry.registration_path.as_deref())
                .unwrap_or(path);
            call_order_finding(
                path,
                sources.get(origin).map(|file| file.parsed.0.as_str()),
                rule,
                hit,
            )
        }));
    }
    findings
}

fn record_flow_limits(
    sources: &BTreeMap<String, Source>,
    lowered: &[LoweredFile<'_>],
    rules: &[(&CallOrderRuleDef, Language)],
    coverage: &mut Vec<AnalysisCoverageIssue>,
) {
    for ((path, file), (_, graphs)) in sources.iter().zip(lowered) {
        if !rules.iter().any(|(_, language)| file.language.matches_rule_language(*language)) {
            continue;
        }
        for (node, graph) in graphs {
            if keyed_flow_exhausted(graph) {
                let mut limitation = issue(Some(path), "control_flow_limit", "function", "The finally-path lowering budget was exhausted; call-sequence analysis skipped this function.");
                limitation.line = Some(node.start_position().row as u32 + 1);
                coverage.push(limitation);
            }
        }
    }
}

fn record_rule_limits(coverage: &mut Vec<AnalysisCoverageIssue>, rule_id: &str, limitations: Vec<autoreview_dataflow::call_order::KeyedLimitation>) {
    for limitation in limitations {
        let detail = match limitation.reason {
            "path_budget" => "The path-state budget was exhausted; this rule skipped the affected root.",
            "helper_summary_limit" => "Helper summaries did not converge within the analysis budget; cross-file result tracking was skipped.",
            "unresolved_helper_target" => "The helper target is ambiguous or depends on unavailable exports; cross-file call-sequence analysis may be incomplete.",
            "reexport_resolution_limit" => "The re-export resolution budget was exhausted; cross-file call-sequence analysis may be incomplete.",
            _ => "The rule could not model a path safely; analysis of the affected root was skipped.",
        };
        let mut record = issue(limitation.path.as_deref(), limitation.reason,
            if limitation.path.is_some() { "function" } else { "rule" }, detail);
        record.line = limitation.line.filter(|line| *line > 0);
        record.rule = Some(rule_id.to_owned());
        coverage.push(record);
    }
}

impl Resolver<'_> {
    fn load(&self, entries: &BTreeSet<String>) -> (BTreeMap<String, Source>, Vec<AnalysisCoverageIssue>) {
        let mut pending: VecDeque<_> = entries.iter().cloned().collect();
        let mut sources = BTreeMap::new();
        let mut coverage = Vec::new();
        while let Some(path) = pending.pop_front() {
            if sources.contains_key(&path) {
                continue;
            }
            let Some((full, language, parsed)) = self.parse_dependency(&path, entries, &mut coverage) else { continue };
            let imports = self.imports(&full, language, &parsed, &mut coverage);
            let exports = self.exports(&full, language, &parsed, &mut coverage);
            if sources.len() < entries.len().saturating_add(UNIT_LIMIT) {
                pending.extend(imports.iter().map(|binding| binding.path.clone()));
                pending.extend(exports.iter().filter_map(|export| match export {
                    KeyedExport::Named { path: Some(path), .. } | KeyedExport::Star { path } if !path.is_empty() => Some(path.clone()),
                    _ => None,
                }));
            }
            sources.insert(
                path,
                Source {
                    language,
                    parsed,
                    imports,
                    exports,
                },
            );
            if sources.len() >= entries.len().saturating_add(UNIT_LIMIT) {
                if let Some(skipped) = pending.iter().find(|path| !sources.contains_key(*path)) {
                    coverage.push(issue(Some(skipped), "dependency_limit", "dependency", "The cross-file call-sequence source limit was reached; remaining dependencies were not analyzed."));
                }
                break;
            }
        }
        (sources, coverage)
    }

    fn parse_dependency(&self, path: &str, entries: &BTreeSet<String>, coverage: &mut Vec<AnalysisCoverageIssue>) -> Option<(PathBuf, Language, ParsedFile)> {
        let full = self.root.join(path);
        let language = language_for_path(&full)?;
        let parsed = match self.cache.get(&full, language) {
            Some(parsed) => parsed,
            None => {
                if !entries.contains(path) {
                    coverage.push(issue(Some(path), "dependency_unavailable", "dependency", "The imported source could not be read or parsed; cross-file call-sequence analysis may be incomplete."));
                }
                return None;
            }
        };
        if !entries.contains(path) && parsed.1.root_node().has_error() {
            coverage.push(issue(Some(path), "dependency_syntax_errors", "dependency", "The imported source has syntax errors; cross-file call-sequence analysis may be incomplete."));
        }
        Some((full, language, parsed))
    }

    fn imports(&self, path: &Path, language: Language, parsed: &ParsedFile, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedImport> {
        let source = &parsed.0;
        let node = parsed.1.root_node();
        let input = ImportSource {
            resolver: self,
            path,
            node,
            source,
        };
        let imports = match language {
            Language::Go => input.go(coverage),
            Language::Java | Language::Kotlin => jvm::imports(self, path, language, node, source),
            Language::Rust => input.rust(),
            Language::JavaScript | Language::TypeScript | Language::Tsx => input.web(coverage),
        };
        imports
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn exports(&self, path: &Path, language: Language, parsed: &ParsedFile, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedExport> {
        if !matches!(language, Language::JavaScript | Language::TypeScript | Language::Tsx) {
            return Vec::new();
        }
        ImportSource { resolver: self, path, node: parsed.1.root_node(), source: &parsed.0 }.web_exports(coverage)
    }

    fn relative(&self, path: &Path) -> Option<String> {
        let canonical = path.canonicalize().ok()?;
        let path = canonical.strip_prefix(&self.root).ok()?;
        Some(path.to_str()?.replace('\\', "/"))
    }

    fn files(&self, dir: &Path, extension: &str) -> Vec<PathBuf> {
        if self.relative(dir).is_none() {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|file| {
                file.extension().and_then(|ext| ext.to_str()) == Some(extension)
                    && self.relative(file).is_some()
            })
            .collect();
        paths.sort();
        paths
    }

    fn binding(
        &self,
        file: &Path,
        local: &str,
        name: Option<&str>,
        owner: Option<&str>,
        exported: bool,
    ) -> Option<KeyedImport> {
        Some(KeyedImport {
            local: local.to_owned(),
            path: self.relative(file)?,
            name: name.map(str::to_owned),
            owner: owner.map(str::to_owned),
            exported,
        })
    }
}

fn issue(path: Option<&str>, reason: &str, scope: &str, detail: &str) -> AnalysisCoverageIssue {
    AnalysisCoverageIssue {
        analyzer: "call-sequence".into(), reason: reason.into(), scope: scope.into(),
        path: path.map(str::to_owned), line: None, rule: None, detail: detail.into(),
    }
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.named_children(&mut node.walk())
        .filter(|child| !child.is_extra())
        .collect()
}

fn text<'src>(node: Node<'_>, source: &'src str) -> Option<&'src str> {
    node.utf8_text(source.as_bytes()).ok()
}

fn unquote(value: &str) -> Option<&str> {
    let delimiter = value.chars().next()?;
    if !matches!(delimiter, '\'' | '"' | '`') {
        return None;
    }
    let content = value.strip_prefix(delimiter)?.strip_suffix(delimiter)?;
    (!content.contains(['\\', '\n', '\r'])).then_some(content)
}
