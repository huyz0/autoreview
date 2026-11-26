use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use autoreview_dataflow::call_order::{check_keyed_units_with_coverage, check_keyed_units_with_verified_components, CallOrderSpec, FactorySourceDiscovery, KeyedCheckResult, KeyedLimitation, KeyedUnit, ResultComponents};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use super::Source;
use super::package_coverage::package_limitation;

const PACKAGE_LIMIT: usize = 8;
const CALL_LIMIT: usize = 2048;
const PACKAGE_SOURCE_LIMIT: u64 = 8 * 1024 * 1024;
const PACKAGE_FILE_LIMIT: usize = 512;

pub(super) struct TrustedGo {
    probe: PathBuf,
    go: PathBuf,
    module_mode: Option<String>,
}

impl TrustedGo {
    fn from_environment() -> Option<Self> {
        let probe = PathBuf::from(std::env::var_os("AUTOREVIEW_GO_ERROR_PROBE")?);
        let go = PathBuf::from(std::env::var_os("AUTOREVIEW_GO_BINARY")?);
        let module_mode = std::env::var("AUTOREVIEW_GO_MODULE_MODE").ok().filter(|mode| !mode.is_empty());
        (probe.is_absolute() && go.is_absolute()).then_some(Self { probe, go, module_mode })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    package_dir: &'a Path,
    go_binary: &'a Path,
    expected_sources: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_sources: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    module_mode: Option<&'a str>,
    calls: Vec<Call>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    include_callees: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    include_error_origins: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency_sources: Option<&'a BTreeMap<String, DependencySnapshot>>,
}

#[derive(Serialize)]
struct DependencySnapshot { directory: PathBuf, files: BTreeMap<String, String> }

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
    #[serde(default)]
    callee: Option<Callee>,
    #[serde(default)]
    error_origin: Option<ErrorOrigin>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Callee { package: String, name: String, #[serde(default, skip_serializing_if = "String::is_empty")] receiver: String }

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorOrigin { kind: String, context_parameter: usize, #[serde(default, skip_serializing_if = "Option::is_none")] specialized: Option<IntegerEvidence> }

#[derive(Clone, Deserialize, Serialize)]
struct IntegerEvidence { parameter: usize, value: String }

#[derive(Default)]
pub(super) struct CallEvidence { callee: Option<serde_json::Value>, error_origin: Option<serde_json::Value> }
pub(super) type CalleeEvidence = BTreeMap<(String, usize), CallEvidence>;

pub(super) fn finding(path: &str, source: Option<&str>, rule: &super::CallOrderRuleDef, hit: &autoreview_dataflow::call_order::CallOrderHit, callees: &CalleeEvidence) -> super::AgentFinding {
    let mut finding = super::super::dsl::call_order_finding(path, source, rule, hit);
    if let Some(evidence) = hit.source_start.and_then(|start| callees.get(&(path.to_owned(), start))) {
        if let Some(callee) = &evidence.callee { finding.meta.get_or_insert_with(Default::default).insert("goTypeCheckedCallee".into(), callee.clone()); }
        if let Some(origin) = &evidence.error_origin { finding.meta.get_or_insert_with(Default::default).insert("goErrorOrigin".into(), origin.clone()); }
    }
    finding
}

struct Group {
    indices: Vec<(usize, usize)>,
    calls: Vec<Call>,
    expected: BTreeMap<String, String>,
}

struct Verified {
    discovery: FactorySourceDiscovery,
    components: BTreeMap<usize, BTreeMap<usize, ResultComponents>>,
    callees: CalleeEvidence,
}

impl Verified {
    fn record(&mut self, unit: &KeyedUnit<'_, '_, '_>, file: &Source, position: (usize, usize), result: CallResult) {
        let (index, start) = position;
        self.record_evidence(unit.path, start, &result);
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

    fn record_evidence(&mut self, path: &str, start: usize, result: &CallResult) {
        if let Some(callee) = &result.callee {
            if valid_callee(callee) {
                if let Ok(value) = serde_json::to_value(callee) { self.callees.entry((path.to_owned(), start)).or_default().callee = Some(value); }
            }
        }
        if let Some(origin) = &result.error_origin {
            if valid_error_origin(origin) {
                if let Ok(value) = serde_json::to_value(origin) { self.callees.entry((path.to_owned(), start)).or_default().error_origin = Some(value); }
            }
        }
    }
}

fn valid_error_origin(origin: &ErrorOrigin) -> bool {
    origin.kind == "context_error_or_nil" && origin.context_parameter < 1024
        && origin.specialized.as_ref().is_none_or(|item| item.parameter < 1024 && item.value.len() <= 40 && item.value.parse::<i128>().is_ok())
}

pub(super) fn check_go_rule(
    spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<String, Source>, root: &Path,
) -> (KeyedCheckResult, CalleeEvidence) {
    let Some(checker) = TrustedGo::from_environment() else { return (check_keyed_units_with_coverage(spec, units), BTreeMap::new()); };
    let include_error_origins = std::env::var_os("AUTOREVIEW_GO_ERROR_ORIGIN_EVIDENCE").is_some_and(|value| value == "1");
    let config = std::env::var_os("AUTOREVIEW_GO_ERROR_ORIGIN_DEPENDENCIES");
    let dependencies = if include_error_origins { config.as_deref().map(|value| value.to_str().ok_or(()).and_then(|text| dependency_snapshots(root, text))).transpose() } else { Ok(None) };
    let invalid = dependencies.is_err();
    let dependencies = dependencies.ok().flatten();
    let options = ProbeOptions { root, checker: &checker, include_callees: std::env::var_os("AUTOREVIEW_GO_CALLEE_EVIDENCE").is_some_and(|value| value == "1"), include_error_origins: include_error_origins && !invalid, dependencies: dependencies.as_ref() };
    let (mut result, evidence) = check_with_probe_and_callees(spec, units, sources, options);
    if invalid { result.limitations.push(dependency_limitation()); }
    (result, evidence)
}

struct ProbeOptions<'a> { root: &'a Path, checker: &'a TrustedGo, include_callees: bool, include_error_origins: bool, dependencies: Option<&'a BTreeMap<String, DependencySnapshot>> }

fn check_with_probe_and_callees(
    spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<String, Source>, options: ProbeOptions<'_>,
) -> (KeyedCheckResult, CalleeEvidence) {
    let ProbeOptions { root, checker, include_callees, include_error_origins, dependencies } = options;
    let mut verified = Verified { discovery: FactorySourceDiscovery { candidates: BTreeMap::new(), limitations: Vec::new() }, components: BTreeMap::new(), callees: BTreeMap::new() };
    let groups = collect_groups(units, sources, root, &mut verified.discovery.limitations);
    for (index, (dir, group)) in groups.into_iter().filter(|(_, group)| !group.calls.is_empty()).enumerate() {
        if index >= PACKAGE_LIMIT || group.calls.len() > CALL_LIMIT {
            let reason = if index >= PACKAGE_LIMIT { "go_error_package_limit" } else { "go_error_call_limit" };
            verified.discovery.limitations.push(package_limitation(root, &dir, reason));
            continue;
        }
        let package_sources = (include_callees || include_error_origins).then(|| package_source_hashes(&dir)).flatten();
        let mut request = Request { package_dir: &dir, go_binary: &checker.go, expected_sources: group.expected, package_sources, module_mode: checker.module_mode.as_deref(), calls: group.calls, include_callees, include_error_origins, dependency_sources: dependencies };
        let results = verify_with_fallback(&checker.probe, &mut request);
        if dependencies.is_some() && request.dependency_sources.is_none() { verified.discovery.limitations.push(dependency_limitation()); }
        match results {
            Ok(results) => for ((unit, start), result) in group.indices.into_iter().zip(results) {
                record_result(&mut verified, units, sources, &request, (unit, start, result));
            },
            Err(reason) => verified.discovery.limitations.push(package_limitation(root, &dir, reason)),
        }
    }
    (check_keyed_units_with_verified_components(spec, units, verified.discovery, &verified.components), verified.callees)
}

fn record_result(verified: &mut Verified, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<String, Source>, request: &Request<'_>, item: (usize, usize, CallResult)) {
    let (unit, start, mut result) = item;
    if !request.include_callees { result.callee = None; }
    if !request.include_error_origins { result.error_origin = None; }
    verified.record(&units[unit], &sources[units[unit].path], (unit, start), result);
}

fn verify_with_fallback(probe: &Path, request: &mut Request<'_>) -> Result<Vec<CallResult>, &'static str> {
    verify(probe, request).or_else(|reason| {
        if !request.include_error_origins { return Err(reason); }
        request.include_error_origins = false;
        request.dependency_sources = None;
        if !request.include_callees { request.package_sources = None; }
        verify(probe, request)
    }).or_else(|reason| {
        if !request.include_callees { return Err(reason); }
        request.include_callees = false;
        request.package_sources = None;
        verify(probe, request)
    })
}

fn dependency_limitation() -> KeyedLimitation {
    KeyedLimitation { reason: "go_dependency_evidence_incomplete", path: None, line: None }
}

fn dependency_snapshots(root: &Path, config: &str) -> Result<BTreeMap<String, DependencySnapshot>, ()> {
    let paths: BTreeMap<String, String> = serde_json::from_str(config).map_err(|_| ())?;
    if paths.is_empty() || paths.len() > PACKAGE_LIMIT { return Err(()); }
    let root = root.canonicalize().map_err(|_| ())?;
    paths.into_iter().map(|(package, relative)| {
        let path = Path::new(&relative);
        if package.is_empty() || package.len() > 1024 || package.chars().any(char::is_control) || path.is_absolute() { return Err(()); }
        let directory = root.join(path).canonicalize().map_err(|_| ())?;
        if !directory.starts_with(&root) || !directory.is_dir() { return Err(()); }
        let files = package_source_hashes(&directory).ok_or(())?;
        if files.is_empty() { return Err(()); }
        Ok((package, DependencySnapshot { directory, files }))
    }).collect()
}

fn package_source_hashes(dir: &Path) -> Option<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().into_string().ok()?;
        if !name.ends_with(".go") { continue; }
        let metadata = entry.file_type().ok()?;
        if !metadata.is_file() || hashes.len() >= PACKAGE_FILE_LIMIT { return None; }
        let size = entry.metadata().ok()?.len();
        total = total.checked_add(size)?;
        if total > PACKAGE_SOURCE_LIMIT { return None; }
        let mut bytes = Vec::new();
        std::fs::File::open(entry.path()).ok()?.take(size.saturating_add(1)).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 != size { return None; }
        hashes.insert(name, format!("{:x}", Sha256::digest(bytes)));
    }
    Some(hashes)
}

fn valid_callee(callee: &Callee) -> bool {
    !callee.package.is_empty() && !callee.name.is_empty()
        && [&callee.package, &callee.name, &callee.receiver].iter().all(|part| part.len() <= 1024 && !part.chars().any(char::is_control))
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

fn verify(probe: &Path, request: &Request<'_>) -> Result<Vec<CallResult>, &'static str> {
    const INCOMPLETE: &str = "go_error_checker_incomplete";
    let input = serde_json::to_vec(request).map_err(|_| INCOMPLETE)?;
    let mut command = Command::new(probe);
    let output = crate::analyzers::typescript_checker::run_bounded(&mut command, input, Duration::from_secs(75)).ok_or(INCOMPLETE)?;
    let result: Response = serde_json::from_slice(&output).map_err(|_| INCOMPLETE)?;
    if result.version == 2 && result.status == "incomplete" && result.calls.is_none() {
        return Err(probe_reason(result.reason.as_deref()));
    }
    if result.version != 2 || result.status != "ok" || result.reason.is_some() { return Err(INCOMPLETE); }
    let calls = result.calls.ok_or(INCOMPLETE)?;
    if calls.len() != request.calls.len() { return Err(INCOMPLETE); }
    calls.into_iter().zip(&request.calls).map(|(actual, expected)| {
        (actual.path == expected.path && actual.start == expected.start && valid_call_result(&actual))
            .then_some(actual).ok_or(INCOMPLETE)
    }).collect()
}

fn probe_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("package_load_failed" | "package_load_incomplete" | "package_sources_unavailable") => "go_error_package_load_incomplete",
        Some("type_errors" | "dependency_type_errors" | "syntax_errors") => "go_error_typecheck_incomplete",
        Some("source_mismatch" | "dependency_source_mismatch") => "go_error_source_mismatch",
        Some("invalid_package_config" | "invalid_module_mode" | "invalid_request" | "no_files" | "invalid_files") => "go_error_probe_invalid_request",
        _ => "go_error_checker_incomplete",
    }
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
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
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
        let item = |count, indexes| CallResult { path: "main.go".into(), start: 1, status: "multi_error".into(), result_count: count, error_indexes: indexes, callee: None, error_origin: None };
        assert!(valid_call_result(&item(3, vec![1, 2])));
        assert!(!valid_call_result(&item(1, vec![0])));
        assert!(!valid_call_result(&item(2, vec![])));
        assert!(!valid_call_result(&item(2, vec![2])));
        assert!(!valid_call_result(&item(3, vec![1, 1])));
        assert!(!valid_call_result(&item(3, vec![2, 1])));
        assert!(serde_json::from_str::<Response>(r#"{"status":"ok","calls":[]}"#).is_err());
    }

    #[test]
    fn probe_failure_reasons_are_bounded_and_unknown_reasons_remain_generic() {
        assert_eq!(probe_reason(Some("package_load_failed")), "go_error_package_load_incomplete");
        assert_eq!(probe_reason(Some("type_errors")), "go_error_typecheck_incomplete");
        assert_eq!(probe_reason(Some("source_mismatch")), "go_error_source_mismatch");
        assert_eq!(probe_reason(Some("invalid_module_mode")), "go_error_probe_invalid_request");
        assert_eq!(probe_reason(Some("unrecognized private details")), "go_error_checker_incomplete");
        assert_eq!(probe_reason(None), "go_error_checker_incomplete");
    }

    #[test]
    fn go_probe_reports_package_load_and_configuration_failures() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let source = "package sample\nfunc operation() error { return nil }\nfunc use() { operation() }\n";
        let invalid = TrustedGo { probe: probe.clone().into(), go: go.into(), module_mode: Some("invalid".into()) };
        let result = analyze_source(source, &invalid);
        assert!(result.hits.is_empty());
        assert!(result.limitations.iter().any(|item| item.reason == "go_error_probe_invalid_request"));
        let missing = TrustedGo { probe: probe.into(), go: std::env::temp_dir().join("autoreview-missing-go-binary"), module_mode: None };
        let result = analyze_source(source, &missing);
        assert!(result.hits.is_empty());
        assert!(result.limitations.iter().any(|item| item.reason == "go_error_package_load_incomplete"));
    }

    #[test]
    fn go_multi_result_components_are_tracked_independently() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let source = "package sample\nfunc pair() (int, error) { return 1, nil }\nfunc two() (error, error) { return nil, nil }\nfunc sink(n int, err error) {}\nfunc ignored() { v, _ := pair(); _ = v }\nfunc checked() { v, err := pair(); if err != nil { return }; _ = v }\nfunc overwritten() { v, err := pair(); err = nil; if err != nil { return }; _ = v }\nfunc both() { a, b := two(); if a != nil { return }; _ = b }\nfunc forwarded() (int, error) { return pair() }\nfunc nested() { sink(pair()) }\nfunc bare() { pair() }\nfunc declared() { var v, e = pair(); _ = v; if e != nil { return } }\nfunc reassigned() { var v int; var e error; v, e = pair(); e = nil; _ = v; if e != nil { return } }\nfunc use(n int, err error) { if err != nil { return } }\nfunc nestedUsed() { use(pair()) }\nfunc unresolved(s func(int, error)) { s(pair()) }\nfunc one(a, b error) { if a != nil { return } }\nfunc twoUsed(a, b error) { if a != nil || b != nil { return } }\nfunc nestedOne() { one(two()) }\nfunc nestedBoth() { twoUsed(two()) }\n";
        let result = analyze_source(source, &checker);
        assert_eq!(result.hits.iter().map(|hit| hit.source_line).collect::<Vec<_>>(), [5, 7, 8, 10, 11, 13, 19], "limitations: {:?}", result.limitations.iter().map(|item| (item.reason, item.line)).collect::<Vec<_>>());
        assert!(result.limitations.iter().any(|item| item.reason == "go_multi_result_unmodeled" && item.line == Some(16)));
    }

    #[test]
    fn go_callee_evidence_tracks_exact_origin_and_keeps_dynamic_calls_unknown() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let source = "package sample\nfunc first() error { return nil }\nfunc second() error { return nil }\nfunc use(f func() error) { first(); second(); f() }\nfunc duplicate() { first(); first() }\n";
        let (result, callees) = analyze_source_with_origins(source, &checker, true, &[], true);
        assert_eq!(result.hits.len(), 4);
        for (name, expected) in [("first()", "first"), ("second()", "second")] {
            let start = source.find("func use").unwrap() + source[source.find("func use").unwrap()..].find(name).unwrap();
            let hit = result.hits.iter().find(|hit| hit.source_start == Some(start)).unwrap();
            let callee = callees[&(hit.path.clone().unwrap(), start)].callee.as_ref().unwrap();
            assert_eq!(callee["package"], "example.test/probe");
            assert_eq!(callee["name"], expected);
        }
        assert!(!callees.contains_key(&("main.go".to_owned(), source.rfind("f()").unwrap())));
        assert!(result.hits.iter().any(|hit| hit.source_line == 5 && hit.source_start.is_none()));
        let rules = crate::analyzers::call_order_rules::load_call_order_rules(&[]);
        let rule = rules.iter().find(|rule| rule.id == "go-project-error-result-unobserved").unwrap();
        let hit = result.hits.iter().find(|hit| hit.source_line == 4 && hit.trigger_call == "first").unwrap();
        let finding = super::finding("main.go", Some(source), rule, hit, &callees);
        assert_eq!(finding.meta.unwrap()["goTypeCheckedCallee"]["name"], "first");
    }

    #[test]
    fn old_go_probe_falls_back_to_result_type_analysis() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_LEGACY_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let source = "package sample\nfunc operation() error { return nil }\nfunc use() { operation() }\n";
        let (result, callees) = analyze_source_with_origins(source, &checker, true, &[], true);
        assert_eq!(result.hits.len(), 1);
        assert!(result.limitations.is_empty());
        assert!(callees.is_empty());
    }

    #[test]
    fn old_go_probe_does_not_silently_ignore_vendor_mode() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_LEGACY_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: Some("vendor".into()) };
        let source = "package sample\nfunc operation() error { return nil }\nfunc use() { operation() }\n";
        let (result, _) = analyze_source_with_evidence(source, &checker, true);
        assert!(result.hits.is_empty());
        assert!(result.limitations.iter().any(|item| item.reason == "go_error_checker_incomplete"));
    }

    #[test]
    fn callee_evidence_covers_selected_files_not_loaded_as_helpers() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let source = "package sample\nfunc operation() error { return nil }\nfunc use() { operation() }\n";
        let extra = [("other.go", "package sample\nvar setting = 1\n"), ("other_test.go", "package sample\nthis is not parsed\n")];
        let (result, callees) = analyze_source_with_files(source, &checker, true, &extra);
        let start = source.rfind("operation()").unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(callees[&("main.go".to_owned(), start)].callee.as_ref().unwrap()["name"], "operation");
    }

    #[test]
    fn vendored_go_dependency_flows_into_findings() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: Some("vendor".into()) };
        let source = "package sample\nimport dep \"example.invalid/offline-dep\"\nfunc use() { dep.Operation() }\n";
        let files = [
            ("go.mod", "module example.test/probe\n\ngo 1.22\n\nrequire example.invalid/offline-dep v1.0.0\n"),
            ("vendor/modules.txt", "# example.invalid/offline-dep v1.0.0\n## explicit; go 1.22\nexample.invalid/offline-dep\n"),
            ("vendor/example.invalid/offline-dep/dep.go", "package dep\nfunc Operation() error { return nil }\n"),
        ];
        let (result, callees) = analyze_source_with_files(source, &checker, true, &files);
        assert_eq!(result.hits.len(), 1);
        assert!(result.limitations.is_empty());
        let call = callees[&("main.go".into(), source.find("dep.Operation()").unwrap())].callee.as_ref().unwrap();
        assert_eq!(call["package"], "example.invalid/offline-dep");
    }

    #[test]
    fn context_error_evidence_reaches_result_finding() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let source = "package sample\nimport (\"context\"; \"errors\")\nfunc operation(ctx context.Context, n int) error { if n > 1 { return errors.New(\"other\") }; return ctx.Err() }\nfunc use(ctx context.Context) { operation(ctx, 1) }\n";
        let (result, evidence) = analyze_source_with_origins(source, &checker, false, &[], true);
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        let rules = crate::analyzers::call_order_rules::load_call_order_rules(&[]);
        let rule = rules.iter().find(|rule| rule.id == "go-project-error-result-unobserved").unwrap();
        let finding = super::finding("main.go", Some(source), rule, hit, &evidence);
        let origin = &finding.meta.unwrap()["goErrorOrigin"];
        assert_eq!(origin["kind"], "context_error_or_nil");
        assert_eq!(origin["contextParameter"], 0);
        assert_eq!(origin["specialized"]["parameter"], 1);
        assert_eq!(origin["specialized"]["value"], "1");
    }

    #[test]
    fn context_error_evidence_crosses_same_package_helper() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let source = "package sample\nimport \"context\"\nfunc wrapper(ctx context.Context) error { err := operation(ctx); if err != nil { return err }; return nil }\nfunc operation(ctx context.Context) error { return ctx.Err() }\nfunc use(ctx context.Context) { wrapper(ctx) }\n";
        let (result, evidence) = analyze_source_with_origins(source, &checker, false, &[], true);
        assert_wrapper_origin(&result, &evidence);
    }

    #[test]
    fn context_error_evidence_crosses_verified_dependency() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let (result, evidence) = analyze_dependency_fixture(&checker);
        assert!(result.limitations.is_empty());
        assert_wrapper_origin(&result, &evidence);
    }

    #[test]
    fn specialized_dependency_evidence_reaches_result_finding() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let source = "package sample\nimport (\"context\"; \"example.test/probe/dep\")\nfunc use(ctx context.Context) {\n dep.Guarded(ctx, 1)\n dep.Guarded(ctx, 2)\n}\n";
        let files = [("dep/dep.go", "package dep\nimport (\"context\"; \"errors\")\nfunc Guarded(ctx context.Context, n int) error { if n > 1 { return errors.New(\"other\") }; return ctx.Err() }\n")];
        let (result, evidence) = analyze_source_with_config(source, &checker, &files, (false, true, Some(r#"{"example.test/probe/dep":"dep"}"#)));
        assert!(result.limitations.is_empty());
        assert_eq!(result.hits.len(), 2);
        let rules = crate::analyzers::call_order_rules::load_call_order_rules(&[]);
        let rule = rules.iter().find(|rule| rule.id == "go-project-error-result-unobserved").unwrap();
        for (index, expression) in ["dep.Guarded(ctx, 1)", "dep.Guarded(ctx, 2)"].iter().enumerate() {
            let start = source.find(expression).unwrap();
            let hit = result.hits.iter().find(|hit| hit.source_start == Some(start)).unwrap();
            let finding = super::finding("main.go", Some(source), rule, hit, &evidence);
            let origin = finding.meta.as_ref().and_then(|meta| meta.get("goErrorOrigin"));
            if index == 0 {
                let origin = origin.unwrap();
                assert_eq!(origin["contextParameter"], 0);
                assert_eq!(origin["specialized"]["parameter"], 1);
                assert_eq!(origin["specialized"]["value"], "1");
            } else {
                assert!(origin.is_none());
            }
        }
    }

    #[test]
    fn older_probe_reports_unavailable_dependency_evidence_and_keeps_findings() {
        let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_LEGACY_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
        let checker = TrustedGo { probe: probe.into(), go: go.into(), module_mode: None };
        let (result, evidence) = analyze_dependency_fixture(&checker);
        assert!(result.hits.iter().any(|hit| hit.trigger_call == "wrapper"));
        assert!(result.limitations.iter().any(|item| item.reason == "go_dependency_evidence_incomplete"));
        assert!(evidence.is_empty());
    }

    #[test]
    fn dependency_snapshot_paths_must_stay_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(dependency_snapshots(dir.path(), r#"{"example.test/dep":".."}"#).is_err());
        assert!(dependency_snapshots(dir.path(), "not-json").is_err());
        assert!(dependency_snapshots(dir.path(), "{}").is_err());
    }

    fn analyze_dependency_fixture(checker: &TrustedGo) -> (KeyedCheckResult, CalleeEvidence) {
        let source = "package sample\nimport (\"context\"; \"example.test/probe/dep\")\nfunc wrapper(ctx context.Context) error { return dep.Leaf(ctx) }\nfunc use(ctx context.Context) { wrapper(ctx) }\n";
        let files = [("dep/dep.go", "package dep\nimport \"context\"\nfunc Leaf(ctx context.Context) error { return ctx.Err() }\n")];
        analyze_source_with_config(source, checker, &files, (false, true, Some(r#"{"example.test/probe/dep":"dep"}"#)))
    }

    fn assert_wrapper_origin(result: &KeyedCheckResult, evidence: &CalleeEvidence) {
        let hit = result.hits.iter().find(|hit| hit.trigger_call == "wrapper").unwrap();
        let origin = evidence[&(hit.path.clone().unwrap(), hit.source_start.unwrap())].error_origin.as_ref().unwrap();
        assert_eq!(origin["kind"], "context_error_or_nil");
        assert_eq!(origin["contextParameter"], 0);
    }

    fn analyze_source(source: &str, checker: &TrustedGo) -> KeyedCheckResult {
        analyze_source_with_evidence(source, checker, false).0
    }

    fn analyze_source_with_evidence(source: &str, checker: &TrustedGo, include_callees: bool) -> (KeyedCheckResult, CalleeEvidence) {
        analyze_source_with_files(source, checker, include_callees, &[])
    }

    fn analyze_source_with_files(source: &str, checker: &TrustedGo, include_callees: bool, extra: &[(&str, &str)]) -> (KeyedCheckResult, CalleeEvidence) {
        analyze_source_with_origins(source, checker, include_callees, extra, false)
    }

    fn analyze_source_with_origins(source: &str, checker: &TrustedGo, include_callees: bool, extra: &[(&str, &str)], include_error_origins: bool) -> (KeyedCheckResult, CalleeEvidence) {
        analyze_source_with_config(source, checker, extra, (include_callees, include_error_origins, None))
    }

    fn analyze_source_with_config(source: &str, checker: &TrustedGo, extra: &[(&str, &str)], options: (bool, bool, Option<&str>)) -> (KeyedCheckResult, CalleeEvidence) {
        let (include_callees, include_error_origins, config) = options;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.test/probe\n\ngo 1.22\n").unwrap();
        std::fs::write(dir.path().join("main.go"), source).unwrap();
        for (name, contents) in extra {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
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
        let dependencies = config.map(|value| dependency_snapshots(dir.path(), value).unwrap());
        check_with_probe_and_callees(&rule.spec, &units, &sources, ProbeOptions { root: dir.path(), checker, include_callees, include_error_origins, dependencies: dependencies.as_ref() })
    }
}
