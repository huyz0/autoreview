use tree_sitter::Node;
use crate::cfg::{Cfg, EdgeKind, IdentityOperand, NodeId, Stmt};
use super::{direct_call, keyed_syntax, line, nested_arguments, site, throwing_call};

pub(super) fn lower(cfg: &mut Cfg<Stmt>, node: Node<'_>, source: &[u8], current: NodeId) -> Option<NodeId> {
    if !matches!(node.kind(), "expression_statement" | "return_statement") { return None; }
    let returning = node.kind() == "return_statement";
    let expression = node.named_child(0).and_then(arm_value)?;
    if expression.kind() != "ternary_expression" { return None; }
    let test = expression.child_by_field_name("condition")?;
    if !simple_test(test, source) { return None; }
    let yes = arm_value(expression.child_by_field_name("consequence")?)?;
    let no = arm_value(expression.child_by_field_name("alternative")?)?;
    if ![yes, no].into_iter().all(|arm| simple_arm(arm, source)) { return None; }
    let literal = arm_value(test)?.kind();
    cfg.nodes[current].condition = keyed_syntax::condition_value(test, source);
    super::super::record_evaluation(cfg, current, test);
    let join = cfg.push_node(line(node));
    for (arm, edge, impossible) in [(yes, EdgeKind::True, literal == "false"), (no, EdgeKind::False, literal == "true")] {
        let start = cfg.push_node(line(arm));
        if !impossible { cfg.edges.push((current, start, edge)); }
        lower_arm(cfg, arm, source, (start, join), returning.then_some(node.start_byte()));
    }
    Some(join)
}

fn lower_arm(cfg: &mut Cfg<Stmt>, arm: Node<'_>, source: &[u8], nodes: (NodeId, NodeId), return_start: Option<usize>) {
    let (start, join) = nodes;
    let mut end = nested_arguments(cfg, arm, source, start);
    if cfg.keyed_exceptions {
        if let Some(call) = throwing_call(arm, source) { end = cfg.fork_exception(end, call, line(arm)); }
    }
    super::super::record_evaluation(cfg, end, arm);
    if let Some(call) = direct_call(arm, source) { cfg.emit(end, call, line(arm)); }
    if let Some(return_start) = return_start {
        if let Some(evaluation) = cfg.nodes[end].evaluations.last_mut() { evaluation.returned = true; }
        cfg.nodes[end].return_start = Some(return_start);
        cfg.emit(end, Stmt::ReturnIdentity(keyed_syntax::result_operand(arm, source, false)), line(arm));
        cfg.terminate(end, Stmt::Return { value: None }, line(arm));
    } else {
        cfg.edges.push((end, join, EdgeKind::Fallthrough));
    }
}

fn simple_arm(arm: Node<'_>, source: &[u8]) -> bool {
    matches!(arm.kind(), "null" | "true" | "false" | "number" | "string")
        || (matches!(arm.kind(), "call_expression" | "method_invocation")
            && site::capture(arm, source).is_some_and(|site| site.arguments.iter().all(|argument|
                argument.as_ref().is_some_and(|value| !matches!(value, IdentityOperand::CallResult { .. })))))
}

fn arm_value(mut node: Node<'_>) -> Option<Node<'_>> {
    while node.kind() == "parenthesized_expression" { node = node.named_child(0)?; }
    Some(node)
}

fn simple_test(node: Node<'_>, source: &[u8]) -> bool {
    let mut pending = vec![node];
    let mut remaining = 256;
    while let Some(node) = pending.pop() {
        if remaining == 0 { return false; }
        remaining -= 1;
        let Some(children) = test_children(node, source) else { return false; };
        pending.extend(children);
    }
    true
}

fn test_children<'a>(node: Node<'a>, source: &[u8]) -> Option<Vec<Node<'a>>> {
    match node.kind() {
        "identifier" | "true" | "false" => Some(Vec::new()),
        "parenthesized_expression" => Some(vec![node.named_child(0)?]),
        "unary_expression" if operator(node, source) == Some("!") => Some(vec![node.child_by_field_name("argument")?]),
        "binary_expression" if matches!(operator(node, source), Some("&&" | "||")) => {
            Some(vec![node.child_by_field_name("left")?, node.child_by_field_name("right")?])
        }
        _ => None,
    }
}

fn operator<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name("operator")?.utf8_text(source).ok()
}
