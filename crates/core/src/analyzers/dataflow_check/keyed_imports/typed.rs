use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use autoreview_dataflow::call_order::{check_keyed_units_with_coverage, check_keyed_units_with_verified_sources, discover_factory_sources, FactorySourceDiscovery, KeyedCheckResult, KeyedLimitation, KeyedUnit};
use autoreview_dataflow::promise_types::TypeModule;
use super::Source;

pub(super) struct TrustedTypeScript {
    pub(super) node: String,
    pub(super) compiler: String,
    pub(super) config: String,
}

impl TrustedTypeScript {
    pub(super) fn from_environment(root: &Path) -> Option<Self> {
        let compiler = std::env::var("AUTOREVIEW_TYPESCRIPT_MODULE").ok()?;
        if !Path::new(&compiler).is_absolute() { return None; }
        let config = std::env::var("AUTOREVIEW_TYPESCRIPT_CONFIG").ok().map(PathBuf::from).unwrap_or_else(|| root.join("tsconfig.json"));
        if !config.is_absolute() { return None; }
        Some(Self { node: std::env::var("AUTOREVIEW_NODE_BINARY").unwrap_or_else(|_| "node".into()), compiler, config: config.to_str()?.to_owned() })
    }
}

pub(super) fn check_typed_rule(
    spec: &autoreview_dataflow::call_order::CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<String, Source>, root: &Path, checker: Option<&TrustedTypeScript>,
) -> KeyedCheckResult {
    let Some(checker) = checker else { return check_keyed_units_with_coverage(spec, units); };
    let modules: Vec<_> = units.iter().filter_map(|unit| sources.get(unit.path).map(|file| TypeModule { path: unit.path, source: file.parsed.0.as_bytes(), root: file.parsed.1.root_node() })).collect();
    if modules.len() != units.len() || modules.iter().zip(units).any(|(module, unit)| module.path != unit.path || module.source != unit.source) {
        return KeyedCheckResult { hits: Vec::new(), limitations: vec![KeyedLimitation { reason: "factory_module_graph_incomplete", path: None, line: None }] };
    }
    let candidates = if spec.contract.as_ref().is_some_and(|contract| contract.typed_promise) {
        discover_calls(&modules)
    } else { discover_factory_sources(spec, &modules, units) };
    let options = crate::analyzers::typescript_checker::CheckerConfig { node: &checker.node, compiler: &checker.compiler, config: &checker.config, root, timeout: std::time::Duration::from_secs(30) };
    let verified = crate::analyzers::typescript_checker::verify_candidates(&options, units, candidates);
    check_keyed_units_with_verified_sources(spec, units, verified)
}

fn discover_calls(modules: &[TypeModule<'_>]) -> FactorySourceDiscovery {
    let mut candidates = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut limitations = Vec::new();
    for (index, module) in modules.iter().enumerate() {
        let mut pending = vec![module.root];
        while let Some(node) = pending.pop() {
            if node.kind() == "call_expression" {
                candidates.entry(index).or_default().insert(node.start_byte());
                limitations.push(KeyedLimitation { reason: "promise_type_unresolved", path: Some(module.path.to_owned()), line: Some(node.start_position().row as u32 + 1) });
            }
            pending.extend(node.named_children(&mut node.walk()));
        }
    }
    FactorySourceDiscovery { candidates, limitations }
}

