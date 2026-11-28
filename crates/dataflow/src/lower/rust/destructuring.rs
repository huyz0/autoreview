use super::{branch_value::{self, Mode}, patterns, Bindings, Lower, Node, NodeId};

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

impl Lower<'_> {
    pub(super) fn destructured_declaration(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        names: &mut Bindings,
        bindings: Vec<Node<'_>>,
    ) -> Option<NodeId> {
        let value = node.child_by_field_name("value")?;
        let end = if let Some(branch) = branch_value::from_value(value) {
            self.lower_branch_value(branch, at, names, &Mode::Project)?
        } else {
            self.evaluate(Some(value), at, names)
        };
        for binding in bindings {
            self.project_pattern_binding(binding, end, names);
        }
        Some(end)
    }
}
