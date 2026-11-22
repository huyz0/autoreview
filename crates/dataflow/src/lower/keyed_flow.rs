use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{site, StatementLowerer};

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
            if let Some(call) = node.named_child(0) {
                if let Some(target) = super::go::call_target_name(call, source) {
                    cfg.nodes[current].stmts.push(Stmt::DeferredCall {
                        target,
                        site: site::capture(call, source),
                    });
                }
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
        super::lower_block(cfg, body, self.source, entry, self.ordinary)
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
        let Some(budget) = cfg.keyed_flow.as_mut().filter(|remaining| **remaining > 0) else {
            return;
        };
        *budget -= 1;
        let node = &mut cfg.nodes[from];
        let exit_line = node.statement_line(node.stmts.len() - 1);
        let terminator = node
            .stmts
            .pop()
            .expect("temporary exit was reached by return or throw");

        let start = cfg.push_node(line(finalizer));
        cfg.edges.push((from, start, EdgeKind::Fallthrough));
        let end = self.block(cfg, finalizer, start);
        let node = &mut cfg.nodes[end];
        let index = node.stmts.len();
        node.stmts.push(terminator);
        node.record_statement_lines(index, exit_line);
        cfg.edges.push((end, cfg.exit, EdgeKind::Fallthrough));
    }
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
