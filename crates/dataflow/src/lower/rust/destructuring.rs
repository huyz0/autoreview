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

pub(crate) fn literal_projection<'tree>(
    declaration: Node<'tree>,
    source: &[u8],
) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    let pattern = if declaration.kind() == "match_expression" {
        super::match_flow::single_match_pattern(declaration)?
    } else {
        declaration.child_by_field_name("pattern")?
    };
    let value = declaration.child_by_field_name("value")?;
    if !matches!(pattern.kind(), "tuple_pattern" | "slice_pattern" | "captured_pattern")
        || pattern.has_error() || value.has_error()
    {
        return None;
    }
    let mut elements = Vec::new();
    // A name in a conditional pattern can refer to a constant. Explicit
    // captures and wildcards prove a match without name resolution.
    let bare_bindings = declaration.kind() == "let_declaration"
        && declaration.child_by_field_name("alternative").is_none();
    project_literal(pattern, value, source, bare_bindings, &mut elements)?;
    Some(elements)
}

fn project_literal<'tree>(
    pattern: Node<'tree>,
    value: Node<'tree>,
    source: &[u8],
    bare_bindings: bool,
    elements: &mut Vec<(Node<'tree>, Node<'tree>)>,
) -> Option<()> {
    let value = super::branch_value::ungroup_value(value);
    if let Some(binding) = literal_binding(pattern, bare_bindings) {
        return (super::match_flow::simple_value(value, source) && straight_line_element(value))
            .then(|| elements.push((binding, value)));
    }
    literal_fields(pattern, value)?.into_iter().try_for_each(|(binding, value)| {
        project_literal(binding, value, source, bare_bindings, elements)
    })
}

fn literal_fields<'tree>(pattern: Node<'tree>, value: Node<'tree>) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    let aggregate = matches!((pattern.kind(), value.kind()),
        ("tuple_pattern", "tuple_expression") | ("slice_pattern", "array_expression"));
    if !aggregate || value.child_by_field_name("length").is_some() {
        return None;
    }
    let bindings: Vec<_> = patterns::pattern_parts(pattern).collect();
    if pattern.kind() == "tuple_pattern" && bindings.len() == 1
        && !pattern.children(&mut pattern.walk()).any(|child| child.kind() == ",")
    {
        return None;
    }
    let values: Vec<_> = children(value).collect();
    (bindings.len() == values.len()).then(|| bindings.into_iter().zip(values).collect())
}

fn literal_binding(pattern: Node<'_>, bare_bindings: bool) -> Option<Node<'_>> {
    match pattern.kind() {
        "_" => Some(pattern),
        "identifier" if bare_bindings => Some(pattern),
        "captured_pattern" => {
            let mut parts = patterns::pattern_parts(pattern);
            let binding = parts.next().filter(|part| part.kind() == "identifier")?;
            parts.next().filter(|part| part.kind() == "_")?;
            parts.next().is_none().then_some(binding)
        }
        _ => None,
    }
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
        if let Some(elements) = literal_projection(node, self.source).filter(|_| self.cfg.keyed_flow.is_some()) {
            return self.lower_literal_projection(node, at, names, elements);
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

    pub(super) fn lower_literal_projection(
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
