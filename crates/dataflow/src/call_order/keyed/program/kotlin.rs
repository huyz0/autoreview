use tree_sitter::Node;

use crate::cfg::IdentityOperand;
use crate::lower::site::{children, identifier, operand};

use super::{CallSite, Program, Scope};

#[derive(Clone, Copy)]
pub(super) struct KotlinName {
    frame: Option<usize>,
    has_defaults: bool,
}

impl KotlinName {
    fn merge(self, other: Self) -> Self {
        Self {
            frame: self.frame.filter(|_| self.frame == other.frame),
            has_defaults: self.has_defaults || other.has_defaults,
        }
    }
}

pub(in crate::call_order::keyed) enum KotlinTarget {
    Positional,
    Declined,
    Bound(usize),
}

pub(in crate::call_order::keyed) enum DefaultArgument {
    Required,
    Literal,
    Alias(usize),
    Unsupported,
}

impl Program<'_, '_, '_> {
    pub(super) fn index_kotlin_name(&mut self, node: Node<'_>, list: Node<'_>, source: &[u8], scope: Scope) {
        if !node.parent().is_some_and(|parent| matches!(parent.kind(), "source_file" | "class_body")) {
            return;
        }
        let Some(name) = node.child_by_field_name("name").and_then(|name| name.utf8_text(source).ok()) else { return };
        // An unsupported signature can still compete with a modeled declaration.
        let has_defaults = list.children(&mut list.walk()).any(|child| child.kind() == "=");
        self.has_kotlin_defaults |= has_defaults;
        let entry = KotlinName { frame: Some(self.frames.len()), has_defaults };
        self.kotlin_names.entry(scope).or_default().entry(name.to_owned())
            .and_modify(|prior| *prior = prior.merge(entry)).or_insert(entry);
    }

    pub(in crate::call_order::keyed) fn kotlin_target(&self, from: usize, name: &str, site: &CallSite) -> KotlinTarget {
        let named = !site.named_arguments.is_empty();
        let fallback = if named { KotlinTarget::Declined } else { KotlinTarget::Positional };
        if (!named && !self.has_kotlin_defaults) || !site.local || name.contains(['.', ':']) {
            return fallback;
        }
        let Some(binding) = self.kotlin_binding(from, name) else { return fallback };
        if !named && !binding.has_defaults {
            return KotlinTarget::Positional;
        }
        let Some(target) = binding.frame else { return KotlinTarget::Declined };
        let caller = &self.frames[from];
        let callee = &self.frames[target];
        if caller.named_kotlin_calls && callee.named_kotlin_calls
            && callee.export.as_ref().is_some_and(|export| caller.unit == callee.unit || export.visible)
            && self.unshadowed_root(from, name, site).is_some() {
            KotlinTarget::Bound(target)
        } else {
            KotlinTarget::Declined
        }
    }

    fn kotlin_binding(&self, from: usize, name: &str) -> Option<KotlinName> {
        let frame = &self.frames[from];
        let lookup = |scope, name: &str| self.kotlin_names.get(&scope)?.get(name).copied();
        let local = [frame.scope, Scope::File(frame.unit)].into_iter().filter_map(|scope| lookup(scope, name));
        let imported = self.units[frame.unit].imports.iter().filter_map(|binding| {
            if binding.owner.is_some() { return None; }
            let name = binding.target(name)?;
            let unit = *self.paths.get(binding.path.as_str())?;
            lookup(Scope::File(unit), name)
        });
        local.chain(imported).reduce(KotlinName::merge)
    }
}

pub(super) fn kotlin_parameter_list(node: Node<'_>) -> Option<Node<'_>> {
    (node.kind() == "function_declaration").then(|| {
        children(node).find(|child| child.kind() == "function_value_parameters")
    }).flatten()
}

pub(super) fn kotlin_parameters(list: Node<'_>, source: &[u8]) -> Option<(Vec<String>, Vec<DefaultArgument>)> {
    if list.has_error() {
        return None;
    }
    let mut cursor = list.walk();
    let mut parts = list.children(&mut cursor).filter(|part| !matches!(part.kind(),
        "(" | ")" | "," | "comment" | "line_comment" | "block_comment")).peekable();
    let mut params = Vec::new();
    let mut defaults = Vec::new();
    while let Some(parameter) = parts.next() {
        if parameter.kind() != "parameter" {
            return None;
        }
        let name = children(parameter).find(|child| identifier(*child))?.utf8_text(source).ok()?;
        let default = if parts.next_if(|part| part.kind() == "=").is_some() {
            default_argument(parts.next()?, source, &params)
        } else {
            DefaultArgument::Required
        };
        params.push(name.to_owned());
        defaults.push(default);
    }
    Some((params, defaults))
}

fn default_argument(value: Node<'_>, source: &[u8], earlier: &[String]) -> DefaultArgument {
    match operand(value, source) {
        Some(IdentityOperand::Binding(name)) if name == "null" => DefaultArgument::Literal,
        Some(IdentityOperand::Binding(name)) => earlier.iter().position(|parameter| *parameter == name)
            .map_or(DefaultArgument::Unsupported, DefaultArgument::Alias),
        Some(IdentityOperand::String(_) | IdentityOperand::Integer(_) | IdentityOperand::Boolean(_)) => DefaultArgument::Literal,
        _ => DefaultArgument::Unsupported,
    }
}

pub(super) fn named_kotlin_scope(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else { return false };
    let Some(name) = node.child_by_field_name("name") else { return false };
    if children(node).take_while(|child| *child != name)
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
