use tree_sitter::{Node, Tree};

pub(super) fn go_functions(tree: &Tree) -> Vec<Node<'_>> {
    let root = tree.root_node();
    root.named_children(&mut root.walk())
        .filter(|node| matches!(node.kind(), "function_declaration" | "method_declaration"))
        .collect()
}

pub(super) fn java_functions(tree: &Tree) -> Vec<Node<'_>> {
    descendants_of_kinds(tree, &["method_declaration", "constructor_declaration"])
}

/// Kotlin and JavaScript-family function declarations can be nested.
pub(super) fn function_declarations(tree: &Tree) -> Vec<Node<'_>> {
    descendants_of_kinds(tree, &["function_declaration"])
}

pub(super) fn rust_functions(tree: &Tree) -> Vec<Node<'_>> {
    descendants_of_kinds(tree, &["function_item"])
        .into_iter()
        .filter(|item| !item.has_error() && item.child_by_field_name("body").is_some())
        .collect()
}

fn descendants_of_kinds<'tree>(tree: &'tree Tree, kinds: &[&str]) -> Vec<Node<'tree>> {
    fn visit<'tree>(node: Node<'tree>, kinds: &[&str], functions: &mut Vec<Node<'tree>>) {
        if kinds.contains(&node.kind()) {
            functions.push(node);
        }
        for child in node.children(&mut node.walk()) {
            visit(child, kinds, functions);
        }
    }
    let mut functions = Vec::new();
    visit(tree.root_node(), kinds, &mut functions);
    functions
}
