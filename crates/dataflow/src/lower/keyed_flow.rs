use tree_sitter::Node;

use crate::cfg::{CallTarget, Cfg, EdgeKind, NodeId, Stmt, ThrowingCall};

use super::{keyed_syntax, line, site, StatementLowerer};

mod handlers;
mod conditional;
mod switch;
pub(crate) use switch::supports_go_switch;
pub(crate) mod loops;

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
            kind if loops::is_loop(kind) => self.lower_loop(cfg, node, current),
            _ if loops::jump_kind(node, self.source).is_some() => {
                self.loop_jump(cfg, node, current)
            }
            "try_statement" | "try_expression" => self.protected(cfg, node, current),
            "throw_statement" | "throw_expression" => {
                super::record_evaluation(cfg, current, node);
                let stmt = Stmt::Throw {
                    exception: handlers::throw_type(node, self.source),
                    implicit: false,
                };
                cfg.terminate(current, stmt, line(node))
            }
            "defer_statement" => {
                let current = node.named_child(0).map_or(current, |call| nested_arguments(cfg, call, self.source, current));
                super::record_evaluation(cfg, current, node);
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
        if supports_go_switch(node) { return self.go_switch(cfg, node, current); }
        if let Some(end) = conditional::lower(cfg, node, self.source, current) { return end; }
        let current = keyed_syntax::expression(node).map_or(current, |expr| nested_arguments(cfg, expr, self.source, current));
        let current = self.exceptional_edge(cfg, node, current);
        let conditional = matches!(node.kind(), "if_statement" | "if_expression");
        if conditional {
            cfg.nodes[current].condition = keyed_syntax::condition(node, self.source);
        } else {
            super::record_evaluation(cfg, current, node);
        }
        self.lower_return_value(cfg, node, current);
        let start = cfg.nodes[current].stmts.len();
        let end = match positioned_call(node, self.source) {
            Some(call) => {
                cfg.emit(current, call, line(node));
                current
            }
            None => (self.ordinary)(cfg, node, self.source, current),
        };
        // Go emits an optional initializer in the test's node.
        if let Some(test) = conditional.then(|| node.child_by_field_name("condition")).flatten() {
            super::record_evaluation(cfg, current, test);
        }
        cfg.nodes[current].record_statement_lines(start, line(node));
        for write in keyed_syntax::writes(node, self.source) {
            cfg.emit(end, Stmt::Rebind(write), line(node));
        }
        end
    }

    fn lower_return_value(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, current: NodeId) {
        if !matches!(node.kind(), "return_statement" | "return_expression") {
            return;
        }
        cfg.nodes[current].return_start = Some(node.start_byte());
        let expr = keyed_syntax::expression(node);
        if let Some(call) = expr.and_then(|expr| direct_call(expr, self.source)) {
            cfg.emit(current, call, line(node));
        }
        let operand = expr.and_then(|expr| keyed_syntax::result_operand(expr, self.source, false));
        cfg.emit(current, Stmt::ReturnIdentity(operand), line(node));
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
        cfg.fork_exception(current, call, line(node))
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

fn nested_arguments(cfg: &mut Cfg<Stmt>, call: Node<'_>, source: &[u8], mut current: NodeId) -> NodeId {
    if call.kind() == "array" {
        for item in site::children(call) {
            current = nested_value(cfg, item, source, current);
        }
        return current;
    }
    if !matches!(call.kind(), "call_expression" | "method_invocation" | "new_expression") {
        return current;
    }
    // Optional calls and spread arguments do not have a modeled, unconditional
    // argument evaluation order. Do not execute nested effects on those paths.
    if site::capture(call, source).is_none() {
        return current;
    }
    let Some(arguments) = site::arguments(call) else { return current };
    for argument in site::children(arguments) {
        current = nested_value(cfg, argument, source, current);
    }
    current
}

fn nested_value(cfg: &mut Cfg<Stmt>, argument: Node<'_>, source: &[u8], current: NodeId) -> NodeId {
    let Some(value) = site::argument(argument).and_then(keyed_syntax::expression_value) else { return current; };
    if value.kind() == "array" { return nested_arguments(cfg, value, source, current); }
    let Some(value) = nested_call(argument, source) else { return current; };
    let mut current = nested_arguments(cfg, value, source, current);
    if cfg.keyed_exceptions {
        if let Some(call) = throwing_call(value, source) {
            current = cfg.fork_exception(current, call, line(value));
        }
    }
    if let Some(call) = direct_call(value, source) { cfg.emit(current, call, line(value)); }
    current
}

fn nested_call<'tree>(argument: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    let value = site::argument(argument).and_then(keyed_syntax::expression_value)?;
    (matches!(value.kind(), "call_expression" | "method_invocation") && site::capture(value, source).is_some())
        .then_some(value)
}

fn positioned_call(node: Node<'_>, source: &[u8]) -> Option<Stmt> {
    let call = match node.kind() {
        "expression_statement" if matches!(node.named_child(0)?.kind(), "await_expression" | "new_expression") => {
            keyed_syntax::expression(node)?
        }
        "call_expression" | "method_invocation" => node,
        _ => return None,
    };
    direct_call(call, source)
}

fn direct_call(call: Node<'_>, source: &[u8]) -> Option<Stmt> {
    if !matches!(call.kind(), "call_expression" | "method_invocation" | "new_expression") {
        return None;
    }
    Some(Stmt::Call {
        target: CallTarget::Named(site::name(call, source)?),
        site: site::capture(call, source),
        args: Vec::new(),
        assigned_to: None,
    })
}

fn throwing_call(expr: Node<'_>, source: &[u8]) -> Option<ThrowingCall> {
    let target = match expr.kind() {
        "method_invocation" | "call_expression" | "new_expression" => site::name(expr, source).unwrap_or_default(),
        "object_creation_expression" => String::new(),
        _ => return None,
    };
    Some(ThrowingCall {
        name: target,
        start: expr.start_byte(),
        site: site::capture(expr, source),
    })
}

pub(super) struct Exit {
    stmt: Stmt,
    line: u32,
}

impl Cfg<Stmt> {
    fn fork_exception(&mut self, current: NodeId, call: ThrowingCall, line: u32) -> NodeId {
        let next = self.push_node(line);
        let unwind = self.push_node(line);
        self.nodes[current].throwing_call = Some(call);
        self.edges.push((current, next, EdgeKind::Fallthrough));
        self.edges.push((current, unwind, EdgeKind::Exceptional));
        self.emit(
            unwind,
            Stmt::Throw {
                exception: None,
                implicit: true,
            },
            line,
        );
        self.edges.push((unwind, self.exit, EdgeKind::Fallthrough));
        next
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
        if let Stmt::LoopJump { target, boundary } = exit.stmt {
            if boundary == self.exit {
                self.edges.push((from, target, EdgeKind::Fallthrough));
                return;
            }
        }
        self.emit(from, exit.stmt, exit.line);
        self.edges.push((from, self.exit, EdgeKind::Fallthrough));
    }
}

pub(crate) fn supports_try(node: Node<'_>) -> bool {
    handlers::Parts::new(node).is_some()
}

pub(super) fn expression_body(cfg: &mut Cfg<Stmt>, expr: Node<'_>, source: &[u8], at: NodeId) {
    let at = nested_arguments(cfg, expr, source, at);
    let at =
        throwing_call(expr, source).map_or(at, |call| cfg.fork_exception(at, call, line(expr)));
    super::record_evaluation(cfg, at, expr);
    if let Some(call) = direct_call(expr, source) {
        cfg.emit(at, call, line(expr));
    }
    cfg.emit(
        at,
        Stmt::ReturnIdentity(keyed_syntax::result_operand(expr, source, false)),
        line(expr),
    );
    cfg.terminate(at, Stmt::Return { value: None }, line(expr));
}
