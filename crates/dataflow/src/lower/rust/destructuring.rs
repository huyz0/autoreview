use super::{branch_value::{self, Mode}, line, patterns, Bindings, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, IdentityOperand, Stmt};
use crate::lower::{keyed_syntax, record_evaluation, site::children};

mod fields;
pub(super) mod iteration;
mod local;

pub(super) type Projection<'tree> = Vec<(Node<'tree>, Node<'tree>)>;

pub(super) enum ValueProjection<'tree> {
    Literal(Projection<'tree>),
    Local(Projection<'tree>),
}

pub(super) fn value_projection<'tree>(node: Node<'tree>, source: &[u8]) -> Option<ValueProjection<'tree>> {
    literal_projection(node, source).map(ValueProjection::Literal)
        .or_else(|| local::projection(node, source).map(ValueProjection::Local))
}

fn is_binding(node: Node<'_>) -> bool {
    matches!(node.kind(), "identifier" | "shorthand_field_identifier")
}

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
    pattern_projection(declaration, declaration.child_by_field_name("value")?, source)
}

fn pattern_projection<'tree>(
    declaration: Node<'tree>, value: Node<'tree>, source: &[u8],
) -> Option<Projection<'tree>> {
    let pattern = if declaration.kind() == "match_expression" {
        super::match_flow::single_match_pattern(declaration)?
    } else {
        declaration.child_by_field_name("pattern")?
    };
    if !matches!(pattern.kind(), "tuple_pattern" | "slice_pattern" | "struct_pattern" | "tuple_struct_pattern" | "captured_pattern")
        || pattern.has_error() || value.has_error()
    {
        return None;
    }
    // A name in a conditional pattern can refer to a constant. Explicit
    // captures and wildcards prove a match without name resolution.
    let bare_bindings = declaration.kind() == "let_declaration"
        && declaration.child_by_field_name("alternative").is_none();
    pattern_pairs(pattern, value, source, bare_bindings)
}

fn pattern_pairs<'tree>(pattern: Node<'tree>, value: Node<'tree>, source: &[u8], bare_bindings: bool) -> Option<Projection<'tree>> {
    let mut elements = Vec::new();
    project_literal(pattern, value, source, bare_bindings, &mut elements)?;
    Some(elements)
}

pub(crate) fn projects_constructor_argument(node: Node<'_>, constructor: Node<'_>, source: &[u8]) -> bool {
    if constructor.child_by_field_name("function").and_then(|path| {
        super::match_flow::exhaustiveness::tuple_constructor_arity(path, source)
    }).is_none() {
        return false;
    }
    std::iter::successors(constructor.parent(), |parent| parent.parent())
        .take_while(|parent| !matches!(parent.kind(), "function_item" | "closure_expression"))
        .filter(|parent| matches!(parent.kind(), "let_declaration" | "let_condition" | "match_expression" | "for_expression"))
        .any(|parent| contains_projected_origin(parent, node, source))
}

fn contains_projected_origin(parent: Node<'_>, origin: Node<'_>, source: &[u8]) -> bool {
    if parent.kind() == "for_expression" {
        return iteration::plan(parent, source).is_some_and(|plan| plan.items.iter().flatten().any(|(_, value)| *value == origin));
    }
    literal_projection(parent, source).is_some_and(|parts| parts.iter().any(|(_, value)| *value == origin))
        || matches!(local::initializer(parent, source), Some(local::Initializer::Snapshot(values)) if values.contains(&origin))
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
    fields::fields(pattern, value, source)?.into_iter().try_for_each(|(binding, value)| {
        project_literal(binding, value, source, bare_bindings, elements)
    })
}

fn literal_binding(pattern: Node<'_>, bare_bindings: bool) -> Option<Node<'_>> {
    match pattern.kind() {
        "_" | "remaining_field_pattern" | "shorthand_field_identifier" => Some(pattern),
        "mut_pattern" => patterns::pattern_parts(pattern).find(|part| part.kind() == "identifier"),
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
        if let Some(projection) = self.keyed_projection(node) {
            return self.lower_pattern_projection(node, at, names, projection);
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

    pub(super) fn keyed_projection<'tree>(&self, node: Node<'tree>) -> Option<ValueProjection<'tree>> {
        self.cfg.keyed_flow?;
        value_projection(node, self.source)
    }

    pub(super) fn lower_pattern_projection(
        &mut self, declaration: Node<'_>, at: NodeId, names: &mut Bindings, projection: ValueProjection<'_>,
    ) -> Option<NodeId> {
        match projection {
            ValueProjection::Literal(pairs) => self.lower_literal_projection(declaration, at, names, pairs),
            ValueProjection::Local(pairs) => {
                self.bind_staged_pattern(pairs, at, names);
                Some(at)
            }
        }
    }

    fn lower_literal_projection(
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
            if is_binding(binding) {
                let temporary = self.fresh(super::argument_flow::OPERAND_TEMPORARY);
                let identity = keyed_syntax::result_operand(value, self.source, false);
                self.rebind_tuple_element(temporary.clone(), value, identity, at);
                bindings.push((binding, temporary));
            }
        }
        for (binding, temporary) in bindings {
            self.bind_pattern_value(binding, temporary, at, names);
        }
        record_evaluation(&mut self.cfg, at, declaration);
        Some(at)
    }

    fn bind_pattern_value(&mut self, binding: Node<'_>, temporary: String, at: NodeId, names: &mut Bindings) {
        let name = self.text(binding).to_owned();
        names.insert(name.clone(), self.fresh(&name));
        self.rebind_tuple_element(name, binding, Some(IdentityOperand::Binding(temporary)), at);
    }

    fn rebind_tuple_element(&mut self, name: String, node: Node<'_>, value: Option<IdentityOperand>, at: NodeId) {
        self.cfg.emit(at, Stmt::Rebind(BindingWrite { name, start: node.start_byte(), value }), line(node));
    }
}

fn pattern_slot(origin: Node<'_>) -> String {
    format!("{}-pattern@{}", super::OPERAND_TEMPORARY, origin.start_byte())
}

impl Lower<'_> {
    pub(super) fn projected_local(&mut self, node: Node<'_>, at: NodeId, names: &Bindings) -> Option<Option<NodeId>> {
        self.cfg.keyed_flow?;
        Some(match local::initializer(node, self.source)? {
            local::Initializer::Snapshot(origins) => self.stage_pattern_values(origins, at, names),
            local::Initializer::Forward => Some(at),
        })
    }

    fn stage_pattern_values<'tree>(&mut self, origins: impl IntoIterator<Item = Node<'tree>>, mut at: NodeId, names: &Bindings) -> Option<NodeId> {
        let mut origins = origins.into_iter();
        while let Some(origin) = origins.next() {
            let Some(end) = self.value(Some(origin), at, names, None) else {
                self.skip_calls(origins);
                return None;
            };
            at = end;
            self.capture_operand(origin, Some(origin), &pattern_slot(origin), at);
        }
        Some(at)
    }

    fn bind_staged_pattern(&mut self, pairs: Projection<'_>, at: NodeId, names: &mut Bindings) {
        for (binding, origin) in pairs.into_iter().filter(|(binding, _)| is_binding(*binding)) {
            self.bind_pattern_value(binding, pattern_slot(origin), at, names);
            record_evaluation(&mut self.cfg, at, binding);
            if let Some(evaluation) = self.cfg.nodes[at].evaluations.last_mut() {
                evaluation.pattern_slot = Some(origin.byte_range());
            }
        }
    }
}
