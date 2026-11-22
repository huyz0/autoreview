use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::cfg::IdentityOperand;
use crate::lower::site::{children, identifier, operand};

#[derive(Default)]
pub(super) struct Bindings<'src> {
    declarations: HashMap<&'src str, Option<Binding<'src>>>,
    writes: HashSet<&'src str>,
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

    pub(super) fn visit(&mut self, node: Node<'_>, source: &'src [u8]) {
        self.record_declarations(node, source);
        if let Some(target) = write_target(node, source) {
            self.writes.extend(names_in(target, source));
        }
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
