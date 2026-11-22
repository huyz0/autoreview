mod interval;

use std::collections::HashSet;
use std::path::Path;

use autoreview_langsupport::{language_for_path, parser_for, Language};
use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{children, descendants, location, parse_source, text, Rule};

pub(super) const INTERVAL_RULE: Rule = Rule {
    id: "react-use-effect-interval-not-cleared",
    category: "correctness",
    subcategory: "frontend.effect-cleanup",
    title: "React effect leaves an interval running",
    message: "This effect starts an interval that remains active on a cleanup path. Clear the same interval in the returned cleanup function.",
    reference: "https://react.dev/reference/react/useEffect",
};

pub(super) fn run(root: &Path, paths: &[String]) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for path in paths {
        let absolute = root.join(path);
        let Some(language @ (Language::JavaScript | Language::TypeScript | Language::Tsx)) =
            language_for_path(&absolute)
        else {
            continue;
        };
        let Some(mut parser) = parser_for(language) else { continue };
        let Some((source, tree)) = parse_source(&mut parser, &absolute) else { continue };
        let root = tree.root_node();
        let hooks = imported_hooks(root, &source);
        if hooks.is_empty() || timer_bindings_shadowed(root, &source) {
            continue;
        }
        for call in descendants(root).filter(|node| node.kind() == "call_expression") {
            let Some(function) = call.child_by_field_name("function") else { continue };
            if !hooks.contains(&text(function, &source)) {
                continue;
            }
            let Some(body) = first_argument(call).and_then(callback_body) else { continue };
            for (acquire, exit) in interval::findings(body, &source) {
                findings.push(INTERVAL_RULE.finding(
                    location(path, exit, &source), vec![location(path, acquire, &source)],
                ));
            }
        }
    }
    findings
}

fn imported_hooks<'s>(root: Node<'_>, source: &'s str) -> HashSet<&'s str> {
    let imports = children(root).filter(|node| {
        node.kind() == "import_statement"
            && node.child_by_field_name("source")
                .is_some_and(|module| text(module, source).trim_matches(['\'', '"']) == "react")
    });
    imports.flat_map(descendants)
        .filter(|node| node.kind() == "import_specifier")
        .filter_map(|node| {
            let imported = node.child_by_field_name("name")?;
            (text(imported, source) == "useEffect")
                .then(|| text(node.child_by_field_name("alias").unwrap_or(imported), source))
        })
        .filter(|hook| !shadowed(root, source, hook))
        .collect()
}

fn timer_bindings_shadowed(root: Node<'_>, source: &str) -> bool {
    ["setInterval", "clearInterval"].into_iter().any(|name| {
        shadowed(root, source, name)
            || children(root).filter(|part| part.kind() == "import_statement")
                .flat_map(descendants)
                .any(|node| node.kind() == "identifier" && text(node, source) == name)
    })
}

fn shadowed(root: Node<'_>, source: &str, name: &str) -> bool {
    descendants(root).filter_map(|node| match node.kind() {
        "variable_declarator" | "function_declaration" | "class_declaration" => node.child_by_field_name("name"),
        "required_parameter" | "optional_parameter" | "catch_clause" => node.child_by_field_name("pattern").or_else(|| node.child_by_field_name("parameter")),
        "formal_parameters" => Some(node),
        "arrow_function" => node.child_by_field_name("parameter"),
        _ => None,
    }).any(|binding| descendants(binding).any(|node| {
        matches!(node.kind(), "identifier" | "shorthand_property_identifier_pattern")
            && text(node, source) == name
    }))
}

fn first_argument(call: Node<'_>) -> Option<Node<'_>> {
    children(call.child_by_field_name("arguments")?).find(|node| node.kind() != "comment")
}

fn callback_body(node: Node<'_>) -> Option<Node<'_>> {
    if !matches!(node.kind(), "arrow_function" | "function_expression")
        || node.child(0).is_some_and(|part| part.kind() == "async")
    {
        return None;
    }
    node.child_by_field_name("body")
}
