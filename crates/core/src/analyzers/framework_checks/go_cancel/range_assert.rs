use std::collections::{BTreeMap, BTreeSet};

use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::package::File;
use super::super::{children, descendants, location, text, Rule};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "go-testify-range-element-self-comparison",
    category: "correctness",
    subcategories: &["testing.assertions"],
    title: "Assertion compares a range value with its source element",
    message: "The range value and indexed slice element come from the same iteration. Compare against an independent expected value so the assertion can detect a wrong result.",
    reference: "https://go.dev/ref/spec#For_statements",
};

pub(super) fn findings(file: &File, peers: &[&File]) -> Vec<AgentFinding> {
    let imports = testify_imports(file);
    let types = slice_types(peers);
    descendants(file.root()).filter(|node| node.kind() == "for_statement")
        .flat_map(|node| inspect_loop(file, node, &imports, &types)).collect()
}

fn inspect_loop(file: &File, loop_node: Node<'_>, imports: &BTreeSet<String>, types: &BTreeSet<String>) -> Vec<AgentFinding> {
    let Some((names, owner, body)) = range_parts(loop_node, &file.source) else { return Vec::new() };
    if !local_slice(owner, loop_node, names[2], types, &file.source) || unsafe_loop(body, &names, &file.source) {
        return Vec::new();
    }
    descendants(body).filter(|node| node.kind() == "call_expression").filter_map(|call| {
        let receiver = assertion_receiver(call, &names, &file.source)?;
        (imports.contains(receiver) && !shadowed(owner, receiver, &file.source))
            .then(|| RULE.finding(location(&file.path, call, &file.source), Vec::new()))
    }).collect()
}

fn range_parts<'tree, 'src>(loop_node: Node<'tree>, source: &'src str) -> Option<([&'src str; 3], Node<'tree>, Node<'tree>)> {
    let range = children(loop_node).find(|node| node.kind() == "range_clause")?;
    if !text(range, source).contains(":=") { return None; }
    let left = range.child_by_field_name("left")?;
    let vars: Vec<_> = children(left).filter(|node| node.kind() == "identifier").collect();
    let [index, value] = vars.as_slice() else { return None };
    let collection = range.child_by_field_name("right").filter(|node| node.kind() == "identifier")?;
    let names = [text(*index, source), text(*value, source), text(collection, source)];
    if names.iter().any(|name| *name == "_" || name.is_empty()) { return None; }
    let owner = std::iter::successors(loop_node.parent(), |node| node.parent())
        .find(|node| matches!(node.kind(), "function_declaration" | "method_declaration" | "func_literal"))?;
    Some((names, owner, loop_node.child_by_field_name("body")?))
}

fn testify_imports(file: &File) -> BTreeSet<String> {
    descendants(file.root()).filter(|node| node.kind() == "import_spec").filter_map(|node| {
        let path = text(node.child_by_field_name("path")?, &file.source).trim_matches(['"', '`']);
        let default = match path {
            "github.com/stretchr/testify/assert" => "assert",
            "github.com/stretchr/testify/require" => "require",
            _ => return None,
        };
        let name = node.child_by_field_name("name").map_or(default, |name| text(name, &file.source));
        (!matches!(name, "_" | ".")).then(|| name.to_owned())
    }).collect()
}

fn slice_types(peers: &[&File]) -> BTreeSet<String> {
    let mut types = BTreeMap::new();
    for file in peers {
        for node in descendants(file.root()).filter(|node| node.kind() == "type_spec") {
            let Some(name) = node.child_by_field_name("name") else { continue };
            let Some(kind) = node.child_by_field_name("type") else { continue };
            let name = text(name, &file.source).to_owned();
            let is_slice = kind.kind() == "slice_type";
            types.entry(name).and_modify(|valid| *valid = false).or_insert(is_slice);
        }
    }
    types.into_iter().filter_map(|(name, valid)| valid.then_some(name)).collect()
}

fn local_slice(owner: Node<'_>, loop_node: Node<'_>, collection: &str, types: &BTreeSet<String>, source: &str) -> bool {
    let declarations: Vec<_> = descendants(owner).filter(|node| node.kind() == "var_spec")
        .filter(|node| node.children_by_field_name("name", &mut node.walk()).any(|name| text(name, source) == collection))
        .collect();
    let [declaration] = declarations.as_slice() else { return false };
    if declaration.start_byte() >= loop_node.start_byte() || collection_rebound(owner, collection, source) { return false; }
    declaration.child_by_field_name("type").is_some_and(|kind| kind.kind() == "slice_type"
        || (kind.kind() == "type_identifier" && types.contains(text(kind, source))
            && !descendants(owner).any(|node| node.kind() == "type_spec"
                && node.child_by_field_name("name").is_some_and(|name| text(name, source) == text(kind, source)))))
}

fn collection_rebound(owner: Node<'_>, collection: &str, source: &str) -> bool {
    descendants(owner).any(|node| match node.kind() {
        "parameter_declaration" => node.children_by_field_name("name", &mut node.walk())
            .any(|name| text(name, source) == collection),
        "short_var_declaration" | "assignment_statement" | "range_clause" => node.child_by_field_name("left")
            .is_some_and(|left| descendants(left).any(|part| part.kind() == "identifier" && text(part, source) == collection)),
        _ => false,
    })
}

fn assertion_receiver<'s>(call: Node<'_>, names: &[&str; 3], source: &'s str) -> Option<&'s str> {
    let function = call.child_by_field_name("function").filter(|node| node.kind() == "selector_expression")?;
    if text(function.child_by_field_name("field")?, source) != "Equal" { return None; }
    let receiver = function.child_by_field_name("operand").filter(|node| node.kind() == "identifier")?;
    let args: Vec<_> = children(call.child_by_field_name("arguments")?).collect();
    if args.len() < 3 { return None; }
    let matches = |value: Node<'_>, indexed: Node<'_>| {
        value.kind() == "identifier" && text(value, source) == names[1] && indexed.kind() == "index_expression"
            && indexed.child_by_field_name("operand").is_some_and(|part| part.kind() == "identifier" && text(part, source) == names[2])
            && indexed.child_by_field_name("index").is_some_and(|part| part.kind() == "identifier" && text(part, source) == names[0])
    };
    (matches(args[1], args[2]) || matches(args[2], args[1])).then(|| text(receiver, source))
}

fn shadowed(owner: Node<'_>, name: &str, source: &str) -> bool {
    descendants(owner).any(|node| match node.kind() {
        "parameter_declaration" | "var_spec" => node.children_by_field_name("name", &mut node.walk()).any(|part| text(part, source) == name),
        "short_var_declaration" | "assignment_statement" => node.child_by_field_name("left").is_some_and(|left|
            descendants(left).any(|part| part.kind() == "identifier" && text(part, source) == name)),
        _ => false,
    })
}

fn unsafe_loop(body: Node<'_>, names: &[&str; 3], source: &str) -> bool {
    descendants(body).any(|node| {
        if matches!(node.kind(), "go_statement" | "defer_statement" | "func_literal") { return true; }
        if node.kind() == "identifier" && text(node, source) == names[2]
            && !node.parent().is_some_and(|parent| parent.kind() == "index_expression"
                && parent.child_by_field_name("operand") == Some(node)) { return true; }
        if matches!(node.kind(), "assignment_statement" | "short_var_declaration" | "inc_statement" | "dec_statement" | "var_spec" | "range_clause") {
            let left = node.child_by_field_name("left").unwrap_or(node);
            return descendants(left).any(|part| part.kind() == "identifier" && names.contains(&text(part, source)));
        }
        if node.kind() == "call_expression" {
            return node.child_by_field_name("arguments").is_some_and(|arguments| children(arguments).any(|argument| {
                (argument.kind() == "identifier" && text(argument, source) == names[2])
                    || (argument.kind() == "unary_expression" && text(argument, source).starts_with('&')
                        && descendants(argument).any(|part| part.kind() == "identifier" && names.contains(&text(part, source))))
            }));
        }
        false
    })
}
