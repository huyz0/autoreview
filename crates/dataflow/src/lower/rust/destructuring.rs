use super::{Bindings, Lower, Node, NodeId};

fn tuple_bindings<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>) -> Option<()> {
    match pattern.kind() {
        "identifier" => names.push(pattern),
        "_" => {}
        "tuple_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern
                .children(&mut cursor)
                .filter(|child| !child.is_extra() && !matches!(child.kind(), "(" | ")" | ","))
            {
                tuple_bindings(child, names)?;
            }
        }
        _ => return None,
    }
    Some(())
}

pub(super) fn tuple_names(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    let pattern = node.child_by_field_name("pattern")?;
    if pattern.kind() != "tuple_pattern" {
        return None;
    }
    let mut names = Vec::new();
    tuple_bindings(pattern, &mut names)?;
    Some(names)
}

pub(super) fn declaration_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    if node.kind() != "let_declaration" || node.child_by_field_name("alternative").is_some() {
        return None;
    }
    node.child_by_field_name("value")?;
    tuple_names(node)
}

pub(crate) fn iterator_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    if node.kind() != "for_expression" {
        return None;
    }
    let pattern = node.child_by_field_name("pattern")?;
    match pattern.kind() {
        "_" => Some(Vec::new()),
        "identifier" => Some(vec![pattern]),
        _ => tuple_names(node),
    }
}

impl Lower<'_> {
    pub(super) fn destructured_declaration(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        names: &mut Bindings,
        bindings: Vec<Node<'_>>,
    ) -> NodeId {
        let end = self.evaluate(node.child_by_field_name("value"), at, names);
        for binding in bindings {
            self.project_pattern_binding(binding, end, names);
        }
        end
    }
}
