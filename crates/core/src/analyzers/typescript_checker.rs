//! Bounded interface to the optional TypeScript type-evidence prototype.
//! The compiler path is supplied explicitly by the caller and executes code;
//! it must point to a trusted TypeScript installation.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

const SCRIPT: &str = include_str!("typescript_promise_probe.cjs");
const MAX_BYTES: usize = 256 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeScriptRequest<'a> {
    pub typescript_module: &'a str,
    pub config: &'a str,
    pub calls: &'a [TypeScriptCall],
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TypeScriptCall {
    pub path: String,
    pub start: usize,
    pub source_sha256: String,
}

#[derive(Debug, Deserialize)]
pub struct TypeScriptResult {
    pub status: String,
    pub reason: Option<String>,
    pub calls: Option<Vec<TypeScriptCallResult>>,
}

#[derive(Debug, Deserialize)]
pub struct TypeScriptCallResult {
    pub path: String,
    pub start: usize,
    pub source_sha256: String,
    pub status: String,
}

/// Runs an explicitly supplied compiler with bounded input, output and time.
/// Any failure returns `None` and must be reported as incomplete analysis.
pub fn check(node: &str, request: &TypeScriptRequest<'_>, timeout: Duration) -> Option<TypeScriptResult> {
    let input = serde_json::to_vec(request).ok()?;
    let output = run_bounded(Command::new(node).args(["-e", SCRIPT]), input, timeout)?;
    let result: TypeScriptResult = serde_json::from_slice(&output).ok()?;
    valid_response(&result, request).then_some(result)
}

pub(super) fn run_bounded(command: &mut Command, input: Vec<u8>, timeout: Duration) -> Option<Vec<u8>> {
    if input.len() > MAX_BYTES { return None; }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let (written_tx, written_rx) = mpsc::sync_channel(1);
    let (output_tx, output_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || { let _ = written_tx.send(stdin.write_all(&input)); });
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.by_ref().take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).map(|_| bytes);
        let _ = output_tx.send(result);
    });
    let started = Instant::now();
    let status = wait_for_child(&mut child, started, timeout);
    let written = written_rx.recv_timeout(timeout.saturating_sub(started.elapsed())).ok()?.is_ok();
    let output = output_rx.recv_timeout(timeout.saturating_sub(started.elapsed())).ok()?.ok()?;
    if !written || !status?.success() || output.len() > MAX_BYTES { return None; }
    Some(output)
}

fn wait_for_child(child: &mut std::process::Child, started: Instant, timeout: Duration) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => { let _ = child.kill(); let _ = child.wait(); return None; }
        }
    }
}

fn valid_response(result: &TypeScriptResult, request: &TypeScriptRequest<'_>) -> bool {
    match result.status.as_str() {
        "incomplete" if result.reason.is_some() && result.calls.is_none() => true,
        "ok" if result.reason.is_none() && result.calls.as_ref().is_some_and(|calls| {
            calls.len() == request.calls.len() && calls.iter().zip(request.calls).all(|(actual, expected)| {
                actual.path == expected.path && actual.start == expected.start && actual.source_sha256 == expected.source_sha256
                    && matches!(actual.status.as_str(), "promise_like" | "not_promise" | "not_proven" | "unavailable" | "source_changed")
            })
        }) => true,
        _ => false,
    }
}

mod verification;
pub use verification::{verify_candidates, CheckerConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use sha2::{Digest, Sha256};
    use autoreview_dataflow::call_order::{FactorySourceDiscovery, KeyedLimitation, KeyedUnit};

    #[test]
    fn checker_proves_a_call_and_fails_closed() {
        let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("tsconfig.json");
        let file = dir.path().join("sample.ts");
        let source = "declare function f(): Promise<void>;\nf();\n";
        std::fs::write(&config, r#"{"compilerOptions":{"target":"ES2022","strict":true},"files":["sample.ts"]}"#).unwrap();
        std::fs::write(&file, source).unwrap();
                let calls = [TypeScriptCall { path: file.to_string_lossy().into_owned(), start: source.rfind("f()").unwrap(), source_sha256: format!("{:x}", Sha256::digest(source.as_bytes())) }];
        let config = config.to_string_lossy();
        let request = TypeScriptRequest { typescript_module: &compiler, config: &config, calls: &calls };
        let result = check("node", &request, Duration::from_secs(20)).unwrap();
        assert_eq!(result.status, "ok");
        assert_eq!(result.calls.unwrap()[0].status, "promise_like");
        std::fs::write(&file, format!("{source}\n")).unwrap();
        assert_eq!(check("node", &request, Duration::from_secs(20)).unwrap().calls.unwrap()[0].status, "source_changed");
        assert!(check("node", &request, Duration::ZERO).is_none());
        let absent = TypeScriptRequest { typescript_module: "missing-autoreview-typescript", ..request };
        assert_eq!(check("node", &absent, Duration::from_secs(20)).unwrap().reason.as_deref(), Some("checker_unavailable"));
        let noisy = dir.path().join("noisy.cjs");
        std::fs::write(&noisy, "process.stdout.write('x'.repeat(300000)); for (;;) {};").unwrap();
        let noisy_path = noisy.to_string_lossy();
        let noisy_request = TypeScriptRequest { typescript_module: &noisy_path, ..absent };
        let before = Instant::now();
        assert!(check("node", &noisy_request, Duration::from_millis(200)).is_none());
        assert!(before.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn checker_distinguishes_known_primitive_results_from_unknown_types() {
        let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("tsconfig.json");
        let file = dir.path().join("sample.ts");
        let source = "interface String { then: number }\ndeclare function text(): string;\ndeclare function number(): number;\ndeclare function uncertain(): unknown;\ntext(); number(); uncertain();\n";
        std::fs::write(&config, r#"{"compilerOptions":{"target":"ES2022","strict":true},"files":["sample.ts"]}"#).unwrap();
        std::fs::write(&file, source).unwrap();
        let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
        let calls: Vec<_> = ["text()", "number()", "uncertain()"].into_iter().map(|name| TypeScriptCall {
            path: file.to_string_lossy().into_owned(), start: source.rfind(name).unwrap(), source_sha256: digest.clone(),
        }).collect();
        let config = config.to_string_lossy();
        let request = TypeScriptRequest { typescript_module: &compiler, config: &config, calls: &calls };
        let result = check("node", &request, Duration::from_secs(20)).unwrap();
        let statuses: Vec<_> = result.calls.unwrap().into_iter().map(|call| call.status).collect();
        assert_eq!(statuses, ["not_proven", "not_promise", "not_proven"]);
    }

    #[test]
    fn verified_candidates_keep_unresolved_locations_and_reject_changed_sources() {
        let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("tsconfig.json");
        let file = dir.path().join("sample.ts");
        let source = "declare function pending(): Promise<void>;\ndeclare function sync(): string;\ndeclare function uncertain(): unknown;\npending();\nsync(); uncertain();\n";
        std::fs::write(&config, r#"{"compilerOptions":{"target":"ES2022","strict":true},"files":["sample.ts"]}"#).unwrap();
        std::fs::write(&file, source).unwrap();
        let units = [KeyedUnit { path: "sample.ts", source: source.as_bytes(), functions: &[], imports: &[], exports: &[], entry: true }];
        let pending = source.rfind("pending()").unwrap();
        let sync = source.rfind("sync()").unwrap();
        let uncertain = source.rfind("uncertain()").unwrap();
        let discovery = || FactorySourceDiscovery {
            candidates: BTreeMap::from([(0, BTreeSet::from([pending, sync, uncertain]))]),
            limitations: vec![
                KeyedLimitation { reason: "promise_type_unresolved", path: Some("sample.ts".into()), line: Some(4) },
                KeyedLimitation { reason: "promise_type_unresolved", path: Some("sample.ts".into()), line: Some(5) },
            ],
        };
        let config = config.to_str().unwrap();
        let options = CheckerConfig { node: "node", compiler: &compiler, config, root: dir.path(), timeout: Duration::from_secs(20) };
        let result = verify_candidates(&options, &units, discovery());
        assert_eq!(result.candidates[&0], BTreeSet::from([pending]));
        assert_eq!(result.limitations.len(), 1);
        assert_eq!(result.limitations[0].line, Some(5));
        std::fs::write(&file, format!("{source}\n")).unwrap();
        let changed = verify_candidates(&options, &units, discovery());
        assert!(changed.candidates.is_empty());
        assert_eq!(changed.limitations.len(), 2);
    }

    #[test]
    fn factory_sources_are_discovered_verified_and_checked_across_files() {
        use autoreview_dataflow::call_order::{check_keyed_units_with_verified_sources, discover_factory_sources, CallOrderSpec, ContractLanguage, ContractMode, KeyedExport, KeyedFunction, KeyedImport, ResultContract};
        use autoreview_dataflow::promise_types::TypeModule;
        use autoreview_langsupport::{parser_for, Language};
        let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
        let dir = tempfile::tempdir().unwrap();
        let paths = ["caller.ts", "barrel.ts", "binding.ts", "factory.ts"];
        let texts = [
            "import { upload } from './barrel'; export function run() { upload('x'); }",
            "export * from './binding';",
            "import create from './factory'; export const upload = create({});",
            "export default function create(x: object): (ctx: string) => Promise<void> { throw new Error(); }",
        ];
        for (path, source) in paths.iter().zip(texts) { std::fs::write(dir.path().join(path), source).unwrap(); }
        let config_path = dir.path().join("tsconfig.json");
        std::fs::write(&config_path, r#"{"compilerOptions":{"target":"ES2022","strict":true},"include":["*.ts"]}"#).unwrap();
        let trees: Vec<_> = texts.iter().map(|text| parser_for(Language::TypeScript).unwrap().parse(text, None).unwrap()).collect();
        let modules: Vec<_> = texts.iter().zip(&trees).zip(paths).map(|((text, tree), path)| TypeModule { path, source: text.as_bytes(), root: tree.root_node() }).collect();
        let function = trees[0].root_node().named_children(&mut trees[0].root_node().walk()).find_map(|node| node.child_by_field_name("declaration")).unwrap();
        let graph = autoreview_dataflow::lower::keyed_variant(Language::TypeScript, texts[0].as_bytes(), function);
        let functions = [KeyedFunction { node: function, graph: &graph }];
        let imports = [vec![KeyedImport { local: "upload".into(), path: "barrel.ts".into(), name: Some("upload".into()), owner: None, exported: true }], vec![], vec![KeyedImport { local: "create".into(), path: "factory.ts".into(), name: Some("default".into()), owner: None, exported: true }], vec![]];
        let exports = [vec![], vec![KeyedExport::Star { path: "binding.ts".into() }], vec![KeyedExport::Named { exported: "upload".into(), local: "upload".into(), path: None }], vec![KeyedExport::Named { exported: "default".into(), local: "create".into(), path: None }]];
        let units: Vec<_> = modules.iter().enumerate().map(|(i, module)| KeyedUnit { path: module.path, source: module.source, functions: if i == 0 { &functions } else { &[] }, imports: &imports[i], exports: &exports[i], entry: i == 0 }).collect();
        let spec = CallOrderSpec { rule_id: "typed-test".into(), after: vec![], unless: vec![], before: vec![], check_before_return: false, identity: None, contract: Some(ResultContract { mode: ContractMode::MustUse, language: ContractLanguage::Web, import: "unused".into(), methods: vec!["missing".into()], borrows: vec![], acquire_awaited: false, complete_observed: false, typed_promise_factory: false, typed_promise: false, typed_go_error: false }) };
        let config = config_path.to_str().unwrap();
        let candidates = discover_factory_sources(&spec, &modules, &units);
        assert_eq!(candidates.candidates[&0].len(), 1);
        let options = CheckerConfig { node: "node", compiler: &compiler, config, root: dir.path(), timeout: Duration::from_secs(20) };
        let verified = verify_candidates(&options, &units, candidates);
        assert!(verified.limitations.is_empty());
        let outcome = check_keyed_units_with_verified_sources(&spec, &units, verified);
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].trigger_call, "upload");
        assert!(outcome.limitations.is_empty(), "{:?}", outcome.limitations.iter().map(|issue| issue.reason).collect::<Vec<_>>());
        let candidates = discover_factory_sources(&spec, &modules, &units);
        let unavailable_options = CheckerConfig { compiler: "missing-autoreview-typescript", ..options };
        let unavailable = verify_candidates(&unavailable_options, &units, candidates);
        let outcome = check_keyed_units_with_verified_sources(&spec, &units, unavailable);
        assert!(outcome.hits.is_empty());
        assert!(outcome.limitations.iter().any(|item| item.reason == "promise_type_unresolved"));
    }
}
