use std::collections::HashSet;

use autoreview_dataflow::cfg::{Cfg, Stmt};
use autoreview_dataflow::lower;
use autoreview_langsupport::Language;
use tree_sitter::{Node, Tree};

use super::cache::ParsedFile;
use super::dispatch::profile;

pub(super) type LoweredFile<'tree> = (usize, Vec<(Node<'tree>, Cfg<Stmt>)>);

pub(super) fn lower_keyed_graphs(language: Language, parsed: &ParsedFile, tracks_results: bool) -> LoweredFile<'_> {
    let (extract, _) = profile(language);
    let tree = &parsed.1;
    let mut nodes = extract(tree);
    let web = matches!(language, Language::JavaScript | Language::TypeScript | Language::Tsx);
    if web {
        nodes.extend(web_named_functions(tree));
    }
    let ordinary = nodes.len();
    if tracks_results {
        if language == Language::Go {
            nodes.extend(go_initializer_functions(tree));
        } else if web {
            let mut seen: HashSet<_> = nodes.iter().map(Node::id).collect();
            nodes.extend(web_result_functions(tree).into_iter().filter(|node| seen.insert(node.id())));
        }
    }
    let graphs = nodes.into_iter()
        .map(|node| (node, lower::keyed_variant(language, parsed.0.as_bytes(), node)))
        .collect();
    (ordinary, graphs)
}

pub(super) fn go_functions(tree: &Tree) -> Vec<Node<'_>> {
    let root = tree.root_node();
    root.named_children(&mut root.walk())
        .filter(|node| matches!(node.kind(), "function_declaration" | "method_declaration"))
        .collect()
}

fn go_initializer_functions(tree: &Tree) -> Vec<Node<'_>> {
    fn collect<'tree>(node: Node<'tree>, functions: &mut Vec<Node<'tree>>) {
        if node.kind() == "func_literal" {
            functions.push(node);
            return;
        }
        for child in node.named_children(&mut node.walk()) {
            collect(child, functions);
        }
    }

    let root = tree.root_node();
    let mut functions = Vec::new();
    for declaration in root.named_children(&mut root.walk()) {
        if declaration.kind() == "var_declaration" {
            collect(declaration, &mut functions);
        }
    }
    functions
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
