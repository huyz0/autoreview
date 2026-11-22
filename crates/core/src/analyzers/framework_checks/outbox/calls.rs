use std::collections::{BTreeMap, HashMap};

use autoreview_schema::{ImportedOperation, OutboxOperationConfig};

use super::super::web::{binding_nodes, first_argument, imports_from, is_function, named_imports};
use super::super::{children, descendants, text};
use super::Node;

pub(super) struct FileCalls<'tree, 'source> {
    root: Node<'tree>,
    source: &'source str,
    bindings: HashMap<&'source str, Vec<Node<'tree>>>,
    calls: Vec<Node<'tree>>,
}

pub(super) struct Endpoints<'tree> {
    pub write: BTreeMap<usize, Node<'tree>>,
    pub publish: BTreeMap<usize, Node<'tree>>,
}

impl Endpoints<'_> {
    pub fn has_both_in(&self, function: Node<'_>) -> bool {
        let range = function.byte_range();
        self.write.range(range.clone()).next().is_some()
            && self.publish.range(range).next().is_some()
    }
}

impl<'tree, 'source> FileCalls<'tree, 'source> {
    pub fn new(root: Node<'tree>, source: &'source str) -> Self {
        let mut bindings: HashMap<&str, Vec<Node<'_>>> = HashMap::new();
        let mut calls = Vec::new();
        for node in descendants(root) {
            if node.kind() == "call_expression" {
                calls.push(node);
            }
            for binding in binding_nodes(node) {
                bindings
                    .entry(text(binding, source))
                    .or_default()
                    .push(binding_scope(binding));
            }
        }
        Self {
            root,
            source,
            bindings,
            calls,
        }
    }

    pub fn resolve(&self, operation: &OutboxOperationConfig) -> Option<Endpoints<'tree>> {
        let write = self.matching_calls(&operation.database_write);
        let mut publish = self.matching_calls(&operation.broker_publish);
        // One invocation cannot serve as evidence of two different operations.
        publish.retain(|start, _| !write.contains_key(start));
        (!write.is_empty() && !publish.is_empty()).then_some(Endpoints { write, publish })
    }

    fn matching_calls(&self, operation: &ImportedOperation) -> BTreeMap<usize, Node<'tree>> {
        let imported = import_targets(self.root, self.source, operation);
        if imported.is_empty() {
            return BTreeMap::new();
        }
        self.calls
            .iter()
            .copied()
            .filter_map(|call| {
                let (root, members) =
                    member_path(call.child_by_field_name("function")?, self.source)?;
                if !imported.get(root)?.contains(&members)
                    || self.shadowed(root, call)
                    || !literal_matches(call, self.source, operation)
                {
                    return None;
                }
                Some((call.start_byte(), call))
            })
            .collect()
    }

    fn shadowed(&self, name: &str, use_site: Node<'_>) -> bool {
        self.bindings.get(name).is_some_and(|scopes| {
            scopes.iter().any(|scope| {
                scope.start_byte() <= use_site.start_byte()
                    && use_site.end_byte() <= scope.end_byte()
            })
        })
    }
}

fn import_targets<'s>(
    root: Node<'_>,
    source: &'s str,
    operation: &'s ImportedOperation,
) -> HashMap<&'s str, Vec<Vec<&'s str>>> {
    let suffix = || operation.members.iter().map(String::as_str);
    let mut names: HashMap<_, Vec<_>> = HashMap::new();
    for (_, local) in named_imports(root, source, &operation.module)
        .filter(|(export, _)| *export == operation.export)
    {
        names.entry(local).or_default().push(suffix().collect());
    }
    for import in imports_from(root, source, &operation.module)
        .flat_map(children)
        .filter(|node| node.kind() == "import_clause")
        .flat_map(children)
    {
        match import.kind() {
            "namespace_import" => {
                if let Some(local) = import.named_child(0) {
                    let members = std::iter::once(operation.export.as_str())
                        .chain(suffix())
                        .collect();
                    names.entry(text(local, source)).or_default().push(members);
                }
            }
            "identifier" if operation.export == "default" => {
                names
                    .entry(text(import, source))
                    .or_default()
                    .push(suffix().collect());
            }
            _ => {}
        }
    }
    names
}

fn member_path<'s>(mut node: Node<'_>, source: &'s str) -> Option<(&'s str, Vec<&'s str>)> {
    let mut members = Vec::new();
    while node.kind() == "member_expression" {
        let property = node
            .child_by_field_name("property")
            .filter(|part| part.kind() == "property_identifier")?;
        members.push(text(property, source));
        node = node.child_by_field_name("object")?;
    }
    members.reverse();
    (node.kind() == "identifier").then(|| (text(node, source), members))
}

fn literal_matches(call: Node<'_>, source: &str, operation: &ImportedOperation) -> bool {
    operation.first_argument.as_deref().is_none_or(|expected| {
        let Some(argument) = first_argument(call).filter(|node| node.kind() == "string") else {
            return false;
        };
        let spelling = text(argument, source);
        let Some(quote @ ('"' | '\'')) = spelling.chars().next() else {
            return false;
        };
        spelling
            .strip_prefix(quote)
            .and_then(|s| s.strip_suffix(quote))
            .is_some_and(|value| !value.contains('\\') && value == expected)
    })
}

fn binding_scope(binding: Node<'_>) -> Node<'_> {
    let Some(parent) = binding.parent() else {
        return binding;
    };
    let owner = if matches!(parent.kind(), "function_declaration" | "class_declaration") {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    };
    let is_var = parent.kind() == "variable_declarator"
        && parent
            .parent()
            .is_some_and(|declaration| declaration.kind() == "variable_declaration");
    std::iter::successors(Some(owner), |node| node.parent())
        .find(|scope| {
            is_function(*scope) || scope.kind() == "program" || !is_var && lexical_scope(*scope)
        })
        .unwrap_or(owner)
}

fn lexical_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "statement_block" | "for_statement" | "for_in_statement" | "switch_body" | "catch_clause"
    )
}
