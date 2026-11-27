use std::path::Path;

use autoreview_langsupport::{language_for_path, Language};
use tree_sitter::Node;

use super::web::{field_text, first_argument, is_function, BindingScopes};
use super::{descendants, location, text, AgentFinding, Rule};

const BASE: Rule = Rule {
    id: "", category: "correctness", subcategories: &["correctness.promise-use"],
    title: "Promise passed to a typed array constructor",
    message: "Web Crypto digest returns a promise. Await the digest before constructing the typed array.",
    reference: "https://www.w3.org/TR/WebCryptoAPI/#SubtleCrypto-method-digest",
};
pub(super) const RULES: &[Rule] = &[
    Rule { id: "javascript-webcrypto-digest-promise-as-array", ..BASE },
    Rule { id: "typescript-webcrypto-digest-promise-as-array", ..BASE },
];

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    if !source.contains("digest") || !source.contains("Uint8Array") || root.has_error() { return Vec::new(); }
    let rule = match language_for_path(Path::new(path)) {
        Some(Language::JavaScript) => &RULES[0],
        Some(Language::TypeScript | Language::Tsx) => &RULES[1],
        _ => return Vec::new(),
    };
    let scopes = BindingScopes::new(root, source);
    if globals_modified(root, source) { return Vec::new(); }
    descendants(root).filter_map(|node| {
        if node.kind() != "new_expression" || field_text(node, "constructor", source) != Some("Uint8Array")
            || scopes.visible("Uint8Array", node).is_some() || scopes.visible("crypto", node).is_some() { return None; }
        let argument = first_argument(node)?;
        let digest = if digest_call(argument, source) { argument } else { local_digest(argument, source)? };
        if scopes.visible("crypto", digest).is_some() { return None; }
        Some(rule.finding(location(path, node, source), vec![location(path, digest, source)]))
    }).collect()
}

fn digest_call(node: Node<'_>, source: &str) -> bool {
    node.kind() == "call_expression" && field_text(node, "function", source) == Some("crypto.subtle.digest")
}

fn local_digest<'t>(argument: Node<'t>, source: &str) -> Option<Node<'t>> {
    if argument.kind() != "identifier" { return None; }
    let function = std::iter::successors(argument.parent(), |node| node.parent()).find(|node| is_function(*node))?;
    let name = text(argument, source);
    let occurrences: Vec<_> = descendants(function).filter(|node| node.kind() == "identifier" && text(*node, source) == name).collect();
    if occurrences.len() != 2 { return None; }
    let binding = occurrences.into_iter().find(|node| *node != argument)?;
    let declaration = binding.parent().filter(|node| node.kind() == "variable_declarator" && node.child_by_field_name("name") == Some(binding))?;
    let statement = declaration.parent().filter(|node| node.kind() == "lexical_declaration")?;
    if !text(statement, source).trim_start().starts_with("const ") || declaration.start_byte() >= argument.start_byte() { return None; }
    let initializer = declaration.child_by_field_name("value")?;
    digest_call(initializer, source).then_some(initializer)
}

// Treat explicit writes or imports of the global names as ambiguous.
fn globals_modified(root: Node<'_>, source: &str) -> bool {
    descendants(root).any(|node| match node.kind() {
        "import_statement" => descendants(node).any(|child| child.kind() == "identifier" && matches!(text(child, source), "crypto" | "Uint8Array")),
        "assignment_expression" | "augmented_assignment_expression" | "update_expression" => {
            let target = node.child_by_field_name("left").or_else(|| node.child_by_field_name("argument"));
            target.is_some_and(|target| descendants(target).any(|child| matches!(text(child, source), "crypto" | "Uint8Array")))
        }
        _ => false,
    })
}
