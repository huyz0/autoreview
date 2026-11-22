use tree_sitter::Node;

use super::{
    ancestors, children, descendants, identifier, is_execution_boundary, is_module_boundary, Scope,
    Source,
};

impl<'tree, 'text> Source<'tree, 'text> {
    pub(in crate::analyzers::rust_review) fn local_binding(
        &self,
        name: Node<'tree>,
    ) -> Option<Node<'tree>> {
        self.declaration(name)
            .filter(|binding| binding.kind() == "let_declaration")
    }

    pub(in crate::analyzers) fn declaration(
        &self,
        name: Node<'tree>,
    ) -> Option<Node<'tree>> {
        if name.kind() != "identifier" {
            return None;
        }
        let text = identifier(self.text(name));
        for scope_node in ancestors(name) {
            if let Some(scope) = self.scopes.get(&scope_node.id()) {
                if let Some(binding) = visible_local(scope, text, name) {
                    return binding
                        .child_by_field_name("pattern")
                        .filter(|pattern| pattern.kind() == "identifier")
                        .map(|_| binding);
                }
            }
            if scope_node.kind() == "function_item" {
                return scope_node
                    .child_by_field_name("parameters")
                    .into_iter()
                    .flat_map(children)
                    .find(|param| {
                        param.child_by_field_name("pattern").is_some_and(|pattern| {
                            pattern.kind() == "identifier" && identifier(self.text(pattern)) == text
                        })
                    });
            }
            if self.binds_in_context(scope_node, name, text)
                || is_execution_boundary(scope_node)
                || is_module_boundary(scope_node)
            {
                return None;
            }
        }
        None
    }

    pub(in crate::analyzers::rust_review) fn references_iteration_binding(
        &self,
        node: Node<'tree>,
        iteration: Node<'tree>,
    ) -> bool {
        if node.kind() != "identifier" {
            return false;
        }
        let name = identifier(self.text(node));
        for scope_node in ancestors(node) {
            if scope_node == iteration {
                return scope_node
                    .child_by_field_name("pattern")
                    .is_some_and(|pattern| {
                        pattern.kind() == "identifier" && identifier(self.text(pattern)) == name
                    });
            }
            if self
                .scopes
                .get(&scope_node.id())
                .is_some_and(|scope| visible_local(scope, name, node).is_some())
                || self.binds_in_context(scope_node, node, name)
                || is_execution_boundary(scope_node)
                || is_module_boundary(scope_node)
            {
                return false;
            }
        }
        false
    }

    pub(super) fn binds_in_context(
        &self,
        scope: Node<'tree>,
        anchor: Node<'tree>,
        name: &str,
    ) -> bool {
        let matches = |pattern| {
            descendants(pattern)
                .any(|part| part.kind() == "identifier" && identifier(self.text(part)) == name)
        };
        match scope.kind() {
            "function_item" | "closure_expression" => scope
                .child_by_field_name("parameters")
                .is_some_and(|params| {
                    children(params)
                        .any(|param| matches(param.child_by_field_name("pattern").unwrap_or(param)))
                }),
            "for_expression" => {
                scope
                    .child_by_field_name("body")
                    .is_some_and(|body| contains(body, anchor))
                    && scope.child_by_field_name("pattern").is_some_and(matches)
            }
            "match_arm" => scope.child_by_field_name("pattern").is_some_and(matches),
            "if_expression" | "while_expression" => {
                let in_body = ["consequence", "body"]
                    .into_iter()
                    .find_map(|field| scope.child_by_field_name(field))
                    .is_some_and(|body| contains(body, anchor));
                scope
                    .child_by_field_name("condition")
                    .is_some_and(|condition| {
                        (in_body || contains(condition, anchor))
                            && descendants(condition)
                                .filter(|part| {
                                    part.kind() == "let_condition"
                                        && part.end_byte() <= anchor.start_byte()
                                })
                                .filter_map(|part| part.child_by_field_name("pattern"))
                                .any(matches)
                    })
            }
            _ => false,
        }
    }
}

fn contains(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

pub(super) fn visible_local<'tree>(
    scope: &Scope<'tree>,
    name: &str,
    before: Node<'_>,
) -> Option<Node<'tree>> {
    scope
        .locals
        .get(name)?
        .iter()
        .rev()
        .copied()
        .find(|binding| binding.end_byte() <= before.start_byte())
}
