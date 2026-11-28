use super::{branch_value, conditional, destructuring, let_else, match_flow, Node};

/// Whether this function's body can be lowered from syntax alone.
pub fn supports_function(function: Node<'_>) -> bool {
    supports_function_with_source(function, &[])
}

/// Whether this function's body can be lowered using its source text.
pub fn supports_function_with_source(function: Node<'_>, source: &[u8]) -> bool {
    function.child_by_field_name("body").is_some_and(|body| supported(body, source))
}

pub(super) fn supported(node: Node<'_>, source: &[u8]) -> bool {
    if matches!(
        node.kind(),
        "function_item" | "closure_expression" | "async_block" | "macro_invocation"
    ) {
        return true;
    }
    supported_node(node, source) && node.named_children(&mut node.walk()).all(|child| supported(child, source))
}

fn supported_node(node: Node<'_>, source: &[u8]) -> bool {
    match node.kind() {
        "label" | "unsafe_block" => false,
        "let_chain" | "let_condition" => conditional::supported(node, source),
        "let_declaration" => supported_declaration(node, source),
        "for_expression"
        | "if_expression"
        | "while_expression"
        | "loop_expression"
        | "return_expression"
        | "break_expression"
        | "continue_expression" => supported_statement(node, source),
        "match_expression" => supports_match_with_source(node, source),
        _ => true,
    }
}

fn supported_statement(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() == "if_expression" && branch_value::supports_value_if(node, source) {
        return true;
    }
    statement_position(node)
        && match node.kind() {
            "for_expression" => destructuring::iterator_bindings(node, source).is_some(),
            "if_expression" | "while_expression" => conditional::supports_owner(node),
            _ => true,
        }
}

fn supported_declaration(node: Node<'_>, source: &[u8]) -> bool {
    simple_binding(node)
        || let_else::supports(node, source)
        || destructuring::declaration_bindings(node, source).is_some()
}

pub(crate) fn lexical_bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    match node.kind() {
        "let_condition" => conditional::bindings(node, source),
        "let_declaration" => {
            let_else::bindings(node, source).or_else(|| destructuring::declaration_bindings(node, source))
        }
        "for_expression" => destructuring::iterator_bindings(node, source),
        "match_arm" => node
            .parent()
            .and_then(|body| body.parent())
            .filter(|matched| supports_match_with_source(*matched, source))
            .map(|_| match_arm_bindings_with_source(node, source)),
        _ => None,
    }
}

/// Whether the match can be represented using syntax alone.
pub fn supports_match(node: Node<'_>) -> bool {
    supports_match_with_source(node, &[])
}

/// Whether the match can be represented using its source text.
pub fn supports_match_with_source(node: Node<'_>, source: &[u8]) -> bool {
    match_flow::supports(node, source)
}

pub(crate) fn discards_match(node: Node<'_>, source: &[u8]) -> bool {
    supports_match_with_source(node, source) && branch_value::is_discarded_statement(node)
}

/// Simple arm-local bindings in a supported Rust match pattern.
pub fn match_arm_bindings(arm: Node<'_>) -> Vec<Node<'_>> {
    match_arm_bindings_with_source(arm, &[])
}

/// Arm-local bindings including alternatives verified with source text.
pub fn match_arm_bindings_with_source<'tree>(arm: Node<'tree>, source: &[u8]) -> Vec<Node<'tree>> {
    match_flow::arm_bindings(arm, source)
}

/// Lines where a supported match projects one or more arm-local values.
pub fn match_binding_lines(function: Node<'_>) -> Vec<u32> {
    match_binding_lines_with_source(function, &[])
}

/// Lines where a match supported with source text projects arm-local values.
pub fn match_binding_lines_with_source(function: Node<'_>, source: &[u8]) -> Vec<u32> {
    binding_sites(function, |node| {
        match_flow::has_bindings(node, source).then_some(())
    })
    .into_iter()
    .map(|(_, line)| line)
    .collect()
}

pub(crate) fn pattern_projection_sites(function: Node<'_>, source: &[u8], tracks_results: bool) -> Vec<(&'static str, u32)> {
    binding_sites(function, |node| {
        if tracks_results && super::stages_aggregate(node) {
            Some("rust_aggregate_element_projection")
        } else {
            projection_reason(node, source)
        }
    })
}

fn projection_reason(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    match node.kind() {
        "match_expression" => {
            match_flow::has_bindings(node, source).then_some("rust_match_binding_projection")
        }
        "let_condition" => conditional::bindings(node, source)
            .is_some_and(|names| !names.is_empty())
            .then_some("rust_conditional_binding_projection"),
        "let_declaration" => declaration_projection(node, source),
        "for_expression" => {
            destructuring::projected_bindings(node, source).map(|_| "rust_destructuring_binding_projection")
        }
        _ => None,
    }
}

fn declaration_projection(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    if let_else::bindings(node, source).is_some_and(|names| !names.is_empty()) {
        return Some("rust_let_else_binding_projection");
    }
    if destructuring::literal_tuple(node, source).is_some() {
        return None;
    }
    destructuring::declaration_bindings(node, source).map(|_| "rust_destructuring_binding_projection")
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
