use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use autoreview_dataflow::call_order::{check_keyed_units_with_coverage, KeyedExport, KeyedFunction, KeyedImport, KeyedUnit};
use autoreview_dataflow::lower;
use autoreview_dataflow::cfg::{Cfg, Stmt};
use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::AnalysisCoverageIssue;
use tree_sitter::Node;

use super::cache::{ParseCache, ParsedFile, SummaryCaches};
use super::dispatch::profile;
use super::dsl::call_order_finding;
use super::functions::{web_named_functions, web_result_functions};
use super::{call_order_rules::CallOrderRuleDef, AgentFinding};

mod go;
mod jvm;
mod rust;
mod web;
mod typed;
mod coverage;
use coverage::{issue, record_flow_limits, record_rule_limits, scope_coverage};
use typed::{check_typed_rule, TrustedTypeScript};

const UNIT_LIMIT: usize = 64;
const TYPED_UNIT_LIMIT: usize = 128;

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
    let source_root = if root.is_absolute() { root.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(root) };
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
    let (typed, ordinary): (Vec<_>, Vec<_>) = rules.into_iter().partition(|(rule, _)| {
        rule.spec.contract.as_ref().is_some_and(|contract| contract.typed_promise_factory)
    });
    let checker = TrustedTypeScript::from_environment(&source_root);
    let (mut findings, mut coverage) = run_group(&resolver, paths, &ordinary, &source_root, (None, UNIT_LIMIT));
    let limit = if checker.is_some() { TYPED_UNIT_LIMIT } else { UNIT_LIMIT };
    let (typed_findings, typed_coverage) = run_group(&resolver, paths, &typed, &source_root, (checker.as_ref(), limit));
    findings.extend(typed_findings);
    coverage.extend(typed_coverage);
    (findings, coverage)
}

fn run_group(
    resolver: &Resolver<'_>, paths: &[String], rules: &[(&CallOrderRuleDef, Language)],
    root: &Path, options: (Option<&TrustedTypeScript>, usize),
) -> (Vec<AgentFinding>, Vec<AnalysisCoverageIssue>) {
    let (checker, limit) = options;
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
    if entries.is_empty() { return (Vec::new(), Vec::new()); }
    let (sources, mut coverage) = if limit == UNIT_LIMIT { resolver.load(&entries) } else { resolver.load_with_limit(&entries, limit) };
    let findings = run_sources(&sources, &entries, rules, root, checker, &mut coverage);
    (findings, scope_coverage(coverage, rules, &entries))
}


fn run_sources(
    sources: &BTreeMap<String, Source>,
    entries: &BTreeSet<String>,
    rules: &[(&CallOrderRuleDef, Language)],
    root: &Path,
    checker: Option<&TrustedTypeScript>,
    coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<AgentFinding> {
    let lowered: Vec<_> = sources
        .values()
        .map(|file| {
            let (extract, _) = profile(file.language);
            let mut nodes = extract(&file.parsed.1);
            if matches!(file.language, Language::JavaScript | Language::TypeScript | Language::Tsx) {
                nodes.extend(web_named_functions(&file.parsed.1));
            }
            let ordinary = nodes.len();
            if matches!(file.language, Language::JavaScript | Language::TypeScript | Language::Tsx)
                && rules.iter().any(|(rule, language)| rule.spec.tracks_result_use() && file.language.matches_rule_language(*language))
            {
                let mut seen: HashSet<_> = nodes.iter().map(Node::id).collect();
                for node in web_result_functions(&file.parsed.1) {
                    if seen.insert(node.id()) {
                        nodes.push(node);
                    }
                }
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
        let typed = rule.spec.contract.as_ref().is_some_and(|contract| contract.typed_promise_factory);
        let units: Vec<_> = sources
            .iter()
            .zip(&functions)
            .filter(|((_, file), _)| !typed || matches!(file.language, Language::JavaScript | Language::TypeScript | Language::Tsx))
            .map(|((path, file), (ordinary, functions))| KeyedUnit {
                path,
                source: file.parsed.0.as_bytes(),
                functions: if rule.spec.tracks_result_use() { functions } else { &functions[..*ordinary] },
                imports: &file.imports,
                exports: &file.exports,
                entry: entries.contains(path) && file.language.matches_rule_language(declared),
            })
            .collect();
        if !units.iter().any(|unit| unit.entry) { continue; }
        let result = if typed {
            check_typed_rule(&rule.spec, &units, sources, root, checker)
        } else {
            check_keyed_units_with_coverage(&rule.spec, &units)
        };
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

impl Resolver<'_> {
    fn load(&self, entries: &BTreeSet<String>) -> (BTreeMap<String, Source>, Vec<AnalysisCoverageIssue>) {
        self.load_with_limit(entries, UNIT_LIMIT)
    }

    fn load_with_limit(&self, entries: &BTreeSet<String>, limit: usize) -> (BTreeMap<String, Source>, Vec<AnalysisCoverageIssue>) {
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
            if sources.len() < entries.len().saturating_add(limit) {
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
            if sources.len() >= entries.len().saturating_add(limit) {
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use super::super::cache::PersistentSummaryCache;

    fn with_resolver<T>(dir: &Path, run: impl FnOnce(&Resolver<'_>) -> T) -> T {
        let root = dir.canonicalize().unwrap();
        let parse = ParseCache::new();
        let persistent = PersistentSummaryCache::load(&root, &root);
        let caches = SummaryCaches { parse: &parse, persistent: &persistent, java_kotlin_packages: Default::default() };
        run(&Resolver { root, cache: &parse, packages: &caches })
    }

    #[test]
    fn dependency_budget_reports_only_unvisited_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ts"), "import { b } from './b'; b();").unwrap();
        std::fs::write(dir.path().join("b.ts"), "import { c } from './c'; export function b() { c(); }").unwrap();
        std::fs::write(dir.path().join("c.ts"), "export function c() {}").unwrap();
        let entries = BTreeSet::from(["a.ts".to_owned()]);
        with_resolver(dir.path(), |resolver| {
            let (limited, issues) = resolver.load_with_limit(&entries, 1);
            assert_eq!(limited.len(), 2);
            assert!(issues.iter().any(|issue| issue.reason == "dependency_limit"));
            let (complete, issues) = resolver.load_with_limit(&entries, 2);
            assert_eq!(complete.len(), 3);
            assert!(!issues.iter().any(|issue| issue.reason == "dependency_limit"));
        });
    }

    #[test]
    fn dependency_limit_is_scoped_to_rules_in_its_graph() {
        let rules = super::super::super::call_order_rules::load_call_order_rules(&[]);
        let ordinary = rules.iter().find(|rule| rule.language == "TypeScript" && !rule.spec.contract.as_ref().is_some_and(|contract| contract.typed_promise_factory)).unwrap();
        let entries = BTreeSet::from(["main.ts".to_owned()]);
        let coverage = scope_coverage(vec![issue(Some("dependency.ts"), "dependency_limit", "dependency", "limit")], &[(ordinary, Language::TypeScript)], &entries);
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].rule.as_deref(), Some(ordinary.id.as_str()));
        assert_ne!(coverage[0].rule.as_deref(), Some("typescript-factory-promise-unobserved"));
    }

    #[test]
    fn typed_rule_runs_with_trusted_checker_and_reports_missing_configuration() {
        use autoreview_dataflow::call_order::{CallOrderSpec, ContractLanguage, ContractMode, ResultContract};
        use autoreview_schema::Severity;
        let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("main.ts", "import { upload } from './barrel'; export function run() { upload('x'); }"),
            ("barrel.ts", "export * from './binding';"),
            ("binding.ts", "import create from './factory'; export const upload = create({});"),
            ("factory.ts", "export default function create(x: object): (ctx: string) => Promise<void> { throw new Error(); }"),
        ];
        for (path, source) in files { std::fs::write(dir.path().join(path), source).unwrap(); }
        std::fs::write(dir.path().join("other.rs"), "fn other() {}\n").unwrap();
        let config = dir.path().join("tsconfig.json");
        std::fs::write(&config, r#"{"compilerOptions":{"target":"ES2022","strict":true},"include":["*.ts"]}"#).unwrap();
        let entries = BTreeSet::from(["main.ts".to_owned(), "other.rs".to_owned()]);
        let (sources, issues) = with_resolver(dir.path(), |resolver| resolver.load(&entries));
        assert!(issues.is_empty());
        let rule = CallOrderRuleDef { id: "test-typed".into(), language: "TypeScript".into(), category: "correctness".into(), severity: Severity::Medium, message: "unused promise".into(), pack_id: None, metadata: None,
            spec: CallOrderSpec { rule_id: "test-typed".into(), after: vec![], unless: vec![], before: vec![], check_before_return: false, identity: None, contract: Some(ResultContract { mode: ContractMode::MustUse, language: ContractLanguage::Web, import: String::new(), methods: vec![], borrows: vec![], acquire_awaited: false, complete_observed: false, typed_promise_factory: true }) } };
        let rules = [(&rule, Language::TypeScript)];
        let mut coverage = Vec::new();
        assert!(run_sources(&sources, &entries, &rules, dir.path(), None, &mut coverage).is_empty());
        assert!(coverage.iter().any(|issue| issue.reason == "typed_promise_source_unavailable"));
        let checker = TrustedTypeScript { node: "node".into(), compiler, config: config.to_str().unwrap().into() };
        let mut coverage = Vec::new();
        let findings = run_sources(&sources, &entries, &rules, dir.path(), Some(&checker), &mut coverage);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("test-typed"));
        assert!(coverage.is_empty(), "{:?}", coverage.iter().map(|issue| &issue.reason).collect::<Vec<_>>());
        let paths = ["main.ts".to_owned(), "other.rs".to_owned()];
        let (findings, coverage) = with_resolver(dir.path(), |resolver| run_group(resolver, &paths, &rules, dir.path(), (Some(&checker), TYPED_UNIT_LIMIT)));
        assert_eq!(findings.len(), 1);
        assert!(coverage.is_empty(), "{:?}", coverage.iter().map(|issue| &issue.reason).collect::<Vec<_>>());
    }

    #[test]
    fn builtin_typed_rule_fixtures_cover_typescript_and_tsx() {
        let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
        let rules = super::super::super::call_order_rules::load_call_order_rules(&[]);
        let rule = rules.iter().find(|rule| rule.id == "typescript-factory-promise-unobserved").unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rules/typescript-factory-promise-unobserved");
        for extension in ["ts", "tsx"] {
            for (fixture, expected) in [("positive", 1), ("negative", 0)] {
                let dir = tempfile::tempdir().unwrap();
                let entry = format!("main.{extension}");
                std::fs::copy(fixtures.join(format!("{fixture}.{extension}")), dir.path().join(&entry)).unwrap();
                for path in ["binding.ts", "factory.ts"] { std::fs::copy(fixtures.join(path), dir.path().join(path)).unwrap(); }
                let config = dir.path().join("tsconfig.json");
                std::fs::write(&config, r#"{"compilerOptions":{"target":"ES2022","strict":true,"jsx":"preserve"},"include":["*.ts","*.tsx"]}"#).unwrap();
                let entries = BTreeSet::from([entry]);
                let (sources, mut coverage) = with_resolver(dir.path(), |resolver| resolver.load(&entries));
                let checker = TrustedTypeScript { node: "node".into(), compiler: compiler.clone(), config: config.to_str().unwrap().into() };
                let found = run_sources(&sources, &entries, &[(rule, Language::TypeScript)], dir.path(), Some(&checker), &mut coverage);
                assert_eq!(found.len(), expected, "{fixture}.{extension}");
                assert!(coverage.is_empty(), "{fixture}.{extension}: {:?}", coverage.iter().map(|issue| &issue.reason).collect::<Vec<_>>());
            }
        }
    }
}

