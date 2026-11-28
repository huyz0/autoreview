use super::{branch_value, line, values, Lower, Node, NodeId};
use crate::cfg::{BindingWrite, OperandEvaluation, Stmt};
use crate::lower::{keyed_syntax, record_evaluation, site::children};

pub(crate) const OPERAND_TEMPORARY: &str = "#rust-operand";

pub(super) fn is_operand(branch: Node<'_>) -> bool {
    let operand = branch_value::outermost_group(branch);
    let Some(owner) = operand.parent() else {
        return false;
    };
    let container = if owner.kind() == "arguments" {
        owner.parent().filter(|call| modeled_call(*call) && call.child_by_field_name("arguments") == Some(owner))
    } else {
        aggregate_operands(owner).is_some_and(|mut operands| operands.any(|item| item == operand)).then_some(owner)
    };
    container.is_some_and(can_evaluate)
}

pub(super) fn aggregate_operands(node: Node<'_>) -> Option<impl Iterator<Item = Node<'_>>> {
    if !matches!(node.kind(), "tuple_expression" | "array_expression") {
        return None;
    }
    let mut operands = children(node).peekable();
    let count = repeat_length(node);
    if let Some(length) = count {
        // A branch is an ordinary repeat operand, which Rust evaluates once even at length zero.
        let operand = operands.peek().copied().filter(|operand| *operand != length)?;
        branch_value::from_value(operand)?;
    }
    Some(operands.filter(move |operand| Some(*operand) != count))
}

fn repeat_length(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("length").filter(|_| node.kind() == "array_expression")
}

pub(crate) fn aggregate_result_operands<'tree>(node: Node<'tree>, source: &[u8]) -> impl Iterator<Item = Node<'tree>> {
    let empty = repeat_length(node).and_then(|length| super::element_access::literal_usize(length, source)) == Some(0);
    aggregate_operands(node).into_iter().flatten().filter(move |_| !empty)
}

fn modeled_call(call: Node<'_>) -> bool {
    call.kind() == "call_expression" && call.child_by_field_name("function").is_some_and(|callee| {
        let callee = callee.child_by_field_name("function").unwrap_or(callee);
        matches!(callee.kind(), "identifier" | "scoped_identifier" | "field_expression")
    })
}

pub(super) fn can_evaluate(node: Node<'_>) -> bool {
    evaluated_position(node) && within_lowered_blocks(node)
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
        .all(|block| {
            if super::is_labeled_block(block) {
                return evaluated_position(block);
            }
            block.parent().is_some_and(|owner| match owner.kind() {
                "function_item" | "for_expression" | "loop_expression" => owner.child_by_field_name("body") == Some(block),
                "while_expression" => owner.child_by_field_name("body") == Some(block)
                    || owner.child_by_field_name("condition") == Some(block),
                "if_expression" => owner.child_by_field_name("consequence") == Some(block),
                "match_arm" => owner.child_by_field_name("value") == Some(block),
                "let_declaration" => owner.child_by_field_name("alternative") == Some(block),
                "block" | "expression_statement" | "else_clause" => true,
                _ => false,
            })
        })
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
        self.record_operand(syntax, origin, value.is_some(), None, at);
        let identity = value.and_then(|value| keyed_syntax::result_operand(value, self.source, false));
        self.cfg.emit(at, Stmt::Rebind(BindingWrite {
            name: temporary.to_owned(), start: origin.start_byte(), value: identity,
        }), line(origin));
    }

    pub(super) fn record_operand(&mut self, syntax: Node<'_>, origin: Node<'_>, has_value: bool, projection_owner: Option<usize>, at: NodeId) {
        record_evaluation(&mut self.cfg, at, origin);
        if let Some(evaluation) = self.cfg.nodes[at].evaluations.last_mut() {
            evaluation.operand = Some(OperandEvaluation { slot: syntax.byte_range(), has_value, projection_owner });
        }
    }
}

pub(crate) fn stages_call(call: Node<'_>) -> bool {
    call.kind() == "call_expression" && contains_branch_operands(call)
}

pub(crate) fn stages_aggregate(node: Node<'_>) -> bool {
    aggregate_operands(node).is_some() && contains_branch_operands(node)
}

fn contains_branch_operands(root: Node<'_>) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        match node.kind() {
            "closure_expression" | "async_block" | "macro_invocation" | "function_item" => continue,
            "arguments" | "array_expression" | "tuple_expression" => {
                let previous = pending.len();
                if node.kind() == "arguments" {
                    pending.extend(children(node));
                } else {
                    pending.extend(aggregate_operands(node).into_iter().flatten());
                }
                if pending[previous..].iter().any(|operand| {
                    branch_value::from_value(*operand).is_some()
                        || super::is_labeled_block(branch_value::ungroup_value(*operand))
                }) {
                    return true;
                }
            }
            _ => pending.extend(children(node)),
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use autoreview_langsupport::{parser_for, Language};

    use crate::cfg::CallTarget;

    use super::*;

    #[test]
    fn array_repeat_evaluates_the_selected_arm_once_and_skips_the_count() {
        for (operand, count, expected) in [
            ("if true { selected() } else { () }", "0", Some("selected")),
            ("match 0 { _ => selected() }", "3", Some("selected")),
            ("match 0 { _ => selected() }", "count()", Some("selected")),
            ("const { skipped() }", "0", None),
        ] {
            let source = format!("fn run() {{ let _ = [{operand}; {count}]; }}");
            let tree = parser_for(Language::Rust).unwrap().parse(&source, None).unwrap();
            let function = tree.root_node().named_child(0).unwrap();
            assert!(!function.has_error(), "{source}");
            let graph = super::super::lower_keyed(source.as_bytes(), function);
            let calls: Vec<_> = graph.nodes.iter().flat_map(|node| &node.stmts).filter_map(|stmt| {
                match stmt {
                    Stmt::Call { target: CallTarget::Named(name), .. } => Some(name.as_str()),
                    _ => None,
                }
            }).collect();
            assert_eq!(calls, expected.into_iter().collect::<Vec<_>>(), "{source}");
        }
    }
}
