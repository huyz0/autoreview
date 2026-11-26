use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::cfg::IdentityOperand;
use crate::lower::site::{children, identifier, operand};

mod local_values;
use local_values::LocalValues;

#[derive(Default)]
pub(super) struct Bindings<'src> {
    declarations: HashMap<&'src str, Option<Binding<'src>>>,
    writes: HashSet<&'src str>,
    all_writes: HashSet<&'src str>,
    local_values: LocalValues<'src>,
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

impl<'src> Bindings<'src> {
    pub(super) fn names(&self) -> impl Iterator<Item = &'src str> + '_ {
        self.declarations.keys().copied()
    }

    pub(super) fn is_uncertain(&self, name: &str, start: usize) -> bool {
        self.declarations.contains_key(name) && self.resolve(name, start).is_none()
    }

    pub(super) fn visit(&mut self, node: Node<'_>, source: &'src [u8], modeled_write: bool, track_results: bool) {
        self.record_declarations(node, source, track_results);
        if mutable_reference(node) {
            if let Some(target) = node.child_by_field_name("value") { self.all_writes.extend(names_in(target, source)); }
        }
        if let Some(target) = write_target(node, source) {
            self.all_writes.extend(names_in(target, source));
            if !modeled_write {
                self.writes.extend(names_in(target, source));
            }
        }
    }

    pub(super) fn stable_boolean(&self, name: &str, at: usize) -> Option<bool> {
        if !self.can_read(name) || self.all_writes.contains(name) { return None; }
        let binding = self.declarations.get(name)?.as_ref()?;
        if binding.visible_at > at { return None; }
        match &binding.alias.as_ref()?.operand {
            IdentityOperand::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    pub(super) fn can_read(&self, name: &str) -> bool {
        self.declarations.get(name).is_some_and(Option::is_some) && !self.writes.contains(name)
    }

    pub(super) fn local_value(&self, name: &str, at: usize, defining: bool) -> Option<usize> {
        self.local_values.resolve(name, at, defining)
    }

    pub(super) fn resolve<'a>(
        &'a self,
        mut name: &'a str,
        mut before: usize,
    ) -> Option<IdentityOperand> {
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

    pub(super) fn record_borrowed_writes(&mut self) {
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

    fn record_declarations(&mut self, node: Node<'_>, source: &'src [u8], track_results: bool) {
        let targets = if node.kind() == "match_arm" && node.parent().and_then(|body| body.parent()).is_some_and(crate::lower::rust::supports_match) {
            crate::lower::rust::match_arm_bindings(node)
        } else if declares_iteration(node) {
            crate::lower::keyed_flow::loops::iterator_bindings(node)
        } else {
            declaration(node)
        };
        let single_target = targets.len() == 1 || declares_iteration(node) || node.kind() == "match_arm"
            || matches!(
                node.kind(),
                "parameter_declaration" | "variadic_parameter_declaration"
            );
        let init = initializer(node);
        for target in targets {
            if track_results {
                self.local_values.record(node, target, source);
            }
            let simple = single_target && simple_target(target).is_some();
            self.record_identity(node, target, init, simple, source);
        }
    }

    fn record_identity(
        &mut self, node: Node<'_>, target: Node<'_>, init: Option<Node<'_>>, simple: bool, source: &'src [u8],
    ) {
        let alias = init.filter(|_| simple).and_then(|value| direct_alias(value, source));
        let visible_at = init.map_or(target.end_byte(), |value| value.end_byte());
        let uncertain = !simple || (alias.is_some() && conditional_var(node));
        for name in names_in(target, source) {
            let binding = Binding { visible_at, alias: alias.clone() };
            self.declarations.entry(name).and_modify(|slot| *slot = None).or_insert(Some(binding));
            if uncertain {
                self.writes.insert(name);
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
        "arrow_function" => node.child_by_field_name("parameter"),
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

fn declares_iteration(node: Node<'_>) -> bool {
    match node.kind() {
        "range_clause" => node.children(&mut node.walk()).any(|part| part.kind() == ":="),
        "for_in_statement" => node.child_by_field_name("kind").is_some(),
        // Kotlin's variable_declaration already contributes its own binding.
        "enhanced_for_statement" | "for_expression" => true,
        _ => false,
    }
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
        "update_expression" | "inc_statement" | "dec_statement" => node
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
    identifiers_in(node).into_iter().filter_map(|node| node.utf8_text(source).ok()).collect()
}

fn identifiers_in(node: Node<'_>) -> Vec<Node<'_>> {
    let mut identifiers = Vec::new();
    let mut nodes = vec![node];
    while let Some(node) = nodes.pop() {
        if identifier(node)
            || matches!(
                node.kind(),
                "shorthand_property_identifier_pattern" | "shorthand_field_identifier"
            )
        {
            identifiers.push(node);
        } else {
            nodes.extend(children(node));
        }
    }
    identifiers
}
