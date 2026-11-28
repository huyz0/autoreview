use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::lower::rust::element_access;
use crate::lower::site::identifier;

use super::super::identity::Syntax;
use super::places::operand_slot;
use super::references::{capture_scope, reference, string_references};
use super::syntax::{descendants, transparent};
use super::{ContractLanguage, Place};

#[derive(Default)]
pub(in crate::call_order) struct RustElements {
    aggregates: HashSet<usize>,
    reads: HashMap<(usize, usize), Place>,
}

impl RustElements {
    pub(in crate::call_order) fn scan(function: Node<'_>, source: &[u8], syntax: &Syntax<'_>) -> Self {
        let Some(body) = function.child_by_field_name("body") else { return Self::default() };
        let mut projections = Self::default();
        let mut locals = HashMap::new();
        for node in descendants(body)
            .filter(|node| matches!(node.kind(), "array_expression" | "tuple_expression") && in_body(*node, body))
        {
            if let Some((access, slot)) = element_access::direct_projection(node, source) {
                projections.record(node, access, slot);
            }
            if let Some(binding) = element_access::local_aggregate(node) {
                locals.insert(binding.start_byte(), node);
            }
        }
        if locals.is_empty() { return projections; }

        let mut pending = Vec::new();
        let mut rejected = HashSet::new();
        let binding = |name, start| syntax.local_result_binding(name, start, false);
        for node in descendants(body) {
            string_references(node, source, ContractLanguage::Rust, |name| {
                if let Some(binding) = binding(name, node.start_byte()) {
                    rejected.insert(binding);
                }
            });
            if !reference(node, source) { continue; }
            let Ok(name) = node.utf8_text(source) else { continue };
            if syntax.local_result_binding(name, node.start_byte(), true) == Some(node.start_byte()) { continue; }
            let Some(id) = binding(name, node.start_byte()) else { continue };
            let Some(&aggregate) = locals.get(&id) else { continue };
            if let Some((access, slot)) = local_projection(node, aggregate, body, source) {
                pending.push((id, aggregate, access, slot));
            } else {
                rejected.insert(id);
            }
        }
        for (binding, aggregate, access, slot) in pending {
            if !rejected.contains(&binding) {
                // A read can use the initializer's slot only while the binding has no other uses.
                projections.record(aggregate, access, slot);
            }
        }
        projections
    }

    pub(in crate::call_order) fn tracks_aggregate(&self, start: usize) -> bool {
        self.aggregates.contains(&start)
    }

    pub(super) fn place(&self, node: Node<'_>) -> Option<Place> {
        self.reads.get(&(node.start_byte(), node.end_byte())).copied()
    }

    fn record(&mut self, aggregate: Node<'_>, access: Node<'_>, slot: Node<'_>) {
        self.aggregates.insert(aggregate.start_byte());
        self.reads.insert((access.start_byte(), access.end_byte()), operand_slot(slot));
    }
}

fn local_projection<'tree>(node: Node<'tree>, aggregate: Node<'tree>, body: Node<'_>, source: &[u8]) -> Option<(Node<'tree>, Node<'tree>)> {
    if !identifier(node) || !in_body(node, body) { return None; }
    let access = std::iter::successors(node.parent(), Node::parent)
        .find(|parent| transparent(*parent) != node)?;
    let (base, index) = element_access::projection(access, source)?;
    if base != node || !element_access::read_only(access) { return None; }
    Some((access, element_access::element_slot(aggregate, access, index, source)?))
}

fn in_body(node: Node<'_>, body: Node<'_>) -> bool {
    std::iter::successors(Some(node), Node::parent)
        .take_while(|node| *node != body)
        .all(|node| !capture_scope(node) && !matches!(node.kind(), "function_item" | "const_block" | "macro_invocation"))
}
