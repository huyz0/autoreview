use super::{branch_value, conditional, destructuring, let_else, match_flow, Node};

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
        "let_declaration" => supported_declaration(node),
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
            "for_expression" => destructuring::iterator_bindings(node).is_some(),
            "if_expression" | "while_expression" => conditional::supports_owner(node),
            _ => true,
        }
}

fn supported_declaration(node: Node<'_>) -> bool {
    simple_binding(node)
        || let_else::supports(node)
        || destructuring::declaration_bindings(node).is_some()
}

pub(crate) fn lexical_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    match node.kind() {
        "let_condition" => conditional::bindings(node),
        "let_declaration" => {
            let_else::bindings(node).or_else(|| destructuring::declaration_bindings(node))
        }
        "for_expression" => destructuring::iterator_bindings(node),
        "match_arm" => node
            .parent()
            .and_then(|body| body.parent())
            .filter(|matched| supports_match(*matched))
            .map(|_| match_arm_bindings(node)),
        _ => None,
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
    binding_sites(function, |node| {
        match_flow::has_bindings(node).then_some(())
    })
    .into_iter()
    .map(|(_, line)| line)
    .collect()
}

pub(crate) fn pattern_projection_sites(function: Node<'_>) -> Vec<(&'static str, u32)> {
    binding_sites(function, projection_reason)
}

fn projection_reason(node: Node<'_>) -> Option<&'static str> {
    match node.kind() {
        "match_expression" => {
            match_flow::has_bindings(node).then_some("rust_match_binding_projection")
        }
        "let_condition" => conditional::bindings(node)
            .is_some_and(|names| !names.is_empty())
            .then_some("rust_conditional_binding_projection"),
        "let_declaration" => declaration_projection(node),
        "for_expression" => {
            destructuring::projected_bindings(node).map(|_| "rust_destructuring_binding_projection")
        }
        _ => None,
    }
}

fn declaration_projection(node: Node<'_>) -> Option<&'static str> {
    if let_else::bindings(node).is_some_and(|names| !names.is_empty()) {
        return Some("rust_let_else_binding_projection");
    }
    destructuring::declaration_bindings(node).map(|_| "rust_destructuring_binding_projection")
}

fn binding_sites<Tag: Ord>(
    function: Node<'_>,
    describe: impl Fn(Node<'_>) -> Option<Tag>,
) -> Vec<(Tag, u32)> {
    let mut pending: Vec<_> = function.child_by_field_name("body").into_iter().collect();
    let mut sites = Vec::new();
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "function_item" | "closure_expression" | "async_block" | "macro_invocation"
        ) {
            continue;
        }
        if let Some(reason) = describe(node) {
            sites.push((reason, node.start_position().row as u32 + 1));
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    sites.sort_unstable();
    sites.dedup();
    sites
}

fn statement_position(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| match parent.kind() {
        "block" | "expression_statement" | "else_clause" => true,
        "match_arm" => {
            matches!(node.kind(), "return_expression" | "break_expression" | "continue_expression")
                && parent.child_by_field_name("value") == Some(node)
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
