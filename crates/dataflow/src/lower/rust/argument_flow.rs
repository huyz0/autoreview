use super::{branch_value, line, values, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, OperandEvaluation, Stmt};
use crate::lower::{keyed_syntax, record_evaluation, site::children};

pub(crate) const OPERAND_TEMPORARY: &str = "#rust-operand";

pub(super) fn is_argument(branch: Node<'_>) -> bool {
    let argument = branch_value::outermost_group(branch);
    let Some(arguments) = argument.parent().filter(|node| node.kind() == "arguments") else {
        return false;
    };
    arguments.parent().is_some_and(|call| {
        modeled_call(call) && call.child_by_field_name("arguments") == Some(arguments)
            && evaluated_position(call) && within_lowered_blocks(call)
    })
}

fn modeled_call(call: Node<'_>) -> bool {
    call.kind() == "call_expression" && call.child_by_field_name("function").is_some_and(|callee| {
        let callee = callee.child_by_field_name("function").unwrap_or(callee);
        matches!(callee.kind(), "identifier" | "scoped_identifier" | "field_expression")
    })
}

fn evaluated_position(mut value: Node<'_>) -> bool {
    while let Some(owner) = value.parent() {
        if let Some(supported) = evaluation_boundary(owner, value) {
            return supported;
        }
        if !evaluates_child(owner, value) {
            return false;
        }
        value = owner;
    }
    false
}

fn evaluation_boundary(owner: Node<'_>, value: Node<'_>) -> Option<bool> {
    let field = match owner.kind() {
        "block" | "expression_statement" | "return_expression" | "break_expression" => return Some(true),
        "let_declaration" | "let_condition" | "match_expression" | "for_expression" | "match_arm" => "value",
        "assignment_expression" => "right",
        "if_expression" | "while_expression" | "match_pattern" => "condition",
        _ => return None,
    };
    Some(owner.child_by_field_name(field) == Some(value))
}

fn evaluates_child(owner: Node<'_>, value: Node<'_>) -> bool {
    match owner.kind() {
        "arguments" | "parenthesized_expression" | "reference_expression" | "try_expression" | "await_expression" => true,
        "call_expression" => modeled_call(owner) && matches!(value.kind(), "arguments" | "field_expression" | "generic_function"),
        "generic_function" => owner.child_by_field_name("function") == Some(value),
        _ => values::eager_operands(owner).is_some_and(|mut operands| operands.any(|item| item == value)),
    }
}

fn within_lowered_blocks(call: Node<'_>) -> bool {
    std::iter::successors(call.parent(), |node| node.parent())
        .take_while(|node| node.kind() != "function_item")
        .filter(|node| node.kind() == "block")
        .all(|block| block.parent().is_some_and(|owner| match owner.kind() {
            "function_item" | "for_expression" | "while_expression" | "loop_expression" => owner.child_by_field_name("body") == Some(block),
            "if_expression" => owner.child_by_field_name("consequence") == Some(block),
            "match_arm" => owner.child_by_field_name("value") == Some(block),
            "let_declaration" => owner.child_by_field_name("alternative") == Some(block),
            "block" | "expression_statement" | "else_clause" => true,
            _ => false,
        }))
}

impl Lower<'_> {
    pub(super) fn skip_calls<'tree>(&mut self, nodes: impl IntoIterator<Item = Node<'tree>>) {
        if self.cfg.keyed_flow.is_none() {
            return;
        }
        let mut pending: Vec<_> = nodes.into_iter().collect();
        while let Some(node) = pending.pop() {
            if node.kind() == "call_expression" {
                self.cfg.unreachable_calls.push(node.start_byte());
            }
            pending.extend(children(node));
        }
    }

    pub(super) fn capture_operand(&mut self, syntax: Node<'_>, value: Option<Node<'_>>, temporary: &str, at: NodeId) {
        if self.cfg.keyed_flow.is_none() {
            return;
        }
        let origin = value.unwrap_or(syntax);
        record_evaluation(&mut self.cfg, at, origin);
        if let Some(evaluation) = self.cfg.nodes[at].evaluations.last_mut() {
            evaluation.operand = Some(OperandEvaluation { slot: syntax.byte_range(), has_value: value.is_some() });
        }
        let identity = value.and_then(|value| keyed_syntax::result_operand(value, self.source, false));
        self.cfg.emit(at, Stmt::Rebind(BindingWrite {
            name: temporary.to_owned(), start: origin.start_byte(), value: identity,
        }), line(origin));
    }
}

pub(crate) fn stages_call(call: Node<'_>) -> bool {
    if call.kind() != "call_expression" {
        return false;
    }
    let mut pending = vec![call];
    while let Some(node) = pending.pop() {
        match node.kind() {
            "closure_expression" | "async_block" | "macro_invocation" | "function_item" => continue,
            "arguments" if children(node).any(|arg| branch_value::from_value(arg).is_some()) => return true,
            _ => pending.extend(children(node)),
        }
    }
    false
}
