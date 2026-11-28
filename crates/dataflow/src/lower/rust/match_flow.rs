use super::{branch_value::{self, Mode}, conditional, patterns, values, Bindings, EdgeKind, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, IdentityOperand, Stmt};
use crate::lower::site::children;

pub(super) mod guard;

// A final unguarded wildcard guarantees an arm.
pub(super) fn supports(node: Node<'_>) -> bool {
    if node.kind() != "match_expression"
        || node.has_error()
        || !branch_value::supports_position(node)
        || !node.child_by_field_name("value").is_some_and(simple_value)
    {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let arms: Vec<_> = body
        .named_children(&mut body.walk())
        .filter(|item| item.kind() != "line_comment" && item.kind() != "block_comment")
        .collect();
    let Some((last, rest)) = arms.split_last() else {
        return false;
    };
    rest.iter().all(|arm| supported_arm(*arm)) && wildcard_arm(*last)
}

fn pattern(arm: Node<'_>) -> Option<Node<'_>> {
    if arm.kind() != "match_arm" || arm.child_by_field_name("value").is_none() {
        return None;
    }
    let pattern = arm.child_by_field_name("pattern")?;
    (pattern
        .child_by_field_name("condition")
        .is_none_or(conditional::supported)
        && arm.child_by_field_name("value").is_some_and(simple_arm))
    .then_some(pattern)
}

// Only accept arm statements whose nested calls the Rust lowerer traverses.
pub(super) fn simple_arm(node: Node<'_>) -> bool {
    match node.kind() {
        "block" => children(node).all(simple_arm),
        "empty_statement" => true,
        "expression_statement" => children(node).next().is_some_and(simple_arm),
        "return_expression" => children(node).next().is_none_or(simple_value),
        "let_declaration" => {
            super::simple_binding(node)
                && node.child_by_field_name("value").is_none_or(simple_value)
        }
        "if_expression" => statement_if(node),
        "assignment_expression" => {
            node.child_by_field_name("left").is_some_and(|left| left.kind() == "identifier")
                && node.child_by_field_name("right").is_some_and(simple_value)
        }
        _ => simple_value(node),
    }
}

fn statement_if(node: Node<'_>) -> bool {
    let root = conditional::root_branch(node);
    (branch_value::is_discarded_statement(root) || conditional::unit_branch(root))
        && node.child_by_field_name("condition").is_some_and(conditional::supported)
        && node.child_by_field_name("consequence").is_some_and(simple_arm)
        && node.child_by_field_name("alternative").is_none_or(|otherwise| {
            children(otherwise).next().is_some_and(simple_arm)
        })
}

pub(super) fn simple_value(node: Node<'_>) -> bool {
    match node.kind() {
        "identifier" | "self" | "scoped_identifier" | "integer_literal" | "boolean_literal"
        | "char_literal" | "string_literal" | "unit_expression" => true,
        "reference_expression" | "parenthesized_expression" | "try_expression" => {
            node.child_by_field_name("value").or_else(|| children(node).next()).is_some_and(simple_value)
        }
        "call_expression" => simple_call(node),
        _ => values::eager_operands(node).is_some_and(|mut operands| operands.all(simple_value)),
    }
}

fn simple_call(node: Node<'_>) -> bool {
    let Some(function) = node.child_by_field_name("function") else { return false };
    let callee = matches!(function.kind(), "identifier" | "scoped_identifier")
        || (function.kind() == "field_expression" && function.child_by_field_name("value").is_some_and(simple_value));
    callee && node.child_by_field_name("arguments").is_some_and(|args| args.named_children(&mut args.walk()).all(simple_value))
}

fn supported_arm(arm: Node<'_>) -> bool {
    arm_pattern(arm).and_then(patterns::projected_names).is_some()
}

fn arm_pattern(arm: Node<'_>) -> Option<Node<'_>> {
    let pattern = pattern(arm)?;
    pattern.children(&mut pattern.walk()).find(|child| child.kind() == "_" || child.is_named() && !child.is_extra())
}

pub(super) fn projection_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    patterns::projected_names(node.child_by_field_name("pattern")?)
}

pub(super) fn arm_bindings(arm: Node<'_>) -> Vec<Node<'_>> {
    arm_pattern(arm).and_then(patterns::projected_names).unwrap_or_default()
}

pub(super) fn has_bindings(node: Node<'_>) -> bool {
    supports(node) && node.child_by_field_name("body").is_some_and(|body|
        body.named_children(&mut body.walk()).any(|arm| !arm_bindings(arm).is_empty()))
}

fn wildcard_arm(arm: Node<'_>) -> bool {
    pattern(arm).is_some_and(|pattern| {
        pattern.child_by_field_name("condition").is_none()
            && pattern.children(&mut pattern.walk()).any(|part| part.kind() == "_")
    })
}

impl Lower<'_> {
    pub(super) fn match_expression(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        names: &Bindings,
    ) -> Option<NodeId> {
        let mode = branch_value::is_returned(node).then_some(Mode::Return);
        self.select_match(node, at, names, mode.as_ref())
    }

    pub(super) fn select_match(
        &mut self, node: Node<'_>, at: NodeId, names: &Bindings, mode: Option<&Mode<'_>>,
    ) -> Option<NodeId> {
        let evaluated = self.evaluate(node.child_by_field_name("value"), at, names);
        let body = node.child_by_field_name("body")?;
        let mut ends = Vec::new();
        let arms: Vec<_> = body
            .named_children(&mut body.walk())
            .filter(|item| item.kind() == "match_arm")
            .collect();
        let mut next = evaluated;
        for (index, arm) in arms.iter().copied().enumerate() {
            let value = arm.child_by_field_name("value")?;
            let start = self.cfg.push_node(super::line(value));
            let last = index + 1 == arms.len();
            let fallback = (!last).then(|| self.cfg.push_node(super::line(arm)));
            self.edge(
                next,
                start,
                if last {
                    EdgeKind::Fallthrough
                } else {
                    EdgeKind::True
                },
            );
            if let Some(fallback) = fallback.filter(|_| arm_pattern(arm).is_none_or(|pattern| pattern.kind() != "_")) {
                self.edge(next, fallback, EdgeKind::False);
            }
            let mut arm_names = self.bind_arm(arm, start, names);
            let body_start = self.guard_entry(arm, start, fallback, &mut arm_names)?;
            let end = match mode {
                Some(mode) => self.lower_value_arm(value, body_start, &mut arm_names, mode),
                None => self.statement(value, body_start, &mut arm_names),
            };
            if let Some(end) = end {
                ends.push(end);
            }
            if let Some(fallback) = fallback {
                next = fallback;
            }
        }
        self.join_paths(node, ends)
    }

    fn bind_arm(&mut self, arm: Node<'_>, start: NodeId, names: &Bindings) -> Bindings {
        let mut arm_names = names.clone();
        for binding in arm_bindings(arm) {
            self.project_pattern_binding(binding, start, &mut arm_names);
        }
        arm_names
    }

    pub(super) fn project_pattern_binding(&mut self, binding: Node<'_>, at: NodeId, names: &mut Bindings) {
        let name = self.text(binding).to_owned();
        let start = binding.start_byte();
        names.insert(name.clone(), self.fresh(&name));
        if self.cfg.keyed_flow.is_some() {
            let value = Some(IdentityOperand::Fresh { name: name.clone(), start, age: 0, unit: 0 });
            self.cfg.emit(at, Stmt::Rebind(BindingWrite { name, start, value }), super::line(binding));
        }
    }

    fn guard_entry(
        &mut self,
        arm: Node<'_>,
        start: NodeId,
        fallback: Option<NodeId>,
        names: &mut Bindings,
    ) -> Option<NodeId> {
        let guard = arm
            .child_by_field_name("pattern")
            .and_then(|pattern| pattern.child_by_field_name("condition"));
        let Some(guard) = guard else {
            return Some(start);
        };
        let selected = self
            .cfg
            .push_node(super::line(arm.child_by_field_name("value")?));
        self.lower_pattern_test(guard, start, (selected, fallback?), names);
        Some(selected)
    }
}
