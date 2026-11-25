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
    pub key: Option<IdentityOperand>,
}

impl Entry {
    pub(super) fn handle(collection: IdentityOperand) -> Self {
        Self { collection, key: None }
    }

    pub(super) fn display_collection(&self) -> String {
        binding_name(&self.collection)
            .unwrap_or_default()
            .to_owned()
    }

    pub(super) fn display_key(&self) -> String {
        let Some(key) = &self.key else { return String::new() };
        match key {
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
    local_callables: HashSet<&'src str>,
    owner: usize,
    pub(super) unit: usize,
}

impl<'src> Syntax<'src> {
    pub(super) fn scan(
        function: Node<'_>,
        source: &'src [u8],
        writes: &HashSet<usize>,
        unit: usize,
        track_results: bool,
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
            syntax.record_calls_and_guards(node, source);
            if let Some(name) = local_callable(node, function, source) {
                syntax.local_callables.insert(name);
            }
            syntax.bindings.visit(node, source, writes.contains(&node.start_byte()), track_results);
            nodes.extend(children(node));
        }
        syntax.bindings.record_borrowed_writes();
        Some(syntax)
    }

    fn record_calls_and_guards(&mut self, node: Node<'_>, source: &[u8]) {
        match node.kind() {
            "method_invocation" | "call_expression" | "new_expression" => {
                if let Some(name) = name(node, source) {
                    self.calls.push((node.start_byte(), name));
                }
            }
            "if_statement" | "if_expression" => {
                if let Some((name, _)) = crate::lower::keyed_syntax::condition(node, source) {
                    *self.guards.entry(name).or_default() += 1;
                }
            }
            _ => {}
        }
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

    pub(super) fn local_result_binding(&self, name: &str, start: usize, defining: bool) -> Option<usize> {
        self.bindings.local_value(name, start, defining)
    }

    pub(super) fn shadows_api(&self, name: &str) -> bool {
        self.taken_name(name) || self.local_callables.contains(name)
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

    pub(super) fn any_call(&self, predicate: impl Fn(&str) -> bool) -> bool {
        self.calls.iter().any(|(_, name)| predicate(name))
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
        let collection = match selector.collection {
            CollectionSelector::Receiver => self.resolve(values, site.receiver.as_deref()?, site.start)?,
            CollectionSelector::Argument(index) => self.value(values, site.arguments.get(index)?.as_ref()?, site.start)?,
        };
        binding_name(&collection)?;
        let key = site.arguments.get(selector.key_argument)?.as_ref()?;
        let key = self.value(values, key, site.start)?;
        Some(Entry { collection, key: Some(key) })
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

fn local_callable<'src>(node: Node<'_>, function: Node<'_>, source: &'src [u8]) -> Option<&'src str> {
    if node == function || !matches!(node.kind(), "function_item" | "function_declaration") {
        return None;
    }
    node.child_by_field_name("name")?.utf8_text(source).ok()
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
