mod rust;

pub(super) use rust::rust_names;

use tree_sitter::Node;

use crate::lower::site::children;

use super::{qualified_names, ResultContract};

type Names = Vec<String>;

pub(super) fn kotlin_names(root: Node<'_>, source: &[u8], contract: &ResultContract) -> Names {
    let mut names = qualified_names(vec![contract.import.clone()], ".", contract);
    for directive in children(root).filter(|node| node.kind() == "import") {
        let Some(path) = children(directive)
            .next()
            .and_then(|node| text(node, source))
        else {
            continue;
        };
        let alias = children(directive)
            .nth(1)
            .and_then(|node| text(node, source));
        let (_, member) = path.rsplit_once('.').unwrap_or(("", path));
        let alias = alias.unwrap_or(member);
        add_import(&mut names, contract, path, alias, ".");
    }
    names.retain(|call| call.contains('.') || !root_declares(root, source, call));
    names
}

fn root_declares(root: Node<'_>, source: &[u8], name: &str) -> bool {
    children(root).any(|node| {
        node.child_by_field_name("name")
            .and_then(|part| text(part, source))
            == Some(name)
    })
}

pub(super) fn web_names(root: Node<'_>, source: &[u8], contract: &ResultContract) -> Names {
    let mut names = Vec::new();
    for import in children(root).filter(|node| node.kind() == "import_statement") {
        let Some(path) = import
            .child_by_field_name("source")
            .and_then(|node| text(node, source))
        else {
            continue;
        };
        if !web_module_matches(path, &contract.import) || type_only(import) {
            continue;
        }
        if let Some(clause) = children(import).find(|node| node.kind() == "import_clause") {
            add_web_bindings(&mut names, contract, clause, source);
        }
    }
    names
}

fn web_module_matches(path: &str, module: &str) -> bool {
    let path = path.trim_matches(['"', '\'']);
    path == module || module.strip_prefix("node:") == Some(path)
}

fn add_web_bindings(names: &mut Names, contract: &ResultContract, clause: Node<'_>, source: &[u8]) {
    for binding in children(clause) {
        match binding.kind() {
            "namespace_import" => {
                add_web_namespace(names, contract, binding.named_child(0), source)
            }
            "identifier" if contract.import.starts_with("node:") => {
                add_web_namespace(names, contract, Some(binding), source);
            }
            "named_imports" => {
                names.extend(children(binding).filter_map(|spec| web_alias(spec, source, contract)))
            }
            _ => {}
        }
    }
}

fn add_web_namespace(
    names: &mut Names,
    contract: &ResultContract,
    binding: Option<Node<'_>>,
    source: &[u8],
) {
    if let Some(alias) = binding.and_then(|node| text(node, source)) {
        names.extend(qualified_names(vec![alias.to_owned()], ".", contract));
    }
}

fn web_alias(spec: Node<'_>, source: &[u8], contract: &ResultContract) -> Option<String> {
    if type_only(spec) {
        return None;
    }
    let member = spec
        .child_by_field_name("name")
        .and_then(|node| text(node, source))?;
    if !contract.methods.iter().any(|name| name == member) {
        return None;
    }
    let alias = spec
        .child_by_field_name("alias")
        .and_then(|node| text(node, source));
    Some(alias.unwrap_or(member).to_owned())
}

fn type_only(node: Node<'_>) -> bool {
    node.children(&mut node.walk())
        .any(|part| part.kind() == "type")
}

fn add_import(names: &mut Names, contract: &ResultContract, path: &str, alias: &str, sep: &str) {
    let clean: String = path.chars().filter(|ch| !ch.is_whitespace()).collect();
    let path = clean.strip_prefix("::").unwrap_or(&clean);
    if path == contract.import {
        names.extend(qualified_names(vec![alias.to_owned()], sep, contract));
    } else if let Some(member) = path
        .strip_prefix(&contract.import)
        .and_then(|suffix| suffix.strip_prefix(sep))
    {
        if contract.methods.iter().any(|method| method == member) {
            names.push(alias.to_owned());
        }
    }
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}
