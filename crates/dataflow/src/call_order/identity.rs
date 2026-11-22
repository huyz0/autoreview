use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::cfg::{CallSite, IdentityOperand};
use crate::lower::{
    keyed_flow,
    site::{children, identifier, name, operand},
};
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
    declarations: HashMap<&'src str, Option<Binding<'src>>>,
    writes: HashSet<&'src str>,
    calls: Vec<(usize, String)>,
}

struct Binding<'src> {
    visible_at: usize,
    alias: Option<Alias<'src>>,
}

#[derive(Clone)]
struct Alias<'src> {
    operand: IdentityOperand,
    captured_at: usize,
    borrowed_root: Option<&'src str>,
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
        syntax.record_borrowed_writes();
        Some(syntax)
    }

    pub(super) fn matches(&self, patterns: &[NamePattern], name: &str, start: usize) -> bool {
        if patterns.iter().any(|pattern| pattern.matches(name)) {
            return true;
        }
        let Some((receiver, member)) = name.split_once('.') else {
            return false;
        };
        let Some(IdentityOperand::Binding(root)) = self.resolve(receiver, start) else {
            return false;
        };
        root != receiver
            && patterns
                .iter()
                .any(|pattern| pattern.matches(&format!("{root}.{member}")))
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
        let Some((receiver, member)) = name.split_once('.') else {
            return false;
        };
        // A rebound or shadowed receiver may have referred to a scoped
        // collection at this call. It cannot discharge a proven obligation.
        self.declarations.contains_key(receiver)
            && self.resolve(receiver, start).is_none()
            && self.declarations.keys().any(|candidate| {
                let qualified = format!("{candidate}.{member}");
                cleanup.iter().any(|pattern| pattern.matches(&qualified))
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
        let IdentityOperand::Binding(collection) = self.resolve(collection, site.start)? else {
            return None;
        };
        let key = match site.arguments.get(selector.key_argument)?.as_ref()? {
            IdentityOperand::Binding(name) => self.resolve(name, site.start)?,
            literal => literal.clone(),
        };
        Some(Entry { collection, key })
    }

    fn resolve<'a>(&'a self, mut name: &'a str, mut before: usize) -> Option<IdentityOperand> {
        loop {
            let binding = self.declarations.get(name)?.as_ref()?;
            if binding.visible_at > before || self.writes.contains(name) {
                return None;
            }
            let Some(alias) = &binding.alias else {
                return Some(IdentityOperand::Binding(name.to_owned()));
            };
            match &alias.operand {
                IdentityOperand::Binding(origin) => {
                    name = origin;
                    before = alias.captured_at;
                }
                literal => return Some(literal.clone()),
            }
        }
    }

    fn record_borrowed_writes(&mut self) {
        let mut affected: Vec<_> = self.writes.iter().copied().collect();
        while let Some(name) = affected.pop() {
            let Some(root) = self
                .declarations
                .get(name)
                .and_then(Option::as_ref)
                .and_then(|binding| binding.alias.as_ref()?.borrowed_root)
            else {
                continue;
            };
            if self.writes.insert(root) {
                affected.push(root);
            }
        }
    }

    fn visit(&mut self, node: Node<'_>, source: &'src [u8]) {
        if matches!(node.kind(), "method_invocation" | "call_expression") {
            if let Some(name) = name(node, source) {
                self.calls.push((node.start_byte(), name));
            }
        }
        self.record_declarations(node, source);
        if let Some(target) = write_target(node, source) {
            self.writes.extend(names_in(target, source));
        }
    }
    fn record_declarations(&mut self, node: Node<'_>, source: &'src [u8]) {
        let targets = declaration(node);
        let single_target = targets.len() == 1
            || matches!(
                node.kind(),
                "parameter_declaration" | "variadic_parameter_declaration"
            );
        let init = initializer(node);
        for target in targets {
            let simple = single_target && simple_target(target).is_some();
            let alias = simple
                .then_some(init)
                .flatten()
                .and_then(|value| direct_alias(value, source));
            let visible_at = init.map_or(target.end_byte(), |value| value.end_byte());
            let uncertain = !simple || (alias.is_some() && conditional_var(node));
            for name in names_in(target, source) {
                let binding = Binding {
                    visible_at,
                    alias: alias.clone(),
                };
                self.declarations
                    .entry(name)
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some(binding));
                if uncertain {
                    self.writes.insert(name);
                }
            }
        }
    }
}

fn declaration(node: Node<'_>) -> Vec<Node<'_>> {
    if matches!(
        node.kind(),
        "parameter_declaration" | "variadic_parameter_declaration" | "var_spec"
    ) {
        return node
            .children_by_field_name("name", &mut node.walk())
            .collect();
    }
    let target = match node.kind() {
        "variable_declarator" | "formal_parameter" | "catch_formal_parameter" => {
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
    };
    target.into_iter().collect()
}

fn initializer(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "short_var_declaration" => single(node.child_by_field_name("right")?),
        "variable_declaration"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "property_declaration") =>
        {
            children(node.parent()?)
                .last()
                .filter(|value| *value != node)
        }
        "variable_declarator" | "let_declaration" => node.child_by_field_name("value"),
        "var_spec" => single(node.child_by_field_name("value")?),
        _ => None,
    }
}

fn single(node: Node<'_>) -> Option<Node<'_>> {
    let mut children = children(node);
    let first = children.next()?;
    children.next().is_none().then_some(first)
}

fn simple_target(node: Node<'_>) -> Option<Node<'_>> {
    let node = if node.kind() == "expression_list" {
        single(node)?
    } else {
        node
    };
    identifier(node).then_some(node)
}

fn direct_alias<'src>(value: Node<'_>, source: &'src [u8]) -> Option<Alias<'src>> {
    let capture = value.start_byte();
    let (value, borrowed_root) = if mutable_reference(value) {
        let bound = value
            .child_by_field_name("value")
            .filter(|node| identifier(*node))?;
        (bound, bound.utf8_text(source).ok())
    } else {
        (value, None)
    };
    Some(Alias {
        operand: operand(value, source)?,
        captured_at: capture,
        borrowed_root,
    })
}

fn conditional_var(node: Node<'_>) -> bool {
    let Some(var) = node.parent().filter(|parent| {
        node.kind() == "variable_declarator" && parent.kind() == "variable_declaration"
    }) else {
        return false;
    };
    !var.parent()
        .and_then(|block| block.parent())
        .is_some_and(|owner| owner.kind() == "function_declaration")
}

fn mutable_reference(node: Node<'_>) -> bool {
    node.kind() == "reference_expression"
        && children(node).any(|child| child.kind() == "mutable_specifier")
}

fn direct_borrow_binding(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "let_declaration"
            && initializer(parent) == Some(node)
            && parent
                .child_by_field_name("pattern")
                .is_some_and(identifier)
            && node.child_by_field_name("value").is_some_and(identifier)
    })
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
        "reference_expression" if mutable_reference(node) && !direct_borrow_binding(node) => {
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
