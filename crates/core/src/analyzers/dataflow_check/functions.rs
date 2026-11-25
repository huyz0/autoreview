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

pub(super) fn web_result_functions(tree: &Tree) -> Vec<Node<'_>> {
    descendants_of_kinds(tree, &["arrow_function", "function_expression", "method_definition"])
        .into_iter()
        .filter(|node| node.child_by_field_name("body").is_some_and(|body| body.kind() == "statement_block"))
        .collect()
}

pub(super) fn web_named_functions(tree: &Tree) -> Vec<Node<'_>> {
    descendants_of_kinds(tree, &["arrow_function", "function_expression"])
        .into_iter()
        .filter(|node| {
            if let Some(export) = node.parent().filter(|parent| parent.kind() == "export_statement") {
                return export.children(&mut export.walk()).any(|child| child.kind() == "default");
            }
            let Some(binding) = node.parent().filter(|parent| parent.kind() == "variable_declarator") else { return false };
            let Some(declaration) = binding.parent().filter(|parent| parent.kind() == "lexical_declaration") else { return false };
            binding.child_by_field_name("value") == Some(*node)
                && declaration.children(&mut declaration.walk()).any(|child| child.kind() == "const")
                && declaration.parent().is_some_and(|parent| matches!(parent.kind(), "program" | "export_statement"))
        })
        .collect()
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
