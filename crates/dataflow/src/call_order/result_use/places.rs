mod allocations;

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::lower::site::{children, identifier};

use super::syntax::{descendants, initializers, transparent};
use super::Plan;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Place {
    Local(usize),
    Field(usize, usize),
    Capture(usize, usize),
    CallResult(usize, usize),
    Operand(usize, usize),
    CallComponent(usize, usize),
    Return,
}

pub(super) fn operand_slot(node: Node<'_>) -> Place {
    Place::Operand(node.start_byte(), node.end_byte())
}

pub(super) struct Places<'plan, 'src> {
    owners: HashMap<usize, Owner<'src>>,
    escaped_names: HashSet<&'src str>,
    source: &'src [u8],
    plan: &'plan super::super::identity::Syntax<'src>,
    rust_elements: &'plan super::rust_elements::RustElements,
}

struct Owner<'src> {
    definition: usize,
    fields: allocations::Members<'src>,
}

impl<'plan, 'src> Places<'plan, 'src> {
    pub(super) fn new(plan: &'plan Plan<'_, 'src>, function: Node<'_>, source: &'src [u8]) -> Self {
        let language = plan.spec.contract.as_ref().unwrap().language;
        let allocations = allocations::Allocations::new(function, source, language);
        let mut places = Self { owners: HashMap::new(), escaped_names: HashSet::new(), source, plan: &plan.syntax, rust_elements: &plan.rust_elements };
        for (target, value) in descendants(function).flat_map(|node| initializers(node, source)) {
            if let Some((binding, fields)) = places.binding(target, true).zip(allocations.members(value)) {
                if binding == target.start_byte() {
                    places.owners.insert(binding, Owner { definition: target.start_byte(), fields });
                }
            }
        }
        places.retain_stable_owners(function);
        places
    }

    pub(super) fn exclude_captures(&mut self, captures: HashSet<&'src str>) {
        self.escaped_names = captures;
    }

    pub(super) fn source(&self) -> &'src [u8] {
        self.source
    }

    pub(super) fn has_binding(&self, name: &str) -> bool {
        self.plan.taken_name(name)
    }

    pub(super) fn binding(&self, node: Node<'_>, defining: bool) -> Option<usize> {
        identifier(node).then(|| self.named(node.utf8_text(self.source).ok()?, node.start_byte(), defining)).flatten()
    }

    pub(super) fn named(&self, name: &str, at: usize, defining: bool) -> Option<usize> {
        (!self.escaped_names.contains(name)).then(|| self.plan.local_result_binding(name, at, defining)).flatten()
    }

    pub(super) fn place(&self, node: Node<'_>, defining: bool) -> Option<Place> {
        let node = transparent(node);
        if identifier(node) || (defining && node.kind() == "shorthand_field_identifier") {
            return self.binding(node, defining).map(Place::Local);
        }
        if !defining {
            if crate::lower::rust::is_labeled_block(node) { return Some(operand_slot(node)); }
            if let Some(element) = self.rust_elements.place(node) { return Some(element); }
        }
        let (object, member) = field(node)?;
        let binding = self.binding(object, false)?;
        let owner = self.owners.get(&binding)?;
        let member = member.utf8_text(self.source).ok()?;
        owner.fields.get(member).map(|key| Place::Field(binding, key))
    }

    pub(super) fn initialized_fields(&self, binding: usize) -> impl Iterator<Item = Place> + '_ {
        self.owners
            .get(&binding)
            .into_iter()
            .flat_map(|owner| owner.fields.keys())
            .map(move |key| Place::Field(binding, key))
    }

    fn retain_stable_owners(&mut self, function: Node<'_>) {
        let mut rejected = HashSet::new();
        for node in descendants(function).filter(|node| identifier(*node)) {
            let Some(owner) = self.binding(node, true).and_then(|binding| self.owners.get(&binding)) else { continue };
            if owner.definition != node.start_byte() && !static_member_use(node, &owner.fields, self.source) {
                rejected.insert(owner.definition);
            }
        }
        self.owners.retain(|binding, _| !rejected.contains(binding));
    }
}

fn static_member_use(node: Node<'_>, fields: &allocations::Members<'_>, source: &[u8]) -> bool {
    let Some(parent) = node.parent() else { return false };
    let Some((object, member)) = field(parent) else { return false };
    let known = member.utf8_text(source).ok().and_then(|name| fields.get(name)).is_some();
    object == node && known && !address_taken(parent)
}

fn address_taken(node: Node<'_>) -> bool {
    std::iter::successors(node.parent(), Node::parent)
        .find(|node| node.kind() != "parenthesized_expression")
        .is_some_and(|parent| {
            matches!(parent.kind(), "reference_expression" | "unary_expression")
                && parent.child(0).is_some_and(|op| matches!(op.kind(), "&" | "&raw"))
        })
}

fn field(node: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let (receiver, property) = match node.kind() {
        "selector_expression" => ("operand", "field"),
        "field_expression" => ("value", "field"),
        "field_access" => ("object", "field"),
        "member_expression" => ("object", "property"),
        "navigation_expression" if node.child(1).is_some_and(|op| op.kind() == ".") => {
            let mut parts = children(node);
            return Some((parts.next()?, parts.next().filter(|node| identifier(*node))?));
        }
        _ => return None,
    };
    let object = node.child_by_field_name(receiver)?;
    let member = node.child_by_field_name(property)?;
    matches!(member.kind(), "identifier" | "property_identifier" | "field_identifier").then_some((object, member))
}
