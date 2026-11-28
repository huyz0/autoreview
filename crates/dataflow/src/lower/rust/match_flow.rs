use super::{branch_value::{self, Mode}, conditional, patterns, values, Bindings, EdgeKind, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, IdentityOperand, Stmt};
use crate::lower::site::children;

pub(super) mod guard;
mod exhaustiveness;

pub(super) fn supports(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() != "match_expression"
        || node.has_error()
        || !branch_value::supports_position(node, source)
        || !node.child_by_field_name("value").is_some_and(|value| simple_value(value, source))
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
    let wildcard = wildcard_arm(*last, source);
    rest.iter().all(|arm| supported_arm(*arm, source))
        && (wildcard || (supported_arm(*last, source) && exhaustiveness::covers_enum(node, &arms, source)))
}

fn pattern<'tree>(arm: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    if arm.kind() != "match_arm" || arm.child_by_field_name("value").is_none() {
        return None;
    }
    let pattern = arm.child_by_field_name("pattern")?;
    (pattern
        .child_by_field_name("condition")
        .is_none_or(|guard| conditional::supported(guard, source))
        && arm.child_by_field_name("value").is_some_and(|value| simple_arm(value, source)))
    .then_some(pattern)
}

// Only accept arm statements whose nested calls the Rust lowerer traverses.
pub(super) fn simple_arm(node: Node<'_>, source: &[u8]) -> bool {
    let simple_arm = |child| simple_arm(child, source);
    let simple_value = |child| simple_value(child, source);
    match node.kind() {
        "block" => children(node).all(simple_arm),
        "empty_statement" => true,
        "expression_statement" => children(node).next().is_some_and(simple_arm),
        "return_expression" | "break_expression" | "continue_expression" => {
            children(node).next().is_none_or(simple_value)
        }
        "let_declaration" => arm_declaration(node, source),
        "if_expression" => statement_if(node, source),
        "assignment_expression" => {
            node.child_by_field_name("left").is_some_and(|left| left.kind() == "identifier")
                && node.child_by_field_name("right").is_some_and(simple_value)
        }
        _ => simple_value(node),
    }
}

fn arm_declaration(node: Node<'_>, source: &[u8]) -> bool {
    if node.child_by_field_name("alternative").is_some() {
        return super::let_else::supports(node, source);
    }
    (super::simple_binding(node) || super::destructuring::declaration_bindings(node, source).is_some())
        && node.child_by_field_name("value").is_none_or(|value| simple_operand(value, source))
}

fn statement_if(node: Node<'_>, source: &[u8]) -> bool {
    let root = conditional::root_branch(node);
    (branch_value::is_discarded_statement(root) || conditional::unit_branch(root))
        && node.child_by_field_name("condition").is_some_and(|test| conditional::supported(test, source))
        && node.child_by_field_name("consequence").is_some_and(|body| simple_arm(body, source))
        && node.child_by_field_name("alternative").is_none_or(|otherwise| {
            children(otherwise).next().is_some_and(|body| simple_arm(body, source))
        })
}

pub(super) fn simple_value(node: Node<'_>, source: &[u8]) -> bool {
    let simple_value = |child| simple_value(child, source);
    match node.kind() {
        "identifier" | "self" | "scoped_identifier" | "integer_literal" | "boolean_literal"
        | "char_literal" | "string_literal" | "unit_expression" => true,
        "reference_expression" | "parenthesized_expression" | "try_expression" => {
            node.child_by_field_name("value").or_else(|| children(node).next()).is_some_and(simple_value)
        }
        "call_expression" => simple_call(node, source),
        "tuple_expression" | "array_expression" => super::argument_flow::aggregate_operands(node)
            .is_some_and(|mut operands| operands.all(|element| simple_operand(element, source))),
        _ => values::eager_operands(node).is_some_and(|mut operands| operands.all(simple_value)),
    }
}

fn simple_call(node: Node<'_>, source: &[u8]) -> bool {
    let Some(function) = node.child_by_field_name("function") else { return false };
    let function = if function.kind() == "generic_function" {
        let Some(callee) = function.child_by_field_name("function") else { return false };
        callee
    } else {
        function
    };
    let callee = matches!(function.kind(), "identifier" | "scoped_identifier")
        || (function.kind() == "field_expression" && function.child_by_field_name("value").is_some_and(|value| simple_value(value, source)));
    callee && node.child_by_field_name("arguments").is_some_and(|args| children(args).all(|argument| simple_operand(argument, source)))
}

fn simple_operand(operand: Node<'_>, source: &[u8]) -> bool {
    match branch_value::from_value(operand) {
        Some(branch) if branch.kind() == "if_expression" => branch_value::supports_value_if(branch, source),
        Some(branch) => supports(branch, source),
        None => simple_value(operand, source),
    }
}

fn supported_arm(arm: Node<'_>, source: &[u8]) -> bool {
    let Some(pattern) = arm_pattern(arm, source) else { return false };
    // Overlapping alternatives can run the same guard more than once.
    let bindings_source = if pattern.parent().and_then(|wrapper| wrapper.child_by_field_name("condition")).is_some() {
        &[]
    } else {
        source
    };
    patterns::projected_names(pattern, bindings_source).is_some()
}

fn arm_pattern<'tree>(arm: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    let pattern = pattern(arm, source)?;
    pattern.children(&mut pattern.walk()).find(|child| child.kind() == "_" || child.is_named() && !child.is_extra())
}

pub(super) fn projection_bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    patterns::projected_names(node.child_by_field_name("pattern")?, source)
}

pub(super) fn arm_bindings<'tree>(arm: Node<'tree>, source: &[u8]) -> Vec<Node<'tree>> {
    arm_pattern(arm, source).and_then(|pattern| patterns::projected_names(pattern, source)).unwrap_or_default()
}

pub(super) fn has_bindings(node: Node<'_>, source: &[u8]) -> bool {
    supports(node, source) && node.child_by_field_name("body").is_some_and(|body|
        body.named_children(&mut body.walk()).any(|arm| !arm_bindings(arm, source).is_empty()))
}

fn wildcard_arm(arm: Node<'_>, source: &[u8]) -> bool {
    pattern(arm, source).is_some_and(|pattern| {
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
        let evaluated = self.evaluate(node.child_by_field_name("value"), at, names)?;
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
            if let Some(fallback) = fallback.filter(|_| arm_pattern(arm, self.source).is_none_or(|pattern| pattern.kind() != "_")) {
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
        for binding in arm_bindings(arm, self.source) {
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
