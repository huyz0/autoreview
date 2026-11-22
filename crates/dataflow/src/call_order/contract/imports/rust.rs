use std::collections::HashSet;

use tree_sitter::Node;

use super::{add_import, children, qualified_names, text, Names, ResultContract};

pub(in crate::call_order::contract) fn rust_names(
    function: Node<'_>,
    source: &[u8],
    contract: &ResultContract,
) -> Names {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let scopes = function
        .child_by_field_name("body")
        .into_iter()
        .chain(std::iter::successors(function.parent(), Node::parent))
        .take_while(|node| node.kind() != "mod_item")
        .filter(|node| matches!(node.kind(), "source_file" | "block" | "declaration_list"));
    for scope in scopes {
        let mut imports = Vec::new();
        for item in children(scope) {
            if let Some(usage) = item
                .child_by_field_name("argument")
                .filter(|_| item.kind() == "use_declaration")
            {
                expand_use(&mut imports, "", usage, source);
            } else if let Some(name) = item
                .child_by_field_name("name")
                .and_then(|part| text(part, source))
            {
                seen.insert(name.to_owned());
            }
        }
        for (path, alias) in imports {
            if seen.insert(alias.to_owned()) {
                add_import(&mut names, contract, &path, alias, "::");
            }
        }
    }
    let root = contract
        .import
        .split("::")
        .next()
        .unwrap_or(&contract.import);
    if !seen.contains(root) {
        names.extend(qualified_names(
            vec![contract.import.clone()],
            "::",
            contract,
        ));
    }
    names
}

type Imports<'a> = Vec<(String, &'a str)>;

fn expand_use<'a>(imports: &mut Imports<'a>, prefix: &str, node: Node<'_>, source: &'a [u8]) {
    match node.kind() {
        "scoped_use_list" => {
            let Some(path) = node
                .child_by_field_name("path")
                .and_then(|part| text(part, source))
            else {
                return;
            };
            if let Some(list) = node.child_by_field_name("list") {
                expand_use(imports, &format!("{prefix}{path}::"), list, source);
            }
        }
        "use_list" => children(node).for_each(|part| expand_use(imports, prefix, part, source)),
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .and_then(|part| text(part, source));
            let alias = node
                .child_by_field_name("alias")
                .and_then(|part| text(part, source));
            if let Some((path, alias)) = path.zip(alias) {
                imports.push((format!("{prefix}{path}"), alias));
            }
        }
        "identifier" | "scoped_identifier" => {
            if let Some(path) = text(node, source) {
                let (_, alias) = path.rsplit_once("::").unwrap_or(("", path));
                imports.push((format!("{prefix}{path}"), alias));
            }
        }
        _ => {}
    }
}
