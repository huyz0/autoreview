use super::{assignment_parts, closure_result, return_value, site, Effect, Lower, Node};

impl Lower<'_, '_, '_> {
    pub(super) fn statement(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        if self.project_match(node, effects) {
            return;
        }
        if let Some(value) = self.statement_result(node) {
            self.returned(value, effects);
            return;
        }
        self.statement_effects(node, effects);
    }

    fn statement_result<'tree>(&self, node: Node<'tree>) -> Option<Node<'tree>> {
        if let Some(closure) = self.inside {
            closure_result(node, closure)
        } else if self.returns_to_caller {
            return_value(node).or_else(|| {
                self.return_tail
                    .filter(|tail| {
                        tail.byte_range() == node.byte_range() && tail.kind() == node.kind()
                    })
                    .map(|_| node)
            })
        } else {
            None
        }
    }

    fn statement_effects(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        match node.kind() {
            "expression_statement" => {
                if let Some(expr) = site::children(node).next() {
                    if !site::identifier(expr) {
                        self.statement(expr, effects);
                    }
                }
            }
            "lexical_declaration"
            | "variable_declaration"
            | "local_variable_declaration"
            | "var_declaration"
            | "var_spec_list" => {
                for child in site::children(node) {
                    self.statement(child, effects);
                }
            }
            _ => match assignment_parts(node, self.source) {
                Some((target, value, reassignment)) => {
                    self.assignment(node, target, value, reassignment, effects)
                }
                None => self.expression(node, effects),
            },
        }
    }
}
