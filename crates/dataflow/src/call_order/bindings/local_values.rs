use std::{collections::HashMap, ops::Range};

use tree_sitter::Node;

use super::{children, declares_iteration, identifiers_in};

#[derive(Default)]
pub(super) struct LocalValues<'src> {
    by_name: HashMap<&'src str, Vec<LocalValue>>,
}

struct LocalValue {
    declaration: usize,
    scope: Range<usize>,
    visible_at: usize,
    reusable: bool,
    tracked: bool,
}

impl<'src> LocalValues<'src> {
    pub(super) fn resolve(&self, name: &str, at: usize, defining: bool) -> Option<usize> {
        let locals = self.by_name.get(name)?;
        let at_declaration = |value: &LocalValue| defining && value.declaration == at;
        let binding = locals
            .iter()
            .filter(|value| at_declaration(value) || value.visible_at <= at && value.scope.contains(&at))
            .max_by_key(|value| (at_declaration(value), value.scope.start, value.declaration))?;
        // Go's := can write an existing local alongside newly declared
        // names. JavaScript var redeclarations also keep the same binding.
        let binding = if binding.reusable {
            locals.iter().filter(|value| value.scope == binding.scope).min_by_key(|value| value.declaration)?
        } else {
            binding
        };
        binding.tracked.then_some(binding.declaration)
    }

    pub(super) fn record(&mut self, declaration: Node<'_>, target: Node<'_>, source: &'src [u8]) {
        let web_var = is_web_var(declaration);
        let Some(scope) = scope(declaration, web_var) else { return };
        let targets = if target.kind() == "expression_list" { children(target).collect() } else { vec![target] };
        for target in targets {
            let visible_at = if web_var { scope.start } else { visible_after(declaration, target) };
            let reusable = web_var || declaration.kind() == "short_var_declaration";
            let tracked = !named_return(declaration);
            for identifier in identifiers_in(target) {
                if let Ok(name) = identifier.utf8_text(source) {
                    self.by_name.entry(name).or_default().push(LocalValue {
                        declaration: identifier.start_byte(),
                        scope: scope.clone(),
                        visible_at,
                        reusable,
                        tracked: tracked && identifier == target,
                    });
                }
            }
        }
    }
}

fn visible_after(declaration: Node<'_>, target: Node<'_>) -> usize {
    if declares_iteration(declaration) {
        return declaration
            .child_by_field_name("right")
            .or_else(|| declaration.child_by_field_name("value"))
            .map_or(target.end_byte(), |iterable| iterable.end_byte());
    }
    if declaration.kind() == "arrow_function" {
        return target.end_byte();
    }
    // Kotlin separates the binding name from its property initializer.
    declaration.parent().filter(|parent| parent.kind() == "property_declaration").unwrap_or(declaration).end_byte()
}

fn scope(node: Node<'_>, web_var: bool) -> Option<Range<usize>> {
    if declares_iteration(node) && node.kind() != "range_clause" {
        return Some(node.byte_range());
    }
    std::iter::successors(Some(node), Node::parent)
        .find(|parent| function_scope(*parent) || !web_var && block_scope(*parent))
        .map(|scope| {
            if function_scope(scope) {
                scope.child_by_field_name("body").unwrap_or(scope).byte_range()
            } else {
                scope.byte_range()
            }
        })
}

fn function_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "function_item"
            | "function_expression"
            | "func_literal"
            | "arrow_function"
            | "method_declaration"
            | "method_definition"
            | "closure_expression"
            | "lambda_literal"
            | "lambda_expression"
    )
}

fn block_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "block"
            | "statement_block"
            | "constructor_body"
            | "function_body"
            | "if_statement"
            | "for_statement"
            | "for_in_statement"
            | "enhanced_for_statement"
            | "catch_clause"
            | "catch_block"
    )
}

fn is_web_var(node: Node<'_>) -> bool {
    node.kind() == "variable_declarator" && node.parent().is_some_and(|parent| parent.kind() == "variable_declaration")
}

fn named_return(node: Node<'_>) -> bool {
    // Go's bare return reads these parameters without naming them.
    node.parent()
        .is_some_and(|params| params.parent().and_then(|owner| owner.child_by_field_name("result")) == Some(params))
}
