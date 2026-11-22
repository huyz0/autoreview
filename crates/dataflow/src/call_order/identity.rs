use std::collections::HashSet;

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
    bindings: Bindings<'src>,
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
            if matches!(node.kind(), "method_invocation" | "call_expression") {
                if let Some(name) = name(node, source) {
                    syntax.calls.push((node.start_byte(), name));
                }
            }
            syntax.bindings.visit(node, source);
            nodes.extend(children(node));
        }
        syntax.bindings.record_borrowed_writes();
        Some(syntax)
    }

    pub(super) fn matches(&self, patterns: &[NamePattern], name: &str, start: usize) -> bool {
        matches_any(patterns, name) || self.resolved_receiver_match(patterns, name, start)
    }

    fn resolved_receiver_match(&self, patterns: &[NamePattern], name: &str, start: usize) -> bool {
        name.split_once('.').is_some_and(|(receiver, member)| {
            matches!(self.bindings.resolve(receiver, start), Some(IdentityOperand::Binding(root))
                if root != receiver && matches_receiver(patterns, &root, member))
        })
    }

    pub(super) fn models_cleanup(&self, cleanup: &[NamePattern], modeled: &HashSet<usize>) -> bool {
        self.calls.iter().all(|(start, name)| {
            modeled.contains(start) || !self.may_match_cleanup(cleanup, name, *start)
        })
    }

    fn may_match_cleanup(&self, cleanup: &[NamePattern], name: &str, start: usize) -> bool {
        if self.matches(cleanup, name, start) {
            return true;
        }
        // An ambiguous alias might have referred to a scoped collection at
        // this call. It cannot discharge a proven obligation.
        name.split_once('.').is_some_and(|(receiver, member)| {
            self.bindings.is_uncertain(receiver, start)
                && self
                    .bindings
                    .names()
                    .any(|binding| matches_receiver(cleanup, binding, member))
        })
    }

    pub(super) fn entry(&self, selector: EntryIdentity, site: &CallSite) -> Option<Entry> {
        let (collection, key) = selector.operands(site)?;
        let IdentityOperand::Binding(collection) = self.bindings.resolve(collection, site.start)?
        else {
            return None;
        };
        let key = match key {
            IdentityOperand::Binding(name) => self.bindings.resolve(name, site.start)?,
            literal => literal.clone(),
        };
        Some(Entry { collection, key })
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

fn matches_any(patterns: &[NamePattern], name: &str) -> bool {
    patterns.iter().any(|pattern| pattern.matches(name))
}

fn matches_receiver(patterns: &[NamePattern], receiver: &str, member: &str) -> bool {
    matches_any(patterns, &format!("{receiver}.{member}"))
}

fn unsupported_flow(node: Node<'_>) -> bool {
    // Rust's postfix `?` and Kotlin's `try` share a node kind.
    if node.kind() == "try_expression" {
        if !node.child(0).is_some_and(|first| first.kind() == "try") {
            return false;
        }
        return keyed_flow::blocks(node).is_none()
            || !node.parent().is_some_and(|parent| parent.kind() == "block");
    }
    if node.kind() == "try_statement" {
        return keyed_flow::blocks(node).is_none();
    }
    matches!(
        node.kind(),
        "try_with_resources_statement"
            | "catch_clause"
            | "catch_block"
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
