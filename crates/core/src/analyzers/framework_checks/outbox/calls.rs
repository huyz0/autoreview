use std::collections::{BTreeMap, HashMap};

use autoreview_schema::{ImportedOperation, OutboxOperationConfig};

use super::super::web::{first_argument, imports_from, named_imports, BindingScopes};
use super::super::{children, descendants, text};
use super::Node;

pub(super) struct FileCalls<'tree, 'source> {
    root: Node<'tree>,
    source: &'source str,
    bindings: BindingScopes<'tree, 'source>,
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
        let calls = descendants(root)
            .filter(|node| node.kind() == "call_expression")
            .collect();
        Self {
            root,
            source,
            bindings: BindingScopes::new(root, source),
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
                    || self.bindings.visible(root, call).is_some()
                    || !literal_matches(call, self.source, operation)
                {
                    return None;
                }
                Some((call.start_byte(), call))
            })
            .collect()
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
