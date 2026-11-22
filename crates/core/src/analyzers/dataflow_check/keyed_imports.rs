use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use autoreview_dataflow::call_order::{check_keyed_units, KeyedFunction, KeyedImport, KeyedUnit};
use autoreview_dataflow::lower;
use autoreview_langsupport::{language_for_path, Language};
use tree_sitter::Node;

use super::cache::{ParseCache, ParsedFile, SummaryCaches};
use super::dispatch::profile;
use super::dsl::call_order_finding;
use super::{call_order_rules::CallOrderRuleDef, AgentFinding};

mod jvm;
mod modules;

const UNIT_LIMIT: usize = 64;

struct Source {
    language: Language,
    parsed: ParsedFile,
    imports: Vec<KeyedImport>,
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
) -> Vec<AgentFinding> {
    let rules: Vec<_> = rules
        .iter()
        .filter_map(|rule| {
            rule.spec.identity?;
            Some((rule, Language::from_rule_name(&rule.language)?))
        })
        .collect();
    if rules.is_empty() {
        return Vec::new();
    }
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
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
    run_sources(&resolver.load(&entries), &entries, &rules)
}

fn run_sources(
    sources: &BTreeMap<String, Source>,
    entries: &BTreeSet<String>,
    rules: &[(&CallOrderRuleDef, Language)],
) -> Vec<AgentFinding> {
    let lowered: Vec<_> = sources
        .values()
        .map(|file| {
            let (extract, _) = profile(file.language);
            extract(&file.parsed.1)
                .into_iter()
                .map(|node| {
                    (
                        node,
                        lower::keyed_variant(file.language, file.parsed.0.as_bytes(), node),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let functions: Vec<Vec<_>> = lowered
        .iter()
        .map(|file| {
            file.iter()
                .map(|(node, graph)| KeyedFunction { node: *node, graph })
                .collect()
        })
        .collect();
    let mut findings = Vec::new();
    for &(rule, declared) in rules {
        let units: Vec<_> = sources
            .iter()
            .zip(&functions)
            .map(|((path, file), functions)| KeyedUnit {
                path,
                source: file.parsed.0.as_bytes(),
                functions,
                imports: &file.imports,
                entry: entries.contains(path) && file.language.matches_rule_language(declared),
            })
            .collect();
        findings.extend(check_keyed_units(&rule.spec, &units).iter().map(|hit| {
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

impl Resolver<'_> {
    fn load(&self, entries: &BTreeSet<String>) -> BTreeMap<String, Source> {
        let mut pending: VecDeque<_> = entries.iter().cloned().collect();
        let mut sources = BTreeMap::new();
        while let Some(path) = pending.pop_front() {
            if sources.contains_key(&path) {
                continue;
            }
            let full = self.root.join(&path);
            let Some(language) = language_for_path(&full) else {
                continue;
            };
            let Some(parsed) = self.cache.get(&full, language) else {
                continue;
            };
            let imports = self.imports(&full, language, &parsed);
            if sources.len() < entries.len().saturating_add(UNIT_LIMIT) {
                pending.extend(imports.iter().map(|binding| binding.path.clone()));
            }
            sources.insert(
                path,
                Source {
                    language,
                    parsed,
                    imports,
                },
            );
            if sources.len() >= entries.len().saturating_add(UNIT_LIMIT) {
                break;
            }
        }
        sources
    }

    fn imports(&self, path: &Path, language: Language, parsed: &ParsedFile) -> Vec<KeyedImport> {
        let source = &parsed.0;
        let node = parsed.1.root_node();
        let imports = match language {
            Language::Go => modules::go(self, path, node, source),
            Language::Java | Language::Kotlin => jvm::imports(self, path, language, node, source),
            Language::Rust => modules::rust(self, path, node, source),
            Language::JavaScript | Language::TypeScript | Language::Tsx => {
                modules::web(self, path, node, source)
            }
        };
        imports
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
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

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.named_children(&mut node.walk())
        .filter(|child| !child.is_extra())
        .collect()
}

fn text<'src>(node: Node<'_>, source: &'src str) -> Option<&'src str> {
    node.utf8_text(source.as_bytes()).ok()
}
