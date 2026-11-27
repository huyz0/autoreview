use tree_sitter::Node;

use super::node_calls::{string_literal, NodeCalls};
use super::string_flow::meaningful_children;

pub(in crate::analyzers::framework_checks) struct InlineRoute<'tree, 'source> {
    pub method: &'source str,
    pub path: &'source str,
    pub handler: Node<'tree>,
}

pub(in crate::analyzers::framework_checks) fn inline_route<'tree, 'source>(
    route: Node<'tree>,
    calls: &NodeCalls<'tree, 'source>,
    source: &'source str,
) -> Option<InlineRoute<'tree, 'source>> {
    let (receiver, method) = calls.member(route.child_by_field_name("function")?)?;
    if route.kind() != "call_expression"
        || !matches!(method, "get" | "post" | "put" | "patch" | "delete" | "all")
    {
        return None;
    }
    let app = calls.const_initializer(receiver)?;
    if calls.resolve(app, &["express"]) != Some(("express", "default"))
        || app.kind() != "call_expression"
    {
        return None;
    }
    let mut args = meaningful_children(route.child_by_field_name("arguments")?);
    let path = string_literal(args.next()?, source)?;
    let handler = args.next()?;
    (args.next().is_none() && matches!(handler.kind(), "arrow_function" | "function_expression"))
        .then_some(InlineRoute {
            method,
            path,
            handler,
        })
}
