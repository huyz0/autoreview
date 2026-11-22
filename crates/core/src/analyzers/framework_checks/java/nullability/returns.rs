use std::{collections::HashMap, path::Path};

use autoreview_dataflow::{
    cfg::{Cfg, EdgeKind, NodeId, Stmt},
    lower::java::lower_function,
};
use autoreview_schema::Location;
use tree_sitter::{Node, Parser};

use super::super::{
    children, class_members, descendants,
    hierarchy::{Method, type_key},
    parse_source,
};
use super::{Java, classes, comment};

#[derive(Hash, Eq, PartialEq)]
struct MethodKey {
    owner: String,
    name: String,
    parameters: Vec<String>,
}

pub(super) struct ReturnEvidence {
    nullable: Vec<(NodeId, Location)>,
    flow: Cfg<Stmt>,
    conditions: HashMap<NodeId, (usize, bool)>,
    all_returns_known: bool,
    method_start: usize,
    pub(super) declaration: Location,
}

impl ReturnEvidence {
    pub(super) fn nullable_at_call(&self, arguments: &[Node<'_>]) -> Option<&Location> {
        let reachable = self.flow.reachable_where(|node, edge| {
            let Some(&(index, positive)) = self.conditions.get(&node) else {
                return true;
            };
            let value = match arguments.get(index).map(Node::kind) {
                Some("true") => true,
                Some("false") => false,
                _ => return true,
            };
            match edge {
                EdgeKind::True => value == positive,
                EdgeKind::False => value != positive,
                _ => true,
            }
        });
        self.nullable
            .iter()
            .find_map(|(id, location)| reachable[*id].then_some(location))
    }
}

pub(super) type ChangedReturns = HashMap<String, HashMap<usize, ReturnEvidence>>;

pub(super) fn changed_returns(
    base: &Path,
    head: &Path,
    paths: &[String],
    parser: &mut Parser,
) -> ChangedReturns {
    let mut changed = ChangedReturns::new();
    for path in paths.iter().filter(|path| path.ends_with(".java")) {
        let before = summaries(base, path, parser);
        let mut after = summaries(head, path, parser);
        after.retain(|key, evidence| {
            !evidence.nullable.is_empty()
                && before
                    .get(key)
                    .is_some_and(|old| old.all_returns_known && old.nullable.is_empty())
        });
        if !after.is_empty() {
            changed.insert(
                path.clone(),
                after.into_values().map(|e| (e.method_start, e)).collect(),
            );
        }
    }
    changed
}

fn summaries(root: &Path, path: &str, parser: &mut Parser) -> HashMap<MethodKey, ReturnEvidence> {
    let absolute = root.join(path);
    if !absolute.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
        return HashMap::new();
    }
    let Some((source, tree)) = parse_source(parser, &absolute) else {
        return HashMap::new();
    };
    let java = Java::new(path, &source, &tree);
    let mut summaries = HashMap::new();
    for class in classes(java.root) {
        let Some(owner) = type_key(&java, class) else {
            continue;
        };
        for node in class_members(class).filter(|node| node.kind() == "method_declaration") {
            let Some(method) = Method::parse(&java, node, class.kind() == "interface_declaration")
            else {
                continue;
            };
            let Some((key, evidence)) = summarize(&java, node, &owner, method) else {
                continue;
            };
            summaries.insert(key, evidence);
        }
    }
    summaries
}

fn summarize(
    java: &Java<'_>,
    node: Node<'_>,
    owner: &str,
    method: Method,
) -> Option<(MethodKey, ReturnEvidence)> {
    if !method.is_static || method.generic || method.varargs || method.return_type.is_none() {
        return None;
    }
    let parameters = method.parameters.into_iter().collect::<Option<Vec<_>>>()?;
    let body = node.child_by_field_name("body")?;
    if !direct_return_flow(body) {
        return None;
    }
    let cfg = lower_function(java.source.as_bytes(), node);
    let reachable = cfg.reachable();
    let sites: HashMap<_, _> = descendants(body)
        .filter(|node| node.kind() == "return_statement")
        .map(|node| (node.start_byte(), node))
        .collect();
    let returns: Vec<_> = cfg
        .nodes
        .iter()
        .enumerate()
        .filter(|(id, _)| reachable[*id])
        .filter_map(|(id, node)| Some((id, *sites.get(&node.return_start()?)?)))
        .collect();
    if returns.is_empty() {
        return None;
    }
    let key = MethodKey {
        owner: owner.into(),
        name: method.name,
        parameters,
    };
    let evidence = ReturnEvidence {
        nullable: returns
            .iter()
            .filter(|(_, node)| return_kind(*node) == Some("null_literal"))
            .map(|(id, node)| (*id, java.location(*node)))
            .collect(),
        all_returns_known: returns.iter().all(|(_, node)| is_known_return(*node)),
        conditions: boolean_conditions(java, node, body, &cfg),
        flow: cfg,
        method_start: node.start_byte(),
        declaration: method.location,
    };
    Some((key, evidence))
}

fn is_known_return(node: Node<'_>) -> bool {
    matches!(
        return_kind(node),
        Some(
            "null_literal"
                | "string_literal"
                | "object_creation_expression"
                | "array_creation_expression"
                | "class_literal"
        )
    )
}

fn boolean_conditions(
    java: &Java<'_>,
    method: Node<'_>,
    body: Node<'_>,
    cfg: &Cfg<Stmt>,
) -> HashMap<NodeId, (usize, bool)> {
    if descendants(body)
        .any(|node| matches!(node.kind(), "assignment_expression" | "update_expression"))
    {
        return HashMap::new();
    }
    let parameters: HashMap<_, _> = method
        .child_by_field_name("parameters")
        .into_iter()
        .flat_map(children)
        .filter(|node| node.kind() == "formal_parameter")
        .enumerate()
        .filter(|(_, node)| java.field(*node, "type") == Some("boolean"))
        .filter_map(|(index, node)| Some((java.field(node, "name")?, index)))
        .collect();

    cfg.nodes
        .iter()
        .enumerate()
        .filter_map(|(id, node)| {
            let span = node.condition_span()?;
            let condition =
                unwrap_parentheses(body.descendant_for_byte_range(span.start, span.end)?)?;
            let raw = java.text(condition).trim();
            let (binding, positive) = raw
                .strip_prefix('!')
                .map_or((raw, true), |name| (name.trim(), false));
            Some((id, (*parameters.get(binding)?, positive)))
        })
        .collect()
}

fn unwrap_parentheses(mut value: Node<'_>) -> Option<Node<'_>> {
    while value.kind() == "parenthesized_expression" {
        value = children(value).find(|node| !comment(*node))?;
    }
    Some(value)
}

fn return_kind(statement: Node<'_>) -> Option<&'static str> {
    let value = children(statement).find(|node| !comment(*node))?;
    Some(unwrap_parentheses(value)?.kind())
}

fn direct_return_flow(statement: Node<'_>) -> bool {
    match statement.kind() {
        "block" => children(statement).all(direct_return_flow),
        "if_statement" => ["consequence", "alternative"]
            .iter()
            .filter_map(|name| statement.child_by_field_name(*name))
            .all(direct_return_flow),
        "return_statement" | "throw_statement" | "line_comment" | "block_comment" => true,
        _ => false,
    }
}
