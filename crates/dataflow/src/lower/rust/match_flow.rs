use super::{Bindings, EdgeKind, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, IdentityOperand, Stmt};

pub(super) mod guard;
mod returns;

pub(super) use returns::returns_match;
use returns::statement_match;

// A final unguarded wildcard guarantees an arm.
pub(super) fn supports(node: Node<'_>) -> bool {
    if node.kind() != "match_expression"
        || node.has_error()
        || !(returns_match(node) || node.parent().is_some_and(statement_match))
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
        .is_none_or(guard::supported)
        && arm.child_by_field_name("value").is_some_and(simple_arm))
    .then_some(pattern)
}

// Only accept arm statements whose nested calls the Rust lowerer traverses.
fn simple_arm(node: Node<'_>) -> bool {
    match node.kind() {
        "block" => node.named_children(&mut node.walk()).all(simple_arm),
        "line_comment" | "block_comment" | "empty_statement" => true,
        "expression_statement" | "return_expression" => node.named_child(0).is_none_or(simple_arm),
        "let_declaration" => {
            super::simple_binding(node)
                && node.child_by_field_name("value").is_none_or(simple_value)
        }
        _ => simple_value(node),
    }
}

pub(super) fn simple_value(node: Node<'_>) -> bool {
    match node.kind() {
        "identifier" | "self" | "scoped_identifier" | "integer_literal" | "boolean_literal"
        | "char_literal" | "string_literal" | "unit_expression" => true,
        "field_expression" => node.child_by_field_name("value").is_some_and(|receiver| {
            matches!(receiver.kind(), "identifier" | "self" | "field_expression")
                && simple_value(receiver)
        }),
        "reference_expression" | "parenthesized_expression" | "try_expression" => {
            node.named_child(0).is_some_and(simple_value)
        }
        "call_expression" => simple_call(node),
        _ => false,
    }
}

fn simple_call(node: Node<'_>) -> bool {
    let Some(function) = node.child_by_field_name("function") else { return false };
    let callee = matches!(function.kind(), "identifier" | "scoped_identifier")
        || (function.kind() == "field_expression" && function.child_by_field_name("value").is_some_and(simple_value));
    callee && node.child_by_field_name("arguments").is_some_and(|args| args.named_children(&mut args.walk()).all(simple_value))
}

fn supported_arm(arm: Node<'_>) -> bool {
    pattern(arm).is_some_and(|pattern| {
        let guard = pattern.child_by_field_name("condition");
        let parts: Vec<_> = pattern.named_children(&mut pattern.walk()).filter(|part| Some(*part) != guard).collect();
        matches!(parts.as_slice(), [part] if matches!(part.kind(), "integer_literal" | "boolean_literal" | "char_literal" | "string_literal" | "scoped_identifier")
            || tuple_bindings(*part).is_some())
    })
}

pub(super) fn tuple_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    let ty = node.child_by_field_name("type")?;
    if node.kind() != "tuple_struct_pattern" || !matches!(ty.kind(), "identifier" | "scoped_identifier") { return None; }
    let mut bindings = Vec::new();
    let mut has_element = false;
    for child in node.children(&mut node.walk()) {
        if child == ty || matches!(child.kind(), "(" | ")" | ",") { continue; }
        match child.kind() {
            "_" => has_element = true,
            "identifier" => { has_element = true; bindings.push(child); }
            _ => return None,
        }
    }
    has_element.then_some(bindings)
}

pub(super) fn projection_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    let pattern = node.child_by_field_name("pattern")?;
    match pattern.kind() {
        "scoped_identifier" | "boolean_literal" | "integer_literal" => Some(Vec::new()),
        _ => tuple_bindings(pattern),
    }
}

pub(super) fn arm_bindings(arm: Node<'_>) -> Vec<Node<'_>> {
    pattern(arm).and_then(|pattern| pattern.named_child(0)).and_then(tuple_bindings).unwrap_or_default()
}

pub(super) fn binding_lines(function: Node<'_>, has_bindings: impl Fn(Node<'_>) -> bool) -> Vec<u32> {
    let mut pending = function.child_by_field_name("body").into_iter().collect::<Vec<_>>();
    let mut lines = Vec::new();
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "function_item" | "closure_expression" | "async_block" | "macro_invocation") { continue; }
        if has_bindings(node) {
            lines.push(node.start_position().row as u32 + 1);
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

pub(super) fn has_bindings(node: Node<'_>) -> bool {
    supports(node) && node.child_by_field_name("body").is_some_and(|body|
        body.named_children(&mut body.walk()).any(|arm| !arm_bindings(arm).is_empty()))
}

fn wildcard_arm(arm: Node<'_>) -> bool {
    pattern(arm).is_some_and(|pattern| pattern.child_by_field_name("condition").is_none()
        && pattern.child_count() == 1 && pattern.child(0).is_some_and(|part| part.kind() == "_"))
}

impl Lower<'_> {
    pub(super) fn match_expression(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        names: &Bindings,
    ) -> Option<NodeId> {
        let evaluated = self.evaluate(node.child_by_field_name("value"), at, names);
        let body = node.child_by_field_name("body")?;
        let mut ends = Vec::new();
        let returning = returns_match(node);
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
            if let Some(fallback) = fallback {
                self.edge(next, fallback, EdgeKind::False);
            }
            let mut arm_names = self.bind_arm(arm, start, names);
            let body_start = self.guard_entry(arm, start, fallback, &arm_names)?;
            if returning {
                self.return_match_arm(value, body_start, &mut arm_names);
            } else if let Some(end) = self.statement(value, body_start, &mut arm_names) {
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
        names: &Bindings,
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
        self.lower_guard(guard, start, (selected, fallback?), names);
        Some(selected)
    }
}
