use tree_sitter::Node;

use crate::cfg::{CallTarget, Cfg, EdgeKind, NodeId, Stmt};

use super::{keyed_syntax, site, StatementLowerer};

mod handlers;

pub(super) const EXIT_PATH_LIMIT: usize = 128;

pub(super) fn lower(
    cfg: &mut Cfg<Stmt>,
    node: Node<'_>,
    source: &[u8],
    current: NodeId,
    ordinary: StatementLowerer,
) -> NodeId {
    Flow { source, ordinary }.statement(cfg, node, current)
}

pub(super) struct Flow<'a> {
    source: &'a [u8],
    ordinary: StatementLowerer,
}

impl Flow<'_> {
    fn statement(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, current: NodeId) -> NodeId {
        match node.kind() {
            "try_statement" | "try_expression" => self.protected(cfg, node, current),
            "throw_statement" | "throw_expression" => {
                let stmt = Stmt::Throw {
                    exception: handlers::throw_type(node, self.source),
                    implicit: false,
                };
                cfg.terminate(current, stmt, line(node))
            }
            "defer_statement" => {
                if let Some(stmt) = self.deferred(node) {
                    cfg.emit(current, stmt, line(node));
                }
                current
            }
            "block" | "statement_block" => self.block(cfg, node, current),
            _ => self.ordinary_statement(cfg, node, current),
        }
    }

    fn block(&self, cfg: &mut Cfg<Stmt>, body: Node<'_>, entry: NodeId) -> NodeId {
        let statements = site::children(body)
            .find(|child| child.kind() == "statement_list")
            .unwrap_or(body);
        super::lower_block(cfg, statements, self.source, entry, self.ordinary)
    }

    fn ordinary_statement(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, current: NodeId) -> NodeId {
        let current = self.exceptional_edge(cfg, node, current);
        if matches!(node.kind(), "if_statement" | "if_expression") {
            cfg.nodes[current].condition = keyed_syntax::condition(node, self.source);
        }
        let start = cfg.nodes[current].stmts.len();
        let end = match awaited_call(node, self.source) {
            Some(call) => {
                cfg.emit(current, call, line(node));
                current
            }
            None => (self.ordinary)(cfg, node, self.source, current),
        };
        cfg.nodes[current].record_statement_lines(start, line(node));
        for write in keyed_syntax::writes(node, self.source) {
            cfg.emit(end, Stmt::Rebind(write), line(node));
        }
        end
    }

    fn exceptional_edge(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, current: NodeId) -> NodeId {
        if !cfg.keyed_exceptions {
            return current;
        }
        let Some(call) =
            keyed_syntax::expression(node).and_then(|expr| throwing_call(expr, self.source))
        else {
            return current;
        };
        let next = cfg.push_node(line(node));
        let unwind = cfg.push_node(line(node));
        cfg.nodes[current].throwing_call = Some(call);
        cfg.edges.push((current, next, EdgeKind::Fallthrough));
        cfg.edges.push((current, unwind, EdgeKind::Exceptional));
        cfg.emit(
            unwind,
            Stmt::Throw {
                exception: None,
                implicit: true,
            },
            line(node),
        );
        cfg.edges.push((unwind, cfg.exit, EdgeKind::Fallthrough));
        next
    }

    fn deferred(&self, node: Node<'_>) -> Option<Stmt> {
        let call = node.named_child(0)?;
        if let Some(target) = super::go::call_target_name(call, self.source) {
            return Some(Stmt::DeferredCall {
                target,
                site: site::capture(call, self.source),
            });
        }
        let function = call
            .child_by_field_name("function")
            .filter(|node| node.kind() == "func_literal")?;
        let arguments = call.child_by_field_name("arguments")?;
        if site::children(arguments).next().is_some() {
            return None;
        }
        let mut graph = Cfg::new_empty();
        graph.keyed_flow = Some(EXIT_PATH_LIMIT);
        let graph = super::function(
            graph,
            self.source,
            function,
            function.child_by_field_name("body"),
            super::go::lower_block,
        );
        Some(Stmt::DeferredBlock(Box::new(graph)))
    }
}

fn awaited_call(node: Node<'_>, source: &[u8]) -> Option<Stmt> {
    if node.kind() != "expression_statement" || node.named_child(0)?.kind() != "await_expression" {
        return None;
    }
    let call = keyed_syntax::expression(node).filter(|node| node.kind() == "call_expression")?;
    Some(Stmt::Call {
        target: CallTarget::Named(site::name(call, source)?),
        site: site::capture(call, source),
        args: Vec::new(),
        assigned_to: None,
    })
}

fn throwing_call(expr: Node<'_>, source: &[u8]) -> Option<(String, usize)> {
    let target = match expr.kind() {
        "method_invocation" | "call_expression" => site::name(expr, source).unwrap_or_default(),
        "new_expression" | "object_creation_expression" => String::new(),
        _ => return None,
    };
    Some((target, expr.start_byte()))
}

pub(super) struct Exit {
    stmt: Stmt,
    line: u32,
}

impl Cfg<Stmt> {
    fn emit(&mut self, at: NodeId, stmt: Stmt, line: u32) {
        let node = &mut self.nodes[at];
        let start = node.stmts.len();
        node.stmts.push(stmt);
        node.record_statement_lines(start, line);
    }

    fn terminate(&mut self, at: NodeId, stmt: Stmt, line: u32) -> NodeId {
        self.emit(at, stmt, line);
        self.edges.push((at, self.exit, EdgeKind::Fallthrough));
        self.push_node(line)
    }

    fn take_exits(&mut self, target: NodeId) -> Vec<(NodeId, Exit)> {
        let mut sources = Vec::new();
        self.edges.retain(|&(from, to, _)| {
            if to == target {
                sources.push(from);
            }
            to != target
        });
        sources
            .into_iter()
            .filter_map(|from| {
                let node = &mut self.nodes[from];
                let index = node.stmts.len().checked_sub(1)?;
                let stmt = node.stmts.pop()?;
                Some((
                    from,
                    Exit {
                        stmt,
                        line: node.statement_line(index),
                    },
                ))
            })
            .collect()
    }

    fn resume_exit(&mut self, from: NodeId, exit: Exit) {
        self.emit(from, exit.stmt, exit.line);
        self.edges.push((from, self.exit, EdgeKind::Fallthrough));
    }
}

pub(crate) fn supports_try(node: Node<'_>) -> bool {
    handlers::Parts::new(node).is_some()
}

fn line(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}
