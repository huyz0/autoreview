use tree_sitter::Node;

use super::node_calls::NodeCalls;
use crate::analyzers::framework_checks::children;

pub(in crate::analyzers::framework_checks) fn origin<'t>(
    calls: &NodeCalls<'t, '_>,
    expression: Node<'t>,
    fuel: usize,
    direct: &impl Fn(Node<'t>, usize) -> Option<Node<'t>>,
) -> Option<Node<'t>> {
    let remaining = fuel.checked_sub(1)?;
    let trace = |part| origin(calls, part, remaining, direct);
    match expression.kind() {
        "identifier" => trace(calls.const_initializer(expression)?),
        "parenthesized_expression" | "template_substitution" => {
            trace(meaningful_children(expression).next()?)
        }
        "template_string" => meaningful_children(expression)
            .filter(|part| part.kind() == "template_substitution")
            .find_map(trace),
        "binary_expression" if expression.child_by_field_name("operator")?.kind() == "+" => {
            ["left", "right"]
                .iter()
                .filter_map(|field| expression.child_by_field_name(*field))
                .find_map(trace)
        }
        _ => direct(expression, remaining),
    }
}

pub(in crate::analyzers::framework_checks) fn meaningful_children(
    node: Node<'_>,
) -> impl DoubleEndedIterator<Item = Node<'_>> {
    children(node).filter(|part| part.kind() != "comment")
}
