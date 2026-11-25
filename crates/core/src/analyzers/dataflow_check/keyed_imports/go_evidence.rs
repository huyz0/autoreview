use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use autoreview_dataflow::call_order::{check_keyed_units_with_coverage, check_keyed_units_with_verified_sources, CallOrderSpec, FactorySourceDiscovery, KeyedCheckResult, KeyedLimitation, KeyedUnit};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use super::Source;

const PACKAGE_LIMIT: usize = 8;
const CALL_LIMIT: usize = 2048;

pub(super) struct TrustedGo {
    probe: PathBuf,
    go: PathBuf,
}

impl TrustedGo {
    fn from_environment() -> Option<Self> {
        let probe = PathBuf::from(std::env::var_os("AUTOREVIEW_GO_ERROR_PROBE")?);
        let go = PathBuf::from(std::env::var_os("AUTOREVIEW_GO_BINARY")?);
        (probe.is_absolute() && go.is_absolute()).then_some(Self { probe, go })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    package_dir: &'a Path,
    go_binary: &'a Path,
    expected_sources: BTreeMap<String, String>,
    calls: Vec<Call>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Call {
    path: String,
    start: usize,
}

#[derive(Deserialize)]
struct Response {
    status: String,
    reason: Option<String>,
    calls: Option<Vec<CallResult>>,
}

#[derive(Deserialize)]
struct CallResult {
    path: String,
    start: usize,
    status: String,
}

struct Group {
    indices: Vec<(usize, usize)>,
    calls: Vec<Call>,
    expected: BTreeMap<String, String>,
}

pub(super) fn check_go_rule(
    spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<String, Source>, root: &Path,
) -> KeyedCheckResult {
    let Some(checker) = TrustedGo::from_environment() else { return check_keyed_units_with_coverage(spec, units); };
    check_with_probe(spec, units, sources, root, &checker)
}

fn check_with_probe(
    spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<String, Source>, root: &Path, checker: &TrustedGo,
) -> KeyedCheckResult {
    let mut discovery = FactorySourceDiscovery { candidates: BTreeMap::new(), limitations: Vec::new() };
    let groups = collect_groups(units, sources, root, &mut discovery.limitations);
    for (index, (dir, group)) in groups.into_iter().filter(|(_, group)| !group.calls.is_empty()).enumerate() {
        if index >= PACKAGE_LIMIT || group.calls.len() > CALL_LIMIT {
            discovery.limitations.push(limitation(None));
            continue;
        }
        let request = Request { package_dir: &dir, go_binary: &checker.go, expected_sources: group.expected, calls: group.calls };
        match verify(&checker.probe, &request) {
            Some(results) => for ((unit, start), status) in group.indices.into_iter().zip(results) {
                match status.as_str() {
                    "error" => { discovery.candidates.entry(unit).or_default().insert(start); }
                    "not_proven" => {}
                    _ => discovery.limitations.push(limitation(Some(units[unit].path))),
                }
            },
            None => discovery.limitations.push(limitation(None)),
        }
    }
    check_keyed_units_with_verified_sources(spec, units, discovery)
}

fn collect_groups(
    units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<String, Source>, root: &Path, limitations: &mut Vec<KeyedLimitation>,
) -> BTreeMap<PathBuf, Group> {
    let mut groups = BTreeMap::<PathBuf, Group>::new();
    let Ok(root) = root.canonicalize() else { limitations.push(limitation(None)); return groups; };
    for (index, unit) in units.iter().enumerate() {
        let Some((dir, name, calls)) = collect_unit(unit, sources, &root) else {
            limitations.push(limitation(Some(unit.path))); continue;
        };
        let group = groups.entry(dir).or_insert_with(|| Group { indices: Vec::new(), calls: Vec::new(), expected: BTreeMap::new() });
        group.expected.insert(name, format!("{:x}", Sha256::digest(unit.source)));
        group.indices.extend(calls.iter().map(|call| (index, call.start)));
        group.calls.extend(calls);
    }
    groups
}

fn collect_unit(unit: &KeyedUnit<'_, '_, '_>, sources: &BTreeMap<String, Source>, root: &Path) -> Option<(PathBuf, String, Vec<Call>)> {
    let file = sources.get(unit.path)?;
    let resolved = root.join(unit.path).canonicalize().ok()?;
    if !resolved.starts_with(root) { return None; }
    let dir = resolved.parent()?.to_path_buf();
    let name = resolved.file_name()?.to_str()?.to_owned();
    let mut pending = vec![file.parsed.1.root_node()];
    let mut calls = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == "call_expression" { calls.push(Call { path: name.clone(), start: node.start_byte() }); }
        pending.extend(node.named_children(&mut node.walk()));
    }
    Some((dir, name, calls))
}

fn verify(probe: &Path, request: &Request<'_>) -> Option<Vec<String>> {
    let input = serde_json::to_vec(request).ok()?;
    let mut command = Command::new(probe);
    let output = crate::analyzers::typescript_checker::run_bounded(&mut command, input, Duration::from_secs(75))?;
    let result: Response = serde_json::from_slice(&output).ok()?;
    if result.status != "ok" || result.reason.is_some() { return None; }
    let calls = result.calls?;
    if calls.len() != request.calls.len() { return None; }
    calls.into_iter().zip(&request.calls).map(|(actual, expected)| {
        (actual.path == expected.path && actual.start == expected.start && matches!(actual.status.as_str(), "error" | "not_proven" | "unavailable"))
            .then_some(actual.status)
    }).collect()
}

fn limitation(path: Option<&str>) -> KeyedLimitation {
    KeyedLimitation { reason: "go_error_checker_incomplete", path: path.map(str::to_owned), line: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_dataflow::call_order::KeyedFunction;
    use autoreview_langsupport::Language;
    use std::rc::Rc;

    #[test]
    fn go_probe_sources_flow_into_result_use() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into() };
        let source = "package sample\nfunc operation() error { return nil }\nfunc run() { operation() }\nfunc observed() error { return operation() }\nfunc branch(n int) { switch n { case 1: operation(); default: return } }\nfunc unsupported(n int) { switch n { case 1: operation(); fallthrough; default: return } }\nfunc anonymous() { func() error { return nil }() }\nfunc helper() error { return operation() }\nfunc discardedHelper() { helper() }\nfunc inspected() { if err := operation(); err != nil { return } }\nfunc overwritten() { err := operation(); err = nil; if err != nil { return } }\nfunc overwrittenInSwitch(n int) { err := operation(); switch n { case 1: err = nil; default: err = nil }; if err != nil { return } }\nfunc readOnOneBranch(n int) { err := operation(); switch n { case 1: err = nil; default: }; if err != nil { return } }\nfunc deferred() { defer operation() }\nfunc detached() { go operation() }\nfunc sink(err error) {}\nfunc passedButIgnored() { sink(operation()) }\nfunc handle(err error) { if err != nil { return } }\nfunc passedAndUsed() { handle(operation()) }\n";
        let result = analyze_source(source, &checker);
        assert_eq!(result.hits.iter().map(|hit| hit.source_line).collect::<Vec<_>>(), [3, 5, 7, 9, 11, 12, 14, 17], "limitations: {:?}", result.limitations.iter().map(|item| item.reason).collect::<Vec<_>>());
        assert!(result.limitations.iter().any(|item| item.reason == "verified_source_unmodeled" && item.line == Some(6)));
        assert!(result.limitations.iter().any(|item| item.reason == "verified_source_unmodeled" && item.line == Some(15)));
        for (fixture, count) in [(include_str!("../../../../tests/fixtures/rules/go-project-error-result-unobserved/positive.go"), 1), (include_str!("../../../../tests/fixtures/rules/go-project-error-result-unobserved/negative.go"), 0)] {
            let result = analyze_source(fixture, &checker);
            assert_eq!(result.hits.len(), count);
            assert!(result.limitations.is_empty());
        }
    }

    fn analyze_source(source: &str, checker: &TrustedGo) -> KeyedCheckResult {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.test/probe\n\ngo 1.22\n").unwrap();
        std::fs::write(dir.path().join("main.go"), source).unwrap();
        let tree = autoreview_langsupport::parser_for(Language::Go).unwrap().parse(source, None).unwrap();
        let parsed = Rc::new((source.to_owned(), tree));
        let sources = BTreeMap::from([("main.go".to_owned(), Source { language: Language::Go, parsed, imports: vec![], exports: vec![] })]);
        let file = &sources["main.go"];
        let (extract, _) = super::super::profile(Language::Go);
        let nodes = extract(&file.parsed.1);
        let graphs: Vec<_> = nodes.iter().map(|node| autoreview_dataflow::lower::keyed_variant(Language::Go, source.as_bytes(), *node)).collect();
        let functions: Vec<_> = nodes.iter().zip(&graphs).map(|(node, graph)| KeyedFunction { node: *node, graph }).collect();
        let units = [KeyedUnit { path: "main.go", source: source.as_bytes(), functions: &functions, imports: &[], exports: &[], entry: true }];
        let rules = crate::analyzers::call_order_rules::load_call_order_rules(&[]);
        let rule = rules.iter().find(|rule| rule.id == "go-project-error-result-unobserved").unwrap();
        check_with_probe(&rule.spec, &units, &sources, dir.path(), checker)
    }
}
