use std::collections::{BTreeMap, BTreeSet};
use super::{CallOrderSpec, KeyedUnit, Program};
use super::super::keyed::KeyedLimitation;

/// Candidate call sites and limitations discovered while inspecting them.
pub struct FactorySourceDiscovery {
    pub candidates: BTreeMap<usize, BTreeSet<usize>>,
    pub limitations: Vec<KeyedLimitation>,
}

/// Discover direct imported calls with factory-return annotation evidence.
/// This returns syntactic candidates only: callers must independently prove
/// that `Promise` denotes the intended project type before treating them as
/// result-use sources. Nested scopes are scanned from their outermost frame so
/// any local shadowing in that frame conservatively excludes the name.
pub fn discover_factory_sources(
    spec: &CallOrderSpec,
    modules: &[crate::promise_types::TypeModule<'_>],
    units: &[KeyedUnit<'_, '_, '_>],
) -> FactorySourceDiscovery {
    if modules.len() != units.len() || modules.iter().zip(units).any(|(module, unit)| module.path != unit.path || module.source != unit.source) {
        return FactorySourceDiscovery {
            candidates: BTreeMap::new(),
            limitations: vec![KeyedLimitation { reason: "factory_module_graph_incomplete", path: None, line: None }],
        };
    }
    let program = Program::new(spec, units);
    let functions: Vec<_> = units.iter().flat_map(|unit| unit.functions.iter()).collect();
    let mut pending: Vec<_> = program.frames.iter().enumerate().filter_map(|(index, frame)| units[frame.unit].entry.then_some(index)).collect();
    let mut reachable = BTreeSet::new();
    while let Some(index) = pending.pop() {
        if !reachable.insert(index) { continue; }
        let frame = &program.frames[index];
        let parent = functions[index].node;
        pending.extend(functions.iter().enumerate().filter_map(|(other, function)| {
            (other != index && program.frames[other].unit == frame.unit
                && function.node.start_byte() >= parent.start_byte() && function.node.end_byte() <= parent.end_byte())
                .then_some(other)
        }));
        if let Some(plan) = &frame.plan {
            pending.extend(program.resolved_calls(index, frame.graph, plan).into_iter().map(|(_, target)| target));
        }
    }
    let mut result = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut limitations = Vec::new();
    for (index, module) in modules.iter().enumerate() {
        if crate::promise_types::module_bindings(module, index).is_none() {
            limitations.push(KeyedLimitation { reason: "factory_binding_inventory_incomplete", path: Some(module.path.to_owned()), line: None });
        }
    }
    let mut cache = BTreeMap::<(usize, String, usize), bool>::new();
    for (index, (function, frame)) in functions.iter().zip(&program.frames).enumerate() {
        if !reachable.contains(&index) { continue; }
        let Some(plan) = &frame.plan else { continue };
        let unit = &units[frame.unit];
        let mut pending = vec![function.node];
        while let Some(node) = pending.pop() {
            if let Some((name, arity)) = direct_call(node, unit.source) {
                if !plan.syntax.shadows_api(name) && !enclosing_scope_shadows(function.node, unit.source, name)
                    && unit.imports.iter().any(|binding| binding.local == name && binding.name.is_some() && binding.owner.is_none() && binding.exported) {
                    let valid = *cache.entry((frame.unit, name.to_owned(), arity)).or_insert_with(|| {
                        crate::promise_types::imported_factory_evidence(modules, units, frame.unit, name, arity).is_some()
                    });
                    if valid && result.entry(frame.unit).or_default().insert(node.start_byte()) {
                        limitations.push(KeyedLimitation {
                            reason: "promise_type_unresolved",
                            path: Some(unit.path.to_owned()),
                            line: Some(node.start_position().row as u32 + 1),
                        });
                    }
                }
            }
            pending.extend(node.named_children(&mut node.walk()));
        }
    }
    FactorySourceDiscovery { candidates: result, limitations }
}

fn enclosing_scope_shadows(function: tree_sitter::Node<'_>, source: &[u8], name: &str) -> bool {
    std::iter::successors(function.parent(), |node| node.parent())
        .filter(|node| matches!(node.kind(), "function_declaration" | "function_expression" | "arrow_function" | "method_definition"))
        .any(|node| super::super::identity::Syntax::may_shadow(node, source, name))
}

fn direct_call<'a>(node: tree_sitter::Node<'_>, source: &'a [u8]) -> Option<(&'a str, usize)> {
    if node.kind() != "call_expression" || node.children(&mut node.walk()).any(|child| matches!(child.kind(), "optional_chain" | "?.")) { return None; }
    let name = node.child_by_field_name("function").filter(|node| node.kind() == "identifier")?.utf8_text(source).ok()?;
    let arguments = node.child_by_field_name("arguments")?;
    let args: Vec<_> = arguments.named_children(&mut arguments.walk()).filter(|arg| arg.kind() != "comment").collect();
    if args.iter().any(|arg| matches!(arg.kind(), "spread_element" | "spread_expression")) { return None; }
    Some((name, args.len()))
}

