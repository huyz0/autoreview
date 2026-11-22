use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{line, site, Exit, Flow};

pub(super) struct Parts<'tree> {
    body: Node<'tree>,
    handlers: Vec<Node<'tree>>,
    finally: Option<Node<'tree>>,
}

impl<'tree> Parts<'tree> {
    pub(super) fn new(node: Node<'tree>) -> Option<Self> {
        let body = site::children(node).find(|node| is_block(*node))?;
        let handlers: Vec<_> = site::children(node)
            .filter(|node| matches!(node.kind(), "catch_clause" | "catch_block"))
            .collect();
        let finally = site::children(node)
            .find(|node| matches!(node.kind(), "finally_clause" | "finally_block"))
            .and_then(block);
        (!handlers.is_empty() || finally.is_some()).then_some(Self {
            body,
            handlers,
            finally,
        })
    }
}

impl Flow<'_> {
    pub(super) fn protected(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, entry: NodeId) -> NodeId {
        let Some(parts) = Parts::new(node) else {
            return (self.ordinary)(cfg, node, self.source, entry);
        };
        let temporary = cfg.push_node(line(node));
        let outer_exit = std::mem::replace(&mut cfg.exit, temporary);
        let normal = self.block(cfg, parts.body, entry);
        let exits = cfg.take_exits(temporary);
        let join = cfg.push_node(line(node));
        cfg.edges.push((normal, join, EdgeKind::Fallthrough));

        let handlers: Vec<_> = parts
            .handlers
            .into_iter()
            .filter_map(|handler| self.handler(cfg, handler, join))
            .collect();
        let mut leaving = route(cfg, exits, &handlers);
        leaving.extend(cfg.take_exits(temporary));
        cfg.exit = outer_exit;

        for (from, exit) in leaving {
            self.resume(cfg, parts.finally, from, exit);
        }
        parts
            .finally
            .map_or(join, |body| self.block(cfg, body, join))
    }

    fn handler(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, join: NodeId) -> Option<Handler> {
        let body = block(node)?;
        let start = cfg.push_node(line(node));
        let end = self.block(cfg, body, start);
        cfg.edges.push((end, join, EdgeKind::Fallthrough));
        Some(Handler {
            start,
            types: catch_types(node, self.source),
        })
    }

    fn resume(&self, cfg: &mut Cfg<Stmt>, finally: Option<Node<'_>>, from: NodeId, exit: Exit) {
        let Some(body) = finally else {
            cfg.resume_exit(from, exit);
            return;
        };
        let Some(budget) = cfg.keyed_flow.as_mut().filter(|remaining| **remaining > 0) else {
            return;
        };
        *budget -= 1;
        let start = cfg.push_node(line(body));
        cfg.edges.push((from, start, EdgeKind::Fallthrough));
        let end = self.block(cfg, body, start);
        cfg.resume_exit(end, exit);
    }
}

struct Handler {
    start: NodeId,
    types: Vec<String>,
}

fn route(
    cfg: &mut Cfg<Stmt>,
    exits: Vec<(NodeId, Exit)>,
    handlers: &[Handler],
) -> Vec<(NodeId, Exit)> {
    let mut leaving = Vec::new();
    for (from, exit) in exits {
        if let Stmt::Throw { exception, .. } = &exit.stmt {
            if caught(cfg, from, handlers, exception.as_deref()) {
                continue;
            }
        }
        leaving.push((from, exit));
    }
    leaving
}

fn caught(
    cfg: &mut Cfg<Stmt>,
    from: NodeId,
    handlers: &[Handler],
    exception: Option<&str>,
) -> bool {
    for handler in handlers {
        let coverage = handler.coverage(exception);
        if coverage != Coverage::None {
            cfg.edges.push((from, handler.start, EdgeKind::Fallthrough));
        }
        if coverage == Coverage::All {
            return true;
        }
    }
    false
}

#[derive(PartialEq, Eq)]
enum Coverage {
    None,
    Some,
    All,
}

impl Handler {
    fn coverage(&self, exception: Option<&str>) -> Coverage {
        if self.types.is_empty()
            || self
                .types
                .iter()
                .any(|kind| kind == "Throwable" || Some(kind.as_str()) == exception)
        {
            return Coverage::All;
        }
        if self.types.iter().any(|kind| kind == "Exception")
            && exception.is_none_or(|kind| kind.ends_with("Exception"))
        {
            return Coverage::All;
        }
        if exception.is_some_and(|thrown| thrown.ends_with("Error"))
            && self.types.iter().all(|kind| kind.ends_with("Exception"))
        {
            return Coverage::None;
        }
        Coverage::Some
    }
}

fn catch_types(node: Node<'_>, source: &[u8]) -> Vec<String> {
    if let Some(parameter) =
        site::children(node).find(|child| child.kind() == "catch_formal_parameter")
    {
        let ty = site::children(parameter).find(|child| child.kind() == "catch_type");
        return ty
            .into_iter()
            .flat_map(site::children)
            .filter_map(|child| type_name(child, source))
            .collect();
    }
    if node.kind() == "catch_block" {
        return site::children(node)
            .find(|child| matches!(child.kind(), "user_type" | "nullable_type"))
            .and_then(|child| type_name(child, source))
            .into_iter()
            .collect();
    }
    Vec::new()
}

pub(super) fn throw_type(node: Node<'_>, source: &[u8]) -> Option<String> {
    let value = node.named_child(0)?;
    let ty = match value.kind() {
        "object_creation_expression" => value.child_by_field_name("type")?,
        "call_expression" => value.named_child(0)?,
        _ => return None,
    };
    type_name(ty, source)
}

fn type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    Some(text.rsplit('.').next()?.trim().to_owned())
}

fn block(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("body")
        .or_else(|| site::children(node).find(|node| is_block(*node)))
}

fn is_block(node: Node<'_>) -> bool {
    matches!(node.kind(), "block" | "statement_block")
}
