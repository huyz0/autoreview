use super::{Bindings, EdgeKind, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, IdentityOperand, Stmt};

// A final unguarded wildcard guarantees an arm.
pub(super) fn supports(node: Node<'_>) -> bool {
    if node.kind() != "match_expression" || node.has_error()
        || !node.parent().is_some_and(non_tail_statement)
        || !node.child_by_field_name("value").is_some_and(simple_value) { return false; }
    let Some(body) = node.child_by_field_name("body") else { return false };
    let arms: Vec<_> = body.named_children(&mut body.walk()).filter(|item| item.kind() != "line_comment" && item.kind() != "block_comment").collect();
    let Some((last, rest)) = arms.split_last() else { return false };
    rest.iter().all(|arm| supported_arm(*arm)) && wildcard_arm(*last)
}

fn non_tail_statement(parent: Node<'_>) -> bool {
    parent.kind() == "expression_statement" && parent.parent().is_some_and(|block| block.kind() == "block")
        && std::iter::successors(parent.next_named_sibling(), |node| node.next_named_sibling())
            .any(|node| !matches!(node.kind(), "line_comment" | "block_comment"))
}

fn pattern(arm: Node<'_>) -> Option<Node<'_>> {
    if arm.kind() != "match_arm" || arm.child_by_field_name("value").is_none() { return None; }
    let pattern = arm.child_by_field_name("pattern")?;
    (pattern.child_by_field_name("condition").is_none_or(simple_value) && arm.child_by_field_name("value").is_some_and(simple_arm)).then_some(pattern)
}

// Only accept arm statements whose nested calls the Rust lowerer traverses.
fn simple_arm(node: Node<'_>) -> bool {
    match node.kind() {
        "block" => node.named_children(&mut node.walk()).all(simple_arm),
        "line_comment" | "block_comment" | "empty_statement" => true,
        "expression_statement" | "return_expression" => node.named_child(0).is_none_or(simple_arm),
        "let_declaration" => super::simple_binding(node) && node.child_by_field_name("value").is_none_or(simple_value),
        _ => simple_value(node),
    }
}

fn simple_value(node: Node<'_>) -> bool {
    match node.kind() {
        "identifier" | "integer_literal" | "boolean_literal" | "char_literal" | "string_literal" | "unit_expression" => true,
        "reference_expression" | "parenthesized_expression" | "try_expression" => node.named_child(0).is_some_and(simple_value),
        "call_expression" => simple_call(node),
        _ => false,
    }
}

fn simple_call(node: Node<'_>) -> bool {
    let Some(function) = node.child_by_field_name("function") else { return false };
    let callee = matches!(function.kind(), "identifier" | "scoped_identifier")
        || (function.kind() == "field_expression" && function.child_by_field_name("value").is_some_and(|value| value.kind() == "identifier"));
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

fn tuple_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
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

pub(super) fn arm_bindings(arm: Node<'_>) -> Vec<Node<'_>> {
    pattern(arm).and_then(|pattern| pattern.named_child(0)).and_then(tuple_bindings).unwrap_or_default()
}

pub(super) fn binding_lines(function: Node<'_>) -> Vec<u32> {
    let mut pending = function.child_by_field_name("body").into_iter().collect::<Vec<_>>();
    let mut lines = Vec::new();
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "function_item" | "closure_expression" | "async_block" | "macro_invocation") { continue; }
        if supports(node) && node.child_by_field_name("body").is_some_and(|body|
            body.named_children(&mut body.walk()).any(|arm| !arm_bindings(arm).is_empty())) {
            lines.push(node.start_position().row as u32 + 1);
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

fn wildcard_arm(arm: Node<'_>) -> bool {
    pattern(arm).is_some_and(|pattern| pattern.child_by_field_name("condition").is_none()
        && pattern.child_count() == 1 && pattern.child(0).is_some_and(|part| part.kind() == "_"))
}

impl Lower<'_> {
    pub(super) fn match_expression(&mut self, node: Node<'_>, at: NodeId, names: &Bindings) -> Option<NodeId> {
        let evaluated = self.evaluate(node.child_by_field_name("value"), at, names);
        let body = node.child_by_field_name("body")?;
        let mut ends = Vec::new();
        let arms: Vec<_> = body.named_children(&mut body.walk()).filter(|item| item.kind() == "match_arm").collect();
        let mut next = evaluated;
        for (index, arm) in arms.iter().copied().enumerate() {
            let value = arm.child_by_field_name("value")?;
            let start = self.cfg.push_node(super::line(value));
            let last = index + 1 == arms.len();
            let fallback = (!last).then(|| self.cfg.push_node(super::line(arm)));
            self.edge(next, start, if last { EdgeKind::Fallthrough } else { EdgeKind::True });
            if let Some(fallback) = fallback { self.edge(next, fallback, EdgeKind::False); }
            let mut arm_names = names.clone();
            for binding in arm_bindings(arm) {
                let name = self.text(binding).to_owned();
                arm_names.insert(name.clone(), self.fresh(&name));
                if self.cfg.keyed_flow.is_some() {
                    self.cfg.emit(start, Stmt::Rebind(BindingWrite { name: name.clone(), start: binding.start_byte(),
                        value: Some(IdentityOperand::Fresh { name, start: binding.start_byte(), age: 0, unit: 0 }) }), super::line(binding));
                }
            }
            let guard = arm.child_by_field_name("pattern").and_then(|pattern| pattern.child_by_field_name("condition"));
            let body_start = if let Some(guard) = guard {
                let checked = self.evaluate(Some(guard), start, &arm_names);
                let selected = self.cfg.push_node(super::line(value));
                self.edge(checked, selected, EdgeKind::True);
                self.edge(checked, fallback?, EdgeKind::False);
                selected
            } else { start };
            if let Some(end) = self.statement(value, body_start, &mut arm_names) { ends.push(end); }
            if let Some(fallback) = fallback { next = fallback; }
        }
        if ends.is_empty() { return None; }
        let join = self.cfg.push_node(super::line(node));
        for end in ends { self.edge(end, join, EdgeKind::Fallthrough); }
        Some(join)
    }
}

