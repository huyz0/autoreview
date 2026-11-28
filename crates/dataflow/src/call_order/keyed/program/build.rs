use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use crate::call_order::CallOrderSpec;

use super::kotlin::{direct_kotlin_scope, kotlin_parameter_list, kotlin_parameters};
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
            has_kotlin_defaults: false,
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
        let kotlin_list = spec.is_none().then(|| kotlin_parameter_list(node)).flatten();
        if let Some(list) = kotlin_list {
            self.index_kotlin_name(node, list, source, scope);
        }
        let (params, kotlin_defaults) = match kotlin_list {
            Some(list) => kotlin_parameters(list, source).map_or((None, Vec::new()), |(params, defaults)| (Some(params), defaults)),
            None => (parameters(node, source), Vec::new()),
        };
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
            direct_kotlin_calls: kotlin_list.is_some() && direct_kotlin_scope(node),
            kotlin_defaults,
            unsupported_rust_line: (rust_support == Some(false))
                .then_some(node.start_position().row as u32 + 1),
            rust_pattern_projections,
            params: params.unwrap_or_default(),
            export,
        });
    }
}

fn scope(node: Node<'_>, unit: usize) -> Scope {
    std::iter::successors(node.parent(), |parent| parent.parent())
        .find(|parent| matches!(parent.kind(), "class_body" | "interface_body" | "enum_body" | "impl_item"))
        .map_or(Scope::File(unit), |parent| {
            Scope::Class(unit, parent.start_byte())
        })
}
