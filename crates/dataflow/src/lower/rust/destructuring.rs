use super::{branch_value::{self, Mode}, line, patterns, Bindings, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, IdentityOperand, Stmt};
use crate::lower::{keyed_syntax, record_evaluation, site::children};

pub(super) fn projected_bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    let pattern = node.child_by_field_name("pattern")?;
    matches!(pattern.kind(), "tuple_pattern" | "tuple_struct_pattern" | "struct_pattern"
        | "slice_pattern" | "reference_pattern" | "ref_pattern" | "mut_pattern")
        .then(|| patterns::projected_names(pattern, source))
        .flatten()
}

pub(super) fn declaration_bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    if node.kind() != "let_declaration" || node.child_by_field_name("alternative").is_some() {
        return None;
    }
    node.child_by_field_name("value")?;
    projected_bindings(node, source)
}

pub(crate) fn iterator_bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    if node.kind() != "for_expression" {
        return None;
    }
    let pattern = node.child_by_field_name("pattern")?;
    match pattern.kind() {
        "_" => Some(Vec::new()),
        "identifier" => Some(vec![pattern]),
        _ => projected_bindings(node, source),
    }
}

pub(crate) fn literal_tuple<'tree>(
    declaration: Node<'tree>,
    source: &[u8],
) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    let pattern = declaration.child_by_field_name("pattern")?;
    let mut value = declaration.child_by_field_name("value")?;
    while value.kind() == "parenthesized_expression" {
        value = children(value).next()?;
    }
    if pattern.kind() != "tuple_pattern" || value.kind() != "tuple_expression"
        || declaration.child_by_field_name("alternative").is_some()
        || super::stages_aggregate(value)
        || pattern.has_error() || value.has_error()
    {
        return None;
    }
    let bindings: Vec<_> = patterns::pattern_parts(pattern).collect();
    let elements: Vec<_> = children(value).collect();
    (bindings.len() == elements.len()
        && bindings.iter().all(|binding| matches!(binding.kind(), "identifier" | "_"))
        && elements.iter().copied().all(|element| {
            super::match_flow::simple_value(element, source) && straight_line_element(element)
        }))
        .then(|| bindings.into_iter().zip(elements).collect())
}

fn straight_line_element(node: Node<'_>) -> bool {
    match node.kind() {
        "try_expression" | "return_expression" | "break_expression" | "continue_expression"
        | "if_expression" | "match_expression" | "block" | "macro_invocation" => false,
        "closure_expression" | "async_block" => true,
        _ => children(node).all(straight_line_element),
    }
}

impl Lower<'_> {
    pub(super) fn destructured_declaration(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        names: &mut Bindings,
        bindings: Vec<Node<'_>>,
    ) -> Option<NodeId> {
        if let Some(elements) = literal_tuple(node, self.source).filter(|_| self.cfg.keyed_flow.is_some()) {
            return self.lower_literal_tuple(node, at, names, elements);
        }
        let value = node.child_by_field_name("value")?;
        let end = if let Some(branch) = branch_value::from_value(value) {
            self.lower_branch_value(branch, at, names, &Mode::Project)?
        } else {
            self.evaluate(Some(value), at, names)?
        };
        for binding in bindings {
            self.project_pattern_binding(binding, end, names);
        }
        Some(end)
    }

    fn lower_literal_tuple(
        &mut self,
        declaration: Node<'_>,
        mut at: NodeId,
        names: &mut Bindings,
        elements: Vec<(Node<'_>, Node<'_>)>,
    ) -> Option<NodeId> {
        let mut bindings = Vec::new();
        // Every initializer runs before any of the pattern's names enter scope.
        let mut elements = elements.into_iter();
        while let Some((binding, value)) = elements.next() {
            let Some(end) = self.value(Some(value), at, names, None) else {
                self.skip_calls(elements.map(|(_, value)| value));
                return None;
            };
            at = end;
            if binding.kind() == "identifier" {
                let temporary = self.fresh(super::argument_flow::OPERAND_TEMPORARY);
                let identity = keyed_syntax::result_operand(value, self.source, false);
                self.rebind_tuple_element(temporary.clone(), value, identity, at);
                bindings.push((binding, temporary));
            }
        }
        for (binding, temporary) in bindings {
            let name = self.text(binding).to_owned();
            names.insert(name.clone(), self.fresh(&name));
            self.rebind_tuple_element(name, binding, Some(IdentityOperand::Binding(temporary)), at);
        }
        record_evaluation(&mut self.cfg, at, declaration);
        Some(at)
    }

    fn rebind_tuple_element(&mut self, name: String, node: Node<'_>, value: Option<IdentityOperand>, at: NodeId) {
        self.cfg.emit(at, Stmt::Rebind(BindingWrite { name, start: node.start_byte(), value }), line(node));
    }
}
