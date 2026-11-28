use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use crate::call_order::CallOrderSpec;
use crate::lower::site::children;

use super::signature::{candidate, export, parameters};
use super::{Associated, Frame, KeyedFunction, KeyedUnit, Plan, Program, Scope, Unit};

impl<'spec, 'src, 'graph> Program<'spec, 'src, 'graph> {
    pub(in crate::call_order) fn new(spec: &'spec CallOrderSpec, inputs: &[KeyedUnit<'src, '_, 'graph>]) -> Self {
        Self::build(Some(spec), inputs)
    }

    pub(in crate::call_order::keyed) fn for_resolution(inputs: &[KeyedUnit<'src, '_, 'graph>]) -> Self {
        Self::build(None, inputs)
    }

    fn build(spec: Option<&'spec CallOrderSpec>, inputs: &[KeyedUnit<'src, '_, 'graph>]) -> Self {
        let mut program = Self {
            frames: Vec::new(),
            targets: BTreeMap::new(),
            kotlin_names: BTreeMap::new(),
            associated: Associated::default(),
            paths: BTreeMap::new(),
            unresolved: RefCell::new(BTreeSet::new()),
            units: Vec::new(),
        };
        for (unit, input) in inputs.iter().enumerate() {
            program.associated.index_types(unit, input);
            program.paths.insert(input.path, unit);
            program.units.push(Unit {
                path: input.path,
                source: input.source,
                imports: input.imports,
                exports: input.exports,
                entry: input.entry,
                frames: Vec::new(),
            });
            for function in input.functions {
                program.add_frame(spec, unit, input.source, function);
            }
        }
        program
    }

    fn add_frame(
        &mut self,
        spec: Option<&'spec CallOrderSpec>,
        unit: usize,
        source: &'src [u8],
        function: &KeyedFunction<'_, 'graph>,
    ) {
        let node = function.node;
        let scope = scope(node, unit);
        if spec.is_none() {
            self.index_kotlin_name(node, source, scope);
        }
        let params = parameters(node, source);
        let export = params.as_ref().and_then(|params| {
            let name = candidate(node, source)?;
            self.targets
                .entry((scope, name.clone(), params.len()))
                .and_modify(|target| *target = None)
                .or_insert(Some(self.frames.len()));
            self.associated.register(unit, (node, source), &name, params.len(), self.frames.len());
            Some(export(node, source, name))
        });
        self.units[unit].frames.push(self.frames.len());
        let plan = spec.and_then(|spec| Plan::new(spec, function.graph, node, source, unit));
        let declined = spec.filter(|_| plan.is_none());
        let declined_cleanup = declined.and_then(|spec| Plan::declined_cleanup(spec, function.graph, node, source, unit));
        let declined_contract = declined.and_then(|spec| Plan::declined_contract(spec, function.graph, node, source, unit));
        let resolution_syntax = spec.is_none().then(|| Plan::resolution_syntax(function.graph, node, source, unit)).flatten().map(Box::new);
        let rust_support = (node.kind() == "function_item")
            .then(|| crate::lower::rust::supports_function_with_source(node, source));
        let rust_pattern_projections = if rust_support == Some(true) {
            crate::lower::rust::pattern_projection_sites(node, source, |aggregate| {
                spec.is_some_and(CallOrderSpec::tracks_result_use)
                    && !plan.as_ref().is_some_and(|plan| plan.rust_elements.tracks_aggregate(aggregate.start_byte()))
            })
        } else {
            Vec::new()
        };
        self.frames.push(Frame {
            graph: function.graph,
            plan,
            resolution_syntax,
            declined_cleanup,
            declined_contract,
            unit,
            scope,
            named_kotlin_calls: spec.is_none() && named_kotlin_scope(node),
            unsupported_rust_line: (rust_support == Some(false))
                .then_some(node.start_position().row as u32 + 1),
            rust_pattern_projections,
            params: params.unwrap_or_default(),
            export,
        });
    }

    fn index_kotlin_name(&mut self, node: Node<'_>, source: &[u8], scope: Scope) {
        if node.kind() != "function_declaration"
            || !node.parent().is_some_and(|parent| matches!(parent.kind(), "source_file" | "class_body"))
            || !children(node).any(|child| child.kind() == "function_value_parameters") {
            return;
        }
        if let Some(name) = node.child_by_field_name("name").and_then(|name| name.utf8_text(source).ok()) {
            // Count declarations with defaults or varargs even if their body is not analyzable.
            self.kotlin_names.entry(scope).or_default().entry(name.to_owned())
                .and_modify(|candidate| *candidate = None).or_insert(Some(self.frames.len()));
        }
    }
}

fn named_kotlin_scope(node: Node<'_>) -> bool {
    let Some(parent) = node.parent().filter(|_| node.kind() == "function_declaration") else { return false };
    let Some(name) = node.child_by_field_name("name") else { return false };
    if !children(node).any(|child| child.kind() == "function_value_parameters")
        || children(node).take_while(|child| *child != name)
            .any(|child| !matches!(child.kind(), "modifiers" | "type_parameters")) {
        return false;
    }
    // Implicit receivers can bind a bare name before Kotlin considers imports.
    match parent.kind() {
        "source_file" => true,
        "class_body" => parent.parent().is_some_and(|owner| {
            owner.parent().is_some_and(|outer| outer.kind() == "source_file")
                && !children(owner).any(|child| child.kind() == "delegation_specifiers")
                && !children(parent).any(|child| child.kind() == "companion_object")
        }),
        _ => false,
    }
}

fn scope(node: Node<'_>, unit: usize) -> Scope {
    std::iter::successors(node.parent(), |parent| parent.parent())
        .find(|parent| matches!(parent.kind(), "class_body" | "interface_body" | "enum_body" | "impl_item"))
        .map_or(Scope::File(unit), |parent| {
            Scope::Class(unit, parent.start_byte())
        })
}
