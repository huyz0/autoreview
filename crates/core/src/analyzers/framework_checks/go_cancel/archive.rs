use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{
    super::{children, descendants, location, text, Rule},
    package,
};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "go-zip-entry-name-to-filesystem-write",
    category: "security",
    subcategories: &["filesystem.path-containment"],
    title: "ZIP entry name controls an extraction destination",
    message: "Joining the entry name to an output directory does not enforce containment. Use traversal-resistant rooted filesystem operations when extracting untrusted archives.",
    reference: "https://go.dev/blog/osroot",
};

pub(super) fn findings(file: &package::File) -> Vec<AgentFinding> {
    if !file.source.contains(".Name") && !file.source.contains("archive/zip") {
        return Vec::new();
    }
    let Some(imports) = Imports::read(file) else {
        return Vec::new();
    };
    file.functions()
        .filter(|function| {
            let names = package::local_names(*function, &file.source);
            [imports.zip, imports.filepath, imports.os]
                .iter()
                .all(|alias| !names.contains(alias))
        })
        .flat_map(|function| imports.check_function(file, function))
        .collect()
}

struct Imports<'s> {
    zip: &'s str,
    filepath: &'s str,
    os: &'s str,
}

impl<'s> Imports<'s> {
    fn read(file: &'s package::File) -> Option<Self> {
        Some(Self {
            zip: package::package_alias(file, "archive/zip", "zip")?,
            filepath: package::package_alias(file, "path/filepath", "filepath")?,
            os: package::package_alias(file, "os", "os")?,
        })
    }

    fn check_function(&self, file: &package::File, function: Node<'_>) -> Vec<AgentFinding> {
        let Some(params) = function.child_by_field_name("parameters") else {
            return Vec::new();
        };
        let Some(body) = function
            .child_by_field_name("body")
            .and_then(statement_list)
        else {
            return Vec::new();
        };
        let receivers: Vec<_> = children(params)
            .flat_map(|param| typed_receivers(param, self.zip, &file.source))
            .collect();
        children(body)
            .filter(|node| node.kind() == "for_statement")
            .filter_map(|loop_node| {
                let entry = range_entry(loop_node, &receivers, &file.source)?;
                let body = statement_list(loop_node.child_by_field_name("body")?)?;
                Some(
                    Extraction {
                        entry,
                        source: &file.source,
                        imports: self,
                    }
                    .writes(file, body),
                )
            })
            .flatten()
            .collect()
    }
}

struct Extraction<'a, 's> {
    entry: &'s str,
    source: &'s str,
    imports: &'a Imports<'s>,
}

impl Extraction<'_, '_> {
    fn writes(&self, file: &package::File, block: Node<'_>) -> Vec<AgentFinding> {
        children(block)
            .take_while(|statement| !self.replaces_entry(*statement))
            .filter(|statement| {
                matches!(
                    statement.kind(),
                    "expression_statement" | "short_var_declaration" | "assignment_statement"
                )
            })
            .flat_map(|statement| {
                direct_values(statement).filter_map(move |call| {
                    let write = package_call(call, self.imports.os, self.source)?;
                    if !matches!(write, "Create" | "WriteFile" | "Mkdir" | "MkdirAll") {
                        return None;
                    }
                    let (join, setup) = self.destination(arguments(call).next()?, statement)?;
                    let name = self.join_entry(join)?;
                    if previous_statement(setup)
                        .is_some_and(|guard| self.continue_unless_local(guard))
                    {
                        return None;
                    }
                    let mut finding = RULE.finding(
                        location(&file.path, call, self.source),
                        vec![location(&file.path, name, self.source)],
                    );
                    finding
                        .meta
                        .get_or_insert_with(Default::default)
                        .insert("cwe".into(), serde_json::json!(["CWE-22"]));
                    Some(finding)
                })
            })
            .collect()
    }

    fn destination<'t>(&self, argument: Node<'t>, write: Node<'t>) -> Option<(Node<'t>, Node<'t>)> {
        if argument.kind() == "call_expression" {
            return Some((argument, write));
        }
        let declaration =
            previous_statement(write).filter(|node| node.kind() == "short_var_declaration")?;
        let bound = only_child(declaration.child_by_field_name("left")?)?;
        if argument.kind() != "identifier"
            || bound.kind() != "identifier"
            || text(bound, self.source) != text(argument, self.source)
        {
            return None;
        }
        Some((
            only_child(declaration.child_by_field_name("right")?)?,
            declaration,
        ))
    }

    fn join_entry<'t>(&self, join: Node<'t>) -> Option<Node<'t>> {
        (package_call(join, self.imports.filepath, self.source)? == "Join").then_some(())?;
        let mut args = arguments(join);
        let root = args.next()?;
        let entry = args.next()?;
        (matches!(
            root.kind(),
            "identifier" | "interpreted_string_literal" | "raw_string_literal"
        ) && self.is_entry_name(entry)
            && args.next().is_none())
        .then_some(entry)
    }

    fn replaces_entry(&self, statement: Node<'_>) -> bool {
        descendants(statement)
            .filter(|node| node.kind() == "assignment_statement")
            .filter_map(|node| node.child_by_field_name("left"))
            .flat_map(children)
            .any(|node| {
                self.is_entry_name(node)
                    || node.kind() == "identifier" && text(node, self.source) == self.entry
            })
    }

    fn is_entry_name(&self, node: Node<'_>) -> bool {
        selector(node, self.source) == Some((self.entry, "Name"))
    }

    fn continue_unless_local(&self, guard: Node<'_>) -> bool {
        let Some(condition) = guard.child_by_field_name("condition") else {
            return false;
        };
        if guard.kind() != "if_statement"
            || guard.child_by_field_name("initializer").is_some()
            || guard.child_by_field_name("alternative").is_some()
            || condition.kind() != "unary_expression"
            || condition
                .child_by_field_name("operator")
                .is_none_or(|node| node.kind() != "!")
        {
            return false;
        }
        let Some((call, body)) = condition
            .child_by_field_name("operand")
            .zip(guard.child_by_field_name("consequence"))
        else {
            return false;
        };
        package_call(call, self.imports.filepath, self.source) == Some("IsLocal")
            && arguments(call)
                .next()
                .is_some_and(|arg| self.is_entry_name(arg))
            && statement_list(body)
                .and_then(only_child)
                .is_some_and(|stmt| text(stmt, self.source) == "continue")
    }
}

fn typed_receivers<'s>(param: Node<'_>, zip: &str, source: &'s str) -> Vec<&'s str> {
    let Some(mut ty) = param.child_by_field_name("type") else {
        return Vec::new();
    };
    if ty.kind() == "pointer_type" {
        let Some(target) = ty.named_child(0) else {
            return Vec::new();
        };
        ty = target;
    }
    if ty.kind() != "qualified_type"
        || ty
            .child_by_field_name("package")
            .is_none_or(|part| text(part, source) != zip)
        || ty
            .child_by_field_name("name")
            .is_none_or(|part| !matches!(text(part, source), "Reader" | "ReadCloser"))
    {
        return Vec::new();
    }
    let mut cursor = param.walk();
    param
        .children_by_field_name("name", &mut cursor)
        .map(|node| text(node, source))
        .collect()
}

fn range_entry<'s>(loop_node: Node<'_>, readers: &[&str], source: &'s str) -> Option<&'s str> {
    let range = children(loop_node).find(|part| part.kind() == "range_clause")?;
    let (reader, "File") = selector(range.child_by_field_name("right")?, source)? else {
        return None;
    };
    let mut cursor = range.walk();
    if !readers.contains(&reader) || !range.children(&mut cursor).any(|part| part.kind() == ":=") {
        return None;
    }
    let left = range.child_by_field_name("left")?;
    let entry = children(left)
        .filter(|part| part.kind() != "comment")
        .nth(1)?;
    (entry.kind() == "identifier").then(|| text(entry, source))
}

fn package_call<'s>(call: Node<'_>, expected: &str, source: &'s str) -> Option<&'s str> {
    (call.kind() == "call_expression").then_some(())?;
    let (selector_alias, name) = selector(call.child_by_field_name("function")?, source)?;
    (selector_alias == expected).then_some(name)
}

fn selector<'s>(node: Node<'_>, source: &'s str) -> Option<(&'s str, &'s str)> {
    if node.kind() != "selector_expression" {
        return None;
    }
    let operand = node
        .child_by_field_name("operand")
        .filter(|operand| operand.kind() == "identifier")?;
    Some((
        text(operand, source),
        text(node.child_by_field_name("field")?, source),
    ))
}

fn arguments(call: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    call.child_by_field_name("arguments")
        .into_iter()
        .flat_map(children)
        .filter(|part| part.kind() != "comment")
}

fn direct_values(statement: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let values = if statement.kind() == "expression_statement" {
        Some(statement)
    } else {
        statement.child_by_field_name("right")
    };
    values.into_iter().flat_map(children)
}

fn statement_list(block: Node<'_>) -> Option<Node<'_>> {
    children(block).find(|part| part.kind() == "statement_list")
}

fn only_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut parts = children(node).filter(|part| part.kind() != "comment");
    let result = parts.next()?;
    parts.next().is_none().then_some(result)
}

fn previous_statement(node: Node<'_>) -> Option<Node<'_>> {
    std::iter::successors(node.prev_named_sibling(), |part| part.prev_named_sibling())
        .find(|part| part.kind() != "comment")
}
