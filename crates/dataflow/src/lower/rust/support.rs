use super::{branch_value, conditional, let_else, match_flow, Node};

/// Whether this function's body can be lowered without skipping it entirely.
pub fn supports_function(function: Node<'_>) -> bool {
    function.child_by_field_name("body").is_some_and(supported)
}

pub(super) fn supported(node: Node<'_>) -> bool {
    if matches!(
        node.kind(),
        "function_item" | "closure_expression" | "async_block" | "macro_invocation"
    ) {
        return true;
    }
    supported_node(node) && node.named_children(&mut node.walk()).all(supported)
}

fn supported_node(node: Node<'_>) -> bool {
    match node.kind() {
        "label" | "unsafe_block" => false,
        "let_chain" | "let_condition" => conditional::supported(node),
        "let_declaration" => simple_binding(node) || let_else::supports(node),
        "for_expression"
        | "if_expression"
        | "while_expression"
        | "loop_expression"
        | "return_expression"
        | "break_expression"
        | "continue_expression" => supported_statement(node),
        "match_expression" => supports_match(node),
        _ => true,
    }
}

fn supported_statement(node: Node<'_>) -> bool {
    if node.kind() == "if_expression" && branch_value::supports_value_if(node) {
        return true;
    }
    statement_position(node)
        && match node.kind() {
            "for_expression" => simple_binding(node),
            "if_expression" | "while_expression" => conditional::supports_owner(node),
            _ => true,
        }
}

/// Whether the match pattern, guard and arm flow can be represented.
pub fn supports_match(node: Node<'_>) -> bool {
    match_flow::supports(node)
}

pub(crate) fn discards_match(node: Node<'_>) -> bool {
    supports_match(node) && !branch_value::is_returned(node) && !branch_value::is_initializer(node)
}

/// Simple arm-local bindings in a supported Rust match pattern.
pub fn match_arm_bindings(arm: Node<'_>) -> Vec<Node<'_>> {
    match_flow::arm_bindings(arm)
}

/// Lines where a supported match projects one or more arm-local values.
pub fn match_binding_lines(function: Node<'_>) -> Vec<u32> {
    match_flow::binding_lines(function, match_flow::has_bindings)
}

pub(crate) use let_else::bindings as let_else_bindings;

pub(crate) fn pattern_projection_sites(function: Node<'_>) -> Vec<(&'static str, u32)> {
    let match_sites = match_binding_lines(function);
    let let_else_sites = match_flow::binding_lines(function, |node| {
        let_else_bindings(node).is_some_and(|names| !names.is_empty())
    });
    let conditional_sites = match_flow::binding_lines(function, |node| {
        conditional::bindings(node).is_some_and(|names| !names.is_empty())
    });
    match_sites
        .into_iter()
        .map(|line| ("rust_match_binding_projection", line))
        .chain(
            let_else_sites
                .into_iter()
                .map(|line| ("rust_let_else_binding_projection", line)),
        )
        .chain(
            conditional_sites
                .into_iter()
                .map(|line| ("rust_conditional_binding_projection", line)),
        )
        .collect()
}

fn statement_position(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| match parent.kind() {
        "block" | "expression_statement" | "else_clause" => true,
        "match_arm" => {
            node.kind() == "return_expression" && parent.child_by_field_name("value") == Some(node)
        }
        _ => false,
    })
}

pub(super) fn simple_binding(node: Node<'_>) -> bool {
    node.child_by_field_name("alternative").is_none()
        && node
            .child_by_field_name("pattern")
            .is_none_or(|pattern| matches!(pattern.kind(), "identifier" | "_"))
}
