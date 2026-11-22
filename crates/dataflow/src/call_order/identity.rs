use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::cfg::{CallSite, IdentityOperand};
use crate::lower::site::{children, identifier, name};
use crate::taint::NamePattern;

use super::{CollectionSelector, EntryIdentity};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Entry {
    pub collection: String,
    pub key: IdentityOperand,
}

impl Entry {
    pub(super) fn display_key(&self) -> String {
        match &self.key {
            IdentityOperand::Binding(name) | IdentityOperand::Integer(name) => name.clone(),
            IdentityOperand::String(value) => format!("{value:?}"),
        }
    }
}

/// The CFG stores the operand spellings. Syntax guards against equating
/// different values of a rebound name or assuming a cleanup was lowered.
#[derive(Default)]
pub(super) struct Syntax<'src> {
    declarations: HashMap<&'src str, usize>,
    writes: HashSet<&'src str>,
    calls: Vec<(usize, String)>,
}

impl<'src> Syntax<'src> {
    pub(super) fn scan(function: Node<'_>, source: &'src [u8]) -> Option<Self> {
        let mut syntax = Self::default();
        let mut nodes = vec![function];
        while let Some(node) = nodes.pop() {
            if unsupported_flow(node) {
                return None;
            }
            syntax.visit(node, source);
            nodes.extend(children(node));
        }
        Some(syntax)
    }

    pub(super) fn models_cleanup(&self, cleanup: &[NamePattern], modeled: &HashSet<usize>) -> bool {
        self.calls.iter().all(|(start, name)| {
            modeled.contains(start) || !cleanup.iter().any(|pattern| pattern.matches(name))
        })
    }

    pub(super) fn entry(&self, selector: EntryIdentity, site: &CallSite) -> Option<Entry> {
        let collection = match selector.collection {
            CollectionSelector::Receiver => site.receiver.as_ref()?,
            CollectionSelector::Argument(index) => match site.arguments.get(index)?.as_ref()? {
                IdentityOperand::Binding(name) => name,
                _ => return None,
            },
        };
        let key = site.arguments.get(selector.key_argument)?.as_ref()?;
        if !self.stable(collection)
            || matches!(key, IdentityOperand::Binding(name) if !self.stable(name))
        {
            return None;
        }
        Some(Entry {
            collection: collection.clone(),
            key: key.clone(),
        })
    }

    fn stable(&self, name: &str) -> bool {
        self.declarations.get(name) == Some(&1) && !self.writes.contains(name)
    }

    fn visit(&mut self, node: Node<'_>, source: &'src [u8]) {
        if matches!(node.kind(), "method_invocation" | "call_expression") {
            if let Some(name) = name(node, source) {
                self.calls.push((node.start_byte(), name));
            }
        }
        if let Some(target) = declaration(node) {
            for name in names_in(target, source) {
                *self.declarations.entry(name).or_default() += 1;
            }
            // A direct copy can alias the original binding.
            if let Some(value) = initializer(node).filter(|node| identifier(*node)) {
                self.writes.extend(names_in(value, source));
                self.writes.extend(names_in(target, source));
            }
        }
        if let Some(target) = write_target(node, source) {
            self.writes.extend(names_in(target, source));
        }
    }
}

fn declaration(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "variable_declarator" | "formal_parameter" | "catch_formal_parameter" | "var_spec" => {
            node.child_by_field_name("name")
        }
        "parameter_declaration" | "variadic_parameter_declaration" => {
            node.child_by_field_name("name")
        }
        "let_declaration" | "required_parameter" | "optional_parameter" => {
            node.child_by_field_name("pattern")
        }
        "short_var_declaration" => node.child_by_field_name("left"),
        "parameter" | "variable_declaration" => node
            .child_by_field_name("pattern")
            .or_else(|| children(node).find(|node| identifier(*node))),
        _ if identifier(node)
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "formal_parameters") =>
        {
            Some(node)
        }
        _ => None,
    }
}

fn initializer(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "short_var_declaration" => node.child_by_field_name("right")?.named_child(0),
        "variable_declaration"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "property_declaration") =>
        {
            children(node.parent()?)
                .last()
                .filter(|value| *value != node)
        }
        "variable_declarator" | "let_declaration" | "var_spec" => node.child_by_field_name("value"),
        _ => None,
    }
}

fn write_target<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    match node.kind() {
        "assignment"
        | "assignment_expression"
        | "augmented_assignment_expression"
        | "compound_assignment_expr"
        | "assignment_statement"
        | "range_clause" => node
            .child_by_field_name("left")
            .or_else(|| node.named_child(0)),
        "update_expression" | "inc_statement" | "dec_statement" | "enhanced_for_statement" => node
            .child_by_field_name("name")
            .or_else(|| node.named_child(0)),
        "unary_expression" if node.utf8_text(source).ok()?.starts_with('&') => {
            node.child_by_field_name("operand")
        }
        "reference_expression"
            if children(node).any(|child| child.kind() == "mutable_specifier") =>
        {
            node.child_by_field_name("value")
        }
        "prefix_expression" | "postfix_expression"
            if ["++", "--"].iter().any(|op| {
                node.utf8_text(source)
                    .is_ok_and(|text| text.starts_with(op) || text.ends_with(op))
            }) =>
        {
            node.named_child(0)
        }
        _ => None,
    }
}

fn names_in<'src>(node: Node<'_>, source: &'src [u8]) -> Vec<&'src str> {
    let mut names = Vec::new();
    let mut nodes = vec![node];
    while let Some(node) = nodes.pop() {
        if identifier(node)
            || matches!(
                node.kind(),
                "shorthand_property_identifier_pattern" | "shorthand_field_identifier"
            )
        {
            names.extend(node.utf8_text(source).ok());
        } else {
            nodes.extend(children(node));
        }
    }
    names
}

fn unsupported_flow(node: Node<'_>) -> bool {
    // Rust's postfix `?` and Kotlin's `try` share a node kind.
    if node.kind() == "try_expression" {
        return node.child(0).is_some_and(|first| first.kind() == "try");
    }
    matches!(
        node.kind(),
        "try_statement"
            | "try_with_resources_statement"
            | "throw_statement"
            | "throw_expression"
            | "switch_statement"
            | "switch_expression"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "select_statement"
            | "when_expression"
            | "match_expression"
            | "goto_statement"
            | "defer_statement"
            | "go_statement"
            | "for_statement"
            | "for_in_statement"
            | "enhanced_for_statement"
            | "for_expression"
            | "while_statement"
            | "while_expression"
            | "do_statement"
            | "do_while_statement"
            | "loop_expression"
    )
}
