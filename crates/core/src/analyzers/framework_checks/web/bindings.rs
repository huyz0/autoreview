use std::collections::HashMap;

use super::{binding_nodes, is_function, loop_binding_kind};
use crate::analyzers::framework_checks::{descendants, text, Node};

pub(in crate::analyzers::framework_checks) struct BindingScopes<'t, 's> {
    scopes: HashMap<&'s str, Vec<Node<'t>>>,
}

impl<'t, 's> BindingScopes<'t, 's> {
    pub fn new(root: Node<'t>, source: &'s str) -> Self {
        let mut scopes: HashMap<_, Vec<_>> = HashMap::new();
        for declaration in descendants(root) {
            for binding in binding_nodes(declaration) {
                scopes.entry(text(binding, source)).or_default()
                    .push(binding_scope(binding, declaration));
            }
        }
        Self { scopes }
    }

    pub fn visible(&self, name: &str, use_site: Node<'_>) -> Option<Node<'t>> {
        self.scopes
            .get(name)?
            .iter()
            .copied()
            .filter(|scope| {
                scope.start_byte() <= use_site.start_byte()
                    && use_site.end_byte() <= scope.end_byte()
            })
            .min_by_key(|scope| scope.end_byte() - scope.start_byte())
    }
}

fn binding_scope<'t>(binding: Node<'t>, declaration: Node<'t>) -> Node<'t> {
    if declaration.kind() == "for_in_statement" {
        if loop_binding_kind(declaration) == Some("var") {
            return std::iter::successors(declaration.parent(), |node| node.parent())
                .find(|scope| is_function(*scope) || scope.kind() == "program")
                .unwrap_or(declaration);
        }
        return declaration;
    }
    let Some(parent) = binding.parent() else {
        return binding;
    };
    let owner = if matches!(parent.kind(), "function_declaration" | "class_declaration") {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    };
    let is_var = parent.kind() == "variable_declarator"
        && parent
            .parent()
            .is_some_and(|declaration| declaration.kind() == "variable_declaration");
    std::iter::successors(Some(owner), |node| node.parent())
        .find(|scope| {
            is_function(*scope) || scope.kind() == "program" || !is_var && lexical_scope(*scope)
        })
        .unwrap_or(owner)
}

fn lexical_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "statement_block" | "for_statement" | "for_in_statement" | "switch_body" | "catch_clause"
    )
}
