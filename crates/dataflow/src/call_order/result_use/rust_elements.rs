use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::lower::rust::element_access;
use crate::lower::site::identifier;

use super::super::identity::Syntax;
use super::places::operand_slot;
use super::references::{capture_scope, reference, string_references};
use super::syntax::descendants;
use super::{ContractLanguage, Place};

mod indices;
use indices::Indices;

#[derive(Default)]
pub(in crate::call_order) struct RustElements {
    aggregates: HashSet<usize>,
    reads: HashMap<(usize, usize), Place>,
}

impl RustElements {
    pub(in crate::call_order) fn scan(function: Node<'_>, source: &[u8], syntax: &Syntax<'_>) -> Self {
        let Some(body) = function.child_by_field_name("body") else { return Self::default() };
        let mut projections = Self::default();
        let indices = Indices::scan(body, source, syntax);
        let locals = projections.scan_literals(body, source, &indices);
        if !locals.is_empty() {
            projections.scan_local_reads(body, source, syntax, &locals, &indices);
        }
        projections
    }

    fn scan_literals<'tree>(&mut self, body: Node<'tree>, source: &[u8], indices: &Indices<'_, '_>) -> HashMap<usize, Node<'tree>> {
        let mut locals = HashMap::new();
        for node in descendants(body)
            .filter(|node| matches!(node.kind(), "array_expression" | "tuple_expression") && in_body(*node, body))
        {
            if let Some((access, slot)) = element_access::direct_projection(node, source, &|index| indices.value(index)) {
                self.record(node, access, slot, source);
            }
            if let Some(binding) = element_access::local_aggregate(node) {
                locals.insert(binding.start_byte(), node);
            }
        }
        locals
    }

    fn scan_local_reads(&mut self, body: Node<'_>, source: &[u8], syntax: &Syntax<'_>, locals: &HashMap<usize, Node<'_>>, indices: &Indices<'_, '_>) {
        let mut pending = Vec::new();
        let mut rejected = HashSet::new();
        for node in descendants(body) {
            string_references(node, source, ContractLanguage::Rust, |name| {
                if let Some(binding) = syntax.local_result_binding(name, node.start_byte(), false) {
                    rejected.insert(binding);
                }
            });
            let Some(id) = referenced_binding(syntax, node, source) else { continue };
            let Some(&aggregate) = locals.get(&id) else { continue };
            if let Some((access, slot)) = local_projection(node, aggregate, body, source, indices) {
                pending.push((id, aggregate, access, slot));
            } else {
                rejected.insert(id);
            }
        }
        // A read can use the initializer's slot only while the binding has no other uses.
        for (_, aggregate, access, slot) in pending.into_iter().filter(|(id, ..)| !rejected.contains(id)) {
            self.record(aggregate, access, slot, source);
        }
    }

    pub(in crate::call_order) fn tracks_aggregate(&self, start: usize) -> bool {
        self.aggregates.contains(&start)
    }

    pub(super) fn place(&self, node: Node<'_>) -> Option<Place> {
        self.reads.get(&(node.start_byte(), node.end_byte())).copied()
    }

    fn record(&mut self, aggregate: Node<'_>, access: Node<'_>, slot: Node<'_>, source: &[u8]) {
        if !self.tracks_aggregate(aggregate.start_byte()) {
            self.aggregates.extend(element_access::nested_aggregates(aggregate, source).map(|node| node.start_byte()));
        }
        self.reads.insert((access.start_byte(), access.end_byte()), operand_slot(slot));
    }
}

fn referenced_binding(syntax: &Syntax<'_>, node: Node<'_>, source: &[u8]) -> Option<usize> {
    if !reference(node, source) { return None; }
    let name = node.utf8_text(source).ok()?;
    let start = node.start_byte();
    if syntax.local_result_binding(name, start, true) == Some(start) { return None; }
    syntax.local_result_binding(name, start, false)
}

fn local_projection<'tree>(node: Node<'tree>, aggregate: Node<'tree>, body: Node<'_>, source: &[u8], indices: &Indices<'_, '_>) -> Option<(Node<'tree>, Node<'tree>)> {
    if !identifier(node) || !in_body(node, body) { return None; }
    element_access::projection_chain(node, aggregate, source, &|index| indices.value(index))
}

fn in_body(node: Node<'_>, body: Node<'_>) -> bool {
    std::iter::successors(Some(node), Node::parent)
        .take_while(|node| *node != body)
        .all(|node| !capture_scope(node) && !matches!(node.kind(), "function_item" | "const_item" | "static_item" | "const_block" | "macro_invocation"))
}
