use std::collections::{BTreeMap, HashMap, HashSet};

use tree_sitter::Node;

use crate::cfg::{CallSite, IdentityOperand};
use crate::lower::{
    keyed_flow,
    site::{children, name},
};
use crate::taint::NamePattern;

use super::bindings::Bindings;
use super::{CollectionSelector, EntryIdentity};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Entry {
    pub collection: IdentityOperand,
    pub key: IdentityOperand,
}

impl Entry {
    pub(super) fn display_collection(&self) -> String {
        binding_name(&self.collection)
            .unwrap_or_default()
            .to_owned()
    }

    pub(super) fn display_key(&self) -> String {
        match &self.key {
            IdentityOperand::Binding(name)
            | IdentityOperand::Integer(name)
            | IdentityOperand::Fresh { name, .. }
            | IdentityOperand::Scoped { name, .. } => name.clone(),
            IdentityOperand::String(value) => format!("{value:?}"),
            IdentityOperand::Boolean(value) => value.to_string(),
            IdentityOperand::CallResult { .. } => String::new(),
        }
    }
}

pub(super) type Values = BTreeMap<String, Option<IdentityOperand>>;

/// Syntax tracks declaration scope and guard names. Values are read from
/// the current path, with lexical resolution for untouched bindings.
#[derive(Default)]
pub(super) struct Syntax<'src> {
    bindings: Bindings<'src>,
    calls: Vec<(usize, String)>,
    guards: HashMap<String, usize>,
    owner: usize,
    pub(super) unit: usize,
}

impl<'src> Syntax<'src> {
    pub(super) fn scan(
        function: Node<'_>,
        source: &'src [u8],
        writes: &HashSet<usize>,
        unit: usize,
    ) -> Option<Self> {
        let mut syntax = Self {
            owner: function.start_byte(),
            unit,
            ..Self::default()
        };
        let mut nodes = vec![function];
        while let Some(node) = nodes.pop() {
            if unsupported_flow(node, source) {
                return None;
            }
            if matches!(node.kind(), "method_invocation" | "call_expression") {
                if let Some(name) = name(node, source) {
                    syntax.calls.push((node.start_byte(), name));
                }
            }
            syntax
                .bindings
                .visit(node, source, writes.contains(&node.start_byte()));
            if matches!(node.kind(), "if_statement" | "if_expression") {
                if let Some((name, _)) = crate::lower::keyed_syntax::condition(node, source) {
                    *syntax.guards.entry(name).or_default() += 1;
                }
            }
            nodes.extend(children(node));
        }
        syntax.bindings.record_borrowed_writes();
        Some(syntax)
    }

    pub(super) fn resolve(
        &self,
        values: &Values,
        name: &str,
        start: usize,
    ) -> Option<IdentityOperand> {
        if !self.bindings.can_read(name) {
            return None;
        }
        values
            .get(name)
            .cloned()
            .unwrap_or_else(|| match self.bindings.resolve(name, start)? {
                IdentityOperand::Binding(name) => Some(IdentityOperand::Scoped {
                    name,
                    owner: self.owner,
                    unit: self.unit,
                }),
                literal => Some(literal),
            })
    }

    pub(super) fn taken_name(&self, name: &str) -> bool {
        self.bindings.names().any(|binding| binding == name)
    }

    pub(super) fn repeated_guard(&self, name: &str) -> bool {
        self.guards.get(name).is_some_and(|count| *count > 1)
    }

    pub(super) fn matches(
        &self,
        patterns: &[NamePattern],
        name: &str,
        start: usize,
        values: &Values,
    ) -> bool {
        matches_any(patterns, name)
            || name.split_once('.').is_some_and(|(receiver, member)| {
                self.resolve(values, receiver, start)
                    .as_ref()
                    .and_then(binding_name)
                    .is_some_and(|root| {
                        root != receiver && matches_receiver(patterns, root, member)
                    })
            })
    }

    pub(super) fn has_registration(&self, candidates: &[NamePattern]) -> bool {
        self.calls.iter().any(|(_, name)| {
            matches_any(candidates, name)
                || name.split_once('.').is_some_and(|(_, member)| {
                    self.bindings
                        .names()
                        .any(|binding| matches_receiver(candidates, binding, member))
                })
        })
    }

    pub(super) fn models_cleanup(&self, cleanup: &[NamePattern], modeled: &HashSet<usize>) -> bool {
        let values = Values::new();
        self.calls.iter().all(|(start, name)| {
            modeled.contains(start) || !self.may_match_cleanup(cleanup, name, *start, &values)
        })
    }

    pub(super) fn may_match_cleanup(
        &self,
        cleanup: &[NamePattern],
        name: &str,
        start: usize,
        values: &Values,
    ) -> bool {
        if self.matches(cleanup, name, start, values) {
            return true;
        }
        // Unresolved receivers may refer to a scoped collection.
        name.split_once('.').is_some_and(|(receiver, member)| {
            (self.bindings.is_uncertain(receiver, start)
                || matches!(values.get(receiver), Some(None)))
                && self
                    .bindings
                    .names()
                    .any(|binding| matches_receiver(cleanup, binding, member))
        })
    }

    pub(super) fn entry(
        &self,
        selector: EntryIdentity,
        site: &CallSite,
        values: &Values,
    ) -> Option<Entry> {
        let (collection, key) = selector.operands(site)?;
        let collection = self.resolve(values, collection, site.start)?;
        binding_name(&collection)?;
        let key = self.value(values, key, site.start)?;
        Some(Entry { collection, key })
    }

    pub(super) fn value(
        &self,
        values: &Values,
        operand: &IdentityOperand,
        start: usize,
    ) -> Option<IdentityOperand> {
        match operand {
            IdentityOperand::Binding(name) => self.resolve(values, name, start),
            literal => Some(literal.clone()),
        }
    }
}

impl EntryIdentity {
    fn operands(self, site: &CallSite) -> Option<(&str, &IdentityOperand)> {
        let collection = match self.collection {
            CollectionSelector::Receiver => site.receiver.as_ref()?,
            CollectionSelector::Argument(index) => match site.arguments.get(index)?.as_ref()? {
                IdentityOperand::Binding(name) => name,
                _ => return None,
            },
        };
        let key = site.arguments.get(self.key_argument)?.as_ref()?;
        Some((collection.as_str(), key))
    }
}

fn binding_name(value: &IdentityOperand) -> Option<&str> {
    match value {
        IdentityOperand::Binding(name)
        | IdentityOperand::Fresh { name, .. }
        | IdentityOperand::Scoped { name, .. } => Some(name),
        _ => None,
    }
}

fn matches_any(patterns: &[NamePattern], name: &str) -> bool {
    patterns.iter().any(|pattern| pattern.matches(name))
}

fn matches_receiver(patterns: &[NamePattern], receiver: &str, member: &str) -> bool {
    matches_any(patterns, &format!("{receiver}.{member}"))
}

fn unsupported_flow(node: Node<'_>, source: &[u8]) -> bool {
    if matches!(
        node.kind(),
        "break_statement" | "continue_statement" | "label"
    ) {
        return crate::lower::keyed_flow::loops::jump_kind(node, source).is_none();
    }

    // Rust's postfix `?` and Kotlin's `try` share a node kind.
    if node.kind() == "try_expression" {
        if !node.child(0).is_some_and(|first| first.kind() == "try") {
            return false;
        }
        return !keyed_flow::supports_try(node)
            || !node.parent().is_some_and(|parent| parent.kind() == "block");
    }
    if node.kind() == "try_statement" {
        return !keyed_flow::supports_try(node);
    }
    matches!(
        node.kind(),
        "try_with_resources_statement"
            | "labeled_statement"
            | "switch_statement"
            | "switch_expression"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "select_statement"
            | "when_expression"
            | "match_expression"
            | "goto_statement"
            | "go_statement"
    )
}
