use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use autoreview_dataflow::call_order::{check_keyed_units_with_coverage, check_keyed_units_with_verified_components, CallOrderSpec, FactorySourceDiscovery, KeyedCheckResult, KeyedLimitation, KeyedUnit, ResultComponents};
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
    version: u32,
    status: String,
    reason: Option<String>,
    calls: Option<Vec<CallResult>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallResult {
    path: String,
    start: usize,
    status: String,
    #[serde(default)]
    result_count: usize,
    #[serde(default)]
    error_indexes: Vec<usize>,
}

struct Group {
    indices: Vec<(usize, usize)>,
    calls: Vec<Call>,
    expected: BTreeMap<String, String>,
}

struct Verified {
    discovery: FactorySourceDiscovery,
    components: BTreeMap<usize, BTreeMap<usize, ResultComponents>>,
}

impl Verified {
    fn record(&mut self, unit: &KeyedUnit<'_, '_, '_>, file: &Source, position: (usize, usize), result: CallResult) {
        let (index, start) = position;
        match result.status.as_str() {
            "error" => { self.discovery.candidates.entry(index).or_default().insert(start); }
            "multi_error" if multi_supported(file, start, result.result_count) => {
                self.discovery.candidates.entry(index).or_default().insert(start);
                self.components.entry(index).or_default().insert(start, ResultComponents { count: result.result_count, indices: result.error_indexes });
            }
            "multi_error" => self.discovery.limitations.push(multi_limitation(unit, start)),
            "not_proven" => {}
            _ => self.discovery.limitations.push(limitation(Some(unit.path))),
        }
    }
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
    let mut verified = Verified { discovery: FactorySourceDiscovery { candidates: BTreeMap::new(), limitations: Vec::new() }, components: BTreeMap::new() };
    let groups = collect_groups(units, sources, root, &mut verified.discovery.limitations);
    for (index, (dir, group)) in groups.into_iter().filter(|(_, group)| !group.calls.is_empty()).enumerate() {
        if index >= PACKAGE_LIMIT || group.calls.len() > CALL_LIMIT {
            verified.discovery.limitations.push(limitation(None));
            continue;
        }
        let request = Request { package_dir: &dir, go_binary: &checker.go, expected_sources: group.expected, calls: group.calls };
        match verify(&checker.probe, &request) {
            Some(results) => for ((unit, start), result) in group.indices.into_iter().zip(results) {
                verified.record(&units[unit], &sources[units[unit].path], (unit, start), result);
            },
            None => verified.discovery.limitations.push(limitation(None)),
        }
    }
    check_keyed_units_with_verified_components(spec, units, verified.discovery, &verified.components)
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

fn verify(probe: &Path, request: &Request<'_>) -> Option<Vec<CallResult>> {
    let input = serde_json::to_vec(request).ok()?;
    let mut command = Command::new(probe);
    let output = crate::analyzers::typescript_checker::run_bounded(&mut command, input, Duration::from_secs(75))?;
    let result: Response = serde_json::from_slice(&output).ok()?;
    if result.version != 2 || result.status != "ok" || result.reason.is_some() { return None; }
    let calls = result.calls?;
    if calls.len() != request.calls.len() { return None; }
    calls.into_iter().zip(&request.calls).map(|(actual, expected)| {
        (actual.path == expected.path && actual.start == expected.start && valid_call_result(&actual))
            .then_some(actual)
    }).collect()
}

fn multi_supported(file: &Source, start: usize, count: usize) -> bool {
    let root = file.parsed.1.root_node();
    let Some(inner) = root.descendant_for_byte_range(start, start.saturating_add(1)) else { return false; };
    let Some(mut call) = std::iter::successors(Some(inner), |node| node.parent())
        .find(|node| node.kind() == "call_expression" && node.start_byte() == start) else { return false; };
    while let Some(parent) = call.parent().filter(|node| node.kind() == "parenthesized_expression") { call = parent; }
    call.parent().is_some_and(|parent| match parent.kind() {
        "expression_statement" | "defer_statement" | "go_statement" => true,
        "expression_list" => multi_list_supported(parent, count),
        "argument_list" => parent.named_children(&mut parent.walk()).filter(|node| node.kind() != "comment").count() == 1
            && parent.parent().is_some_and(|outer| outer.kind() == "call_expression"),
        _ => false,
    })
}

fn multi_list_supported(list: tree_sitter::Node<'_>, count: usize) -> bool {
    if list.named_children(&mut list.walk()).filter(|node| node.kind() != "comment").count() != 1 { return false; }
    let Some(owner) = list.parent() else { return false; };
    match owner.kind() {
        "return_statement" => true,
        "short_var_declaration" | "assignment_statement" => owner.child_by_field_name("left")
            .is_some_and(|left| left.named_children(&mut left.walk()).filter(|node| node.kind() != "comment").count() == count),
        "var_spec" => owner.children_by_field_name("name", &mut owner.walk()).count() == count,
        _ => false,
    }
}

fn valid_call_result(result: &CallResult) -> bool {
    match result.status.as_str() {
        "error" | "not_proven" | "unavailable" => result.result_count == 0 && result.error_indexes.is_empty(),
        "multi_error" => result.result_count > 1 && !result.error_indexes.is_empty()
            && result.error_indexes.iter().all(|index| *index < result.result_count)
            && result.error_indexes.windows(2).all(|pair| pair[0] < pair[1]),
        _ => false,
    }
}

fn multi_limitation(unit: &KeyedUnit<'_, '_, '_>, start: usize) -> KeyedLimitation {
    let line = unit.source.get(..start).map(|source| source.iter().filter(|byte| **byte == b'\n').count() as u32 + 1);
    KeyedLimitation { reason: "go_multi_result_unmodeled", path: Some(unit.path.to_owned()), line }
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

    #[test]
    fn rejects_inconsistent_multi_result_evidence() {
        let item = |count, indexes| CallResult { path: "main.go".into(), start: 1, status: "multi_error".into(), result_count: count, error_indexes: indexes };
        assert!(valid_call_result(&item(3, vec![1, 2])));
        assert!(!valid_call_result(&item(1, vec![0])));
        assert!(!valid_call_result(&item(2, vec![])));
        assert!(!valid_call_result(&item(2, vec![2])));
        assert!(!valid_call_result(&item(3, vec![1, 1])));
        assert!(!valid_call_result(&item(3, vec![2, 1])));
        assert!(serde_json::from_str::<Response>(r#"{"status":"ok","calls":[]}"#).is_err());
    }

    #[test]
    fn go_multi_result_components_are_tracked_independently() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into() };
        let source = "package sample\nfunc pair() (int, error) { return 1, nil }\nfunc two() (error, error) { return nil, nil }\nfunc sink(n int, err error) {}\nfunc ignored() { v, _ := pair(); _ = v }\nfunc checked() { v, err := pair(); if err != nil { return }; _ = v }\nfunc overwritten() { v, err := pair(); err = nil; if err != nil { return }; _ = v }\nfunc both() { a, b := two(); if a != nil { return }; _ = b }\nfunc forwarded() (int, error) { return pair() }\nfunc nested() { sink(pair()) }\nfunc bare() { pair() }\nfunc declared() { var v, e = pair(); _ = v; if e != nil { return } }\nfunc reassigned() { var v int; var e error; v, e = pair(); e = nil; _ = v; if e != nil { return } }\nfunc use(n int, err error) { if err != nil { return } }\nfunc nestedUsed() { use(pair()) }\nfunc unresolved(s func(int, error)) { s(pair()) }\nfunc one(a, b error) { if a != nil { return } }\nfunc twoUsed(a, b error) { if a != nil || b != nil { return } }\nfunc nestedOne() { one(two()) }\nfunc nestedBoth() { twoUsed(two()) }\n";
        let result = analyze_source(source, &checker);
        assert_eq!(result.hits.iter().map(|hit| hit.source_line).collect::<Vec<_>>(), [5, 7, 8, 10, 11, 13, 19], "limitations: {:?}", result.limitations.iter().map(|item| (item.reason, item.line)).collect::<Vec<_>>());
        assert!(result.limitations.iter().any(|item| item.reason == "go_multi_result_unmodeled" && item.line == Some(16)));
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
