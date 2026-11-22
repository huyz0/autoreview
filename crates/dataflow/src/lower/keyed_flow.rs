use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{site, StatementLowerer};

pub(super) const EXIT_PATH_LIMIT: usize = 128;

pub(super) fn needed(function: Node<'_>) -> bool {
    let mut nodes = vec![function];
    while let Some(node) = nodes.pop() {
        if matches!(
            node.kind(),
            "finally_clause"
                | "finally_block"
                | "defer_statement"
                | "throw_statement"
                | "throw_expression"
        ) {
            return true;
        }
        nodes.extend(site::children(node));
    }
    false
}

pub(super) fn lower(
    cfg: &mut Cfg<Stmt>,
    node: Node<'_>,
    source: &[u8],
    current: NodeId,
    ordinary: StatementLowerer,
) -> NodeId {
    let lowering = Flow { source, ordinary };
    match node.kind() {
        "try_statement" | "try_expression" => lowering.try_finally(cfg, node, current),
        "throw_statement" | "throw_expression" => {
            cfg.nodes[current].stmts.push(Stmt::Throw);
            cfg.edges.push((current, cfg.exit, EdgeKind::Fallthrough));
            cfg.push_node(line(node))
        }
        "defer_statement" => {
            if let Some(calls) = deferred_calls(node, source) {
                cfg.nodes[current].stmts.extend(calls);
            }
            current
        }
        "block" | "statement_block" => lowering.block(cfg, node, current),
        _ => ordinary(cfg, node, source, current),
    }
}

struct Flow<'a> {
    source: &'a [u8],
    ordinary: StatementLowerer,
}

impl Flow<'_> {
    fn block(&self, cfg: &mut Cfg<Stmt>, body: Node<'_>, entry: NodeId) -> NodeId {
        let statements = site::children(body)
            .find(|child| child.kind() == "statement_list")
            .unwrap_or(body);
        super::lower_block(cfg, statements, self.source, entry, self.ordinary)
    }

    fn try_finally(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, entry: NodeId) -> NodeId {
        let Some((body, finalizer)) = blocks(node) else {
            return (self.ordinary)(cfg, node, self.source, entry);
        };
        if cfg.keyed_flow == Some(0) {
            return entry;
        }
        let temporary_exit = cfg.push_node(line(node));
        let outer_exit = std::mem::replace(&mut cfg.exit, temporary_exit);
        let normal_end = self.block(cfg, body, entry);
        cfg.exit = outer_exit;

        // Each exiting path keeps its own continuation and original source
        // line. Joining those paths with fallthrough would let a return
        // incorrectly continue after the try statement.
        let mut exits = Vec::new();
        cfg.edges.retain(|&(from, to, _)| {
            if to == temporary_exit {
                exits.push(from);
            }
            to != temporary_exit
        });
        for from in exits {
            self.finalize_exit(cfg, finalizer, from);
        }
        self.block(cfg, finalizer, normal_end)
    }

    fn finalize_exit(&self, cfg: &mut Cfg<Stmt>, finalizer: Node<'_>, from: NodeId) {
        if let Some((start, exit)) = cfg.unwind_from(from, line(finalizer)) {
            let end = self.block(cfg, finalizer, start);
            cfg.resume_exit(end, exit);
        }
    }
}

struct Exit {
    stmt: Stmt,
    line: u32,
}

impl Cfg<Stmt> {
    fn unwind_from(&mut self, from: NodeId, finally_line: u32) -> Option<(NodeId, Exit)> {
        let budget = self
            .keyed_flow
            .as_mut()
            .filter(|remaining| **remaining > 0)?;
        *budget -= 1;
        let node = &mut self.nodes[from];
        let line = node.statement_line(node.stmts.len() - 1);
        let stmt = node
            .stmts
            .pop()
            .expect("temporary exit was reached by return or throw");
        let start = self.push_node(finally_line);
        self.edges.push((from, start, EdgeKind::Fallthrough));
        Some((start, Exit { stmt, line }))
    }

    fn resume_exit(&mut self, end: NodeId, exit: Exit) {
        let node = &mut self.nodes[end];
        let index = node.stmts.len();
        node.stmts.push(exit.stmt);
        node.record_statement_lines(index, exit.line);
        self.edges.push((end, self.exit, EdgeKind::Fallthrough));
    }
}

fn deferred_calls(defer: Node<'_>, source: &[u8]) -> Option<Vec<Stmt>> {
    let call = defer.named_child(0)?;
    if let Some(direct) = deferred_call(call, source) {
        return Some(vec![direct]);
    }

    // An immediately deferred zero-argument closure can schedule a straight
    // line of removals. Anything else requires modeling its own execution.
    let function = call
        .child_by_field_name("function")
        .filter(|node| node.kind() == "func_literal")?;
    if site::children(call.child_by_field_name("arguments")?)
        .next()
        .is_some()
    {
        return None;
    }
    let body = function.child_by_field_name("body")?;
    let Some(list) = site::children(body).find(|node| node.kind() == "statement_list") else {
        return Some(Vec::new());
    };
    site::children(list)
        .map(|stmt| {
            let call = stmt.named_child(0).filter(|call| {
                stmt.kind() == "expression_statement" && call.kind() == "call_expression"
            })?;
            deferred_call(call, source)
        })
        .collect()
}

fn deferred_call(call: Node<'_>, source: &[u8]) -> Option<Stmt> {
    Some(Stmt::DeferredCall {
        target: super::go::call_target_name(call, source)?,
        site: site::capture(call, source),
    })
}

pub(crate) fn blocks(node: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let block = |node: Node<'_>| matches!(node.kind(), "block" | "statement_block");
    let body = site::children(node).find(|node| block(*node))?;
    let finalizer = site::children(node)
        .find(|node| matches!(node.kind(), "finally_clause" | "finally_block"))?;
    Some((body, site::children(finalizer).find(|node| block(*node))?))
}

fn line(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}
