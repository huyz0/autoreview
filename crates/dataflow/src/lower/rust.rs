//! Intraprocedural lowering for Rust functions, including inherent and trait methods.
//! Names are syntactic. Macros and independently executed closures are opaque.

mod values;
mod match_flow;
mod let_else;
mod conditional;
mod support;
mod branch_value;
mod destructuring;
mod patterns;

use support::{simple_binding, supported};
pub(crate) use destructuring::iterator_bindings;
pub use support::{match_arm_bindings, match_binding_lines, supports_function, supports_match};
pub(crate) use support::{discards_match, lexical_bindings, pattern_projection_sites};

use std::collections::HashMap;
use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};
use super::line;
use values::unwrap_value;

pub fn lower_function(source: &[u8], function: Node<'_>) -> Cfg<Stmt> {
    lower_function_with(Cfg::new_empty(), source, function)
}

pub(super) fn lower_keyed(source: &[u8], function: Node<'_>) -> Cfg<Stmt> {
    let mut cfg = Cfg::new_empty();
    cfg.keyed_flow = Some(super::keyed_flow::EXIT_PATH_LIMIT);
    lower_function_with(cfg, source, function)
}

fn lower_function_with(cfg: Cfg<Stmt>, source: &[u8], function: Node<'_>) -> Cfg<Stmt> {
    let mut lower = Lower {
        cfg,
        source,
        serial: 0,
        loops: Vec::new(),
    };
    lower.cfg.entry = lower.cfg.push_node(line(function));
    lower.cfg.exit = lower.cfg.push_node(function.end_position().row as u32 + 1);
    let entry = lower.cfg.entry;
    if let Some(body) = function
        .child_by_field_name("body")
        .filter(|body| supported(*body))
    {
        if let Some(end) = lower.block(body, entry, &HashMap::new()) {
            if let Some(tail) = super::site::children(body).last() {
                lower.return_identity(tail, end);
            }
            lower.finish(end, body.end_position().row as u32 + 1);
        }
    }
    lower.cfg
}

type Bindings = HashMap<String, String>;

struct Lower<'src> {
    cfg: Cfg<Stmt>,
    source: &'src [u8],
    serial: usize,
    loops: Vec<(NodeId, NodeId)>,
}

impl Lower<'_> {
    fn text(&self, node: Node<'_>) -> &str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) {
        self.cfg.edges.push((from, to, kind));
    }

    fn finish(&mut self, at: NodeId, source_line: u32) {
        self.cfg.emit(at, Stmt::Return { value: None }, source_line);
        self.edge(at, self.cfg.exit, EdgeKind::Fallthrough);
    }

    fn block(&mut self, body: Node<'_>, mut at: NodeId, outer: &Bindings) -> Option<NodeId> {
        let mut names = outer.clone();
        for stmt in body.named_children(&mut body.walk()) {
            at = self.statement(stmt, at, &mut names)?;
        }
        Some(at)
    }

    fn statement(&mut self, stmt: Node<'_>, at: NodeId, names: &mut Bindings) -> Option<NodeId> {
        match stmt.kind() {
            "let_declaration" if stmt.child_by_field_name("alternative").is_some() => {
                self.lower_let_else(stmt, at, names)
            }
            "let_declaration" => self.lower_local(stmt, at, names),
            "expression_statement" => self.statement(super::site::children(stmt).next()?, at, names),
            "return_expression" => self.explicit_return(stmt, at, names),
            "break_expression" | "continue_expression" => {
                self.jump(stmt, at, names);
                None
            }
            _ => self.flow_expression(stmt, at, names),
        }
    }

    fn flow_expression(&mut self, expr: Node<'_>, at: NodeId, names: &Bindings) -> Option<NodeId> {
        match expr.kind() {
            "if_expression" => self.branch(expr, at, names),
            "match_expression" => self.match_expression(expr, at, names),
            "while_expression" | "for_expression" | "loop_expression" => {
                self.loop_body(expr, at, names)
            }
            "block" => self.block(expr, at, names),
            "assignment_expression" => {
                let target = expr
                    .child_by_field_name("left")
                    .and_then(|n| self.variable(n, names));
                let end = self.value(expr.child_by_field_name("right"), at, names, target);
                super::record_evaluation(&mut self.cfg, end, expr);
                self.keyed_write(expr, end);
                Some(end)
            }
            _ => {
                let end = self.value(Some(expr), at, names, None);
                super::record_evaluation(&mut self.cfg, end, expr);
                Some(end)
            }
        }
    }

    fn return_identity(&mut self, tail: Node<'_>, at: NodeId) {
        if self.cfg.keyed_flow.is_some() {
            let value = super::keyed_syntax::result_operand(tail, self.source, false);
            self.cfg.emit(at, Stmt::ReturnIdentity(value), line(tail));
        }
    }

    fn keyed_write(&mut self, node: Node<'_>, at: NodeId) {
        if self.cfg.keyed_flow.is_some() {
            for write in super::keyed_syntax::writes(node, self.source) {
                self.cfg.emit(at, Stmt::Rebind(write), line(node));
            }
        }
    }

    fn jump(&mut self, stmt: Node<'_>, at: NodeId, names: &Bindings) {
        let at = self.value(stmt.named_child(0), at, names, None);
        super::record_evaluation(&mut self.cfg, at, stmt);
        if let Some(&(head, after)) = self.loops.last() {
            let target = if stmt.kind() == "break_expression" {
                after
            } else {
                head
            };
            self.edge(at, target, EdgeKind::Fallthrough);
        }
    }

    fn branch(&mut self, expr: Node<'_>, at: NodeId, names: &Bindings) -> Option<NodeId> {
        if branch_value::is_returned(expr) && branch_value::supports_if(expr) {
            return self.lower_branch_value(expr, at, names, &branch_value::Mode::Return);
        }
        if let Some(test) = conditional::test(expr) {
            return self.pattern_branch(test, at, names);
        }
        let condition = self.test(expr, at, names);
        if self.cfg.keyed_flow.is_some() {
            self.cfg.nodes[condition].condition = super::keyed_syntax::condition(expr, self.source);
        }
        let then = expr.child_by_field_name("consequence")?;
        let then_end = self.arm(then, condition, names, EdgeKind::True);
        let otherwise = expr
            .child_by_field_name("alternative")
            .and_then(|node| node.named_child(0));
        let else_end = otherwise.map_or(Some(condition), |node| {
            self.arm(node, condition, names, EdgeKind::False)
        });
        if then_end.is_none() && else_end.is_none() {
            return None;
        }
        let join = self.cfg.push_node(line(expr));
        if let Some(end) = then_end {
            self.edge(end, join, EdgeKind::Fallthrough);
        }
        if let Some(end) = else_end {
            let edge = if otherwise.is_none() {
                EdgeKind::False
            } else {
                EdgeKind::Fallthrough
            };
            self.edge(end, join, edge);
        }
        Some(join)
    }

    fn arm(
        &mut self,
        expr: Node<'_>,
        at: NodeId,
        names: &Bindings,
        edge: EdgeKind,
    ) -> Option<NodeId> {
        let start = self.cfg.push_node(line(expr));
        self.edge(at, start, edge);
        self.flow_expression(expr, start, names)
    }

    fn loop_body(&mut self, expr: Node<'_>, at: NodeId, names: &Bindings) -> Option<NodeId> {
        if let Some(test) = conditional::test(expr) {
            return self.pattern_loop(test, at, names);
        }
        let head = self.cfg.push_node(line(expr));
        let after = self.cfg.push_node(expr.end_position().row as u32 + 1);
        let iterable = self.evaluate(expr.child_by_field_name("value"), at, names);
        self.edge(iterable, head, EdgeKind::Fallthrough);
        let condition = self.test(expr, head, names);
        if self.cfg.keyed_flow.is_some() {
            self.cfg.nodes[condition].condition = super::keyed_syntax::condition(expr, self.source);
            self.cfg.nodes[condition].loop_condition = true;
        }
        if expr.kind() != "loop_expression" {
            self.edge(condition, after, EdgeKind::False);
        }
        self.loops.push((head, after));
        if let Some(body) = expr.child_by_field_name("body") {
            let start = self.cfg.push_node(line(body));
            self.edge(condition, start, EdgeKind::True);
            let mut body_names = names.clone();
            for binding in iterator_bindings(expr).into_iter().flatten() {
                self.project_pattern_binding(binding, start, &mut body_names);
            }
            if let Some(end) = self.flow_expression(body, start, &body_names) {
                self.edge(end, head, EdgeKind::Loop);
            }
        }
        self.loops.pop();
        self.cfg
            .edges
            .iter()
            .any(|(_, to, _)| *to == after)
            .then_some(after)
    }

    fn variable(&self, expr: Node<'_>, names: &Bindings) -> Option<String> {
        let expr = unwrap_value(expr);
        (expr.kind() == "identifier").then(|| {
            let name = self.text(expr);
            names.get(name).cloned().unwrap_or_else(|| name.to_owned())
        })
    }

    fn test(&mut self, expr: Node<'_>, at: NodeId, names: &Bindings) -> NodeId {
        self.evaluate(expr.child_by_field_name("condition"), at, names)
    }

    fn evaluate(&mut self, expr: Option<Node<'_>>, at: NodeId, names: &Bindings) -> NodeId {
        let end = self.value(expr, at, names, None);
        if let Some(expr) = expr {
            super::record_evaluation(&mut self.cfg, end, expr);
        }
        end
    }

    fn fresh(&mut self, name: &str) -> String {
        self.serial += 1;
        format!("{name}#{}", self.serial)
    }
}
