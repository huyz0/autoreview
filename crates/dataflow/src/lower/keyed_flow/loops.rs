use tree_sitter::Node;

use crate::cfg::{BindingWrite, Cfg, EdgeKind, IdentityOperand, NodeId, Stmt};

use super::{keyed_syntax, line, site, Flow};

pub(crate) fn is_loop(kind: &str) -> bool {
    matches!(
        kind,
        "for_statement"
            | "for_in_statement"
            | "enhanced_for_statement"
            | "while_statement"
            | "do_statement"
            | "do_while_statement"
    )
}

pub(crate) fn jump_kind(node: Node<'_>, source: &[u8]) -> Option<bool> {
    match node.kind() {
        "break_statement" if node.named_child_count() == 0 => Some(true),
        "continue_statement" if node.named_child_count() == 0 => Some(false),
        // Kotlin's unlabeled jumps can appear as identifier expressions.
        "break_expression" | "continue_expression" | "identifier" => {
            match node.utf8_text(source).ok()? {
                "break" => Some(true),
                "continue" => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

impl Flow<'_> {
    pub(super) fn lower_loop(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, start: NodeId) -> NodeId {
        let parts = Parts::new(node);
        let entry = self.sequence(cfg, start, parts.init);
        let head = cfg.push_node(line(node));
        let body = cfg.push_node(line(node));
        let after = cfg.push_node(node.end_position().row as u32 + 1);
        let step = cfg.push_node(line(node));
        cfg.edges.push((
            entry,
            if parts.post_test { body } else { head },
            EdgeKind::Fallthrough,
        ));
        let branch = self.loop_condition(cfg, head, parts.condition);
        cfg.edges.push((branch, body, EdgeKind::True));
        if parts.can_exit {
            cfg.edges.push((branch, after, EdgeKind::False));
        }

        cfg.loop_targets.push((step, after, cfg.exit));
        for write in iteration_writes(node, self.source) {
            cfg.emit(body, Stmt::Rebind(write), line(node));
        }
        let end = self.sequence(cfg, body, parts.body);
        cfg.loop_targets.pop();
        cfg.edges.push((end, step, EdgeKind::Fallthrough));
        let next = self.lower_steps(cfg, step, parts.step);
        cfg.edges.push((next, head, EdgeKind::Loop));
        after
    }

    fn sequence<'tree>(
        &self,
        cfg: &mut Cfg<Stmt>,
        at: NodeId,
        nodes: impl IntoIterator<Item = Node<'tree>>,
    ) -> NodeId {
        nodes
            .into_iter()
            .fold(at, |at, node| self.statement(cfg, node, at))
    }

    fn lower_steps(&self, cfg: &mut Cfg<Stmt>, at: NodeId, steps: Vec<Node<'_>>) -> NodeId {
        steps.into_iter().fold(at, |at, node| {
            let next = self.statement(cfg, node, at);
            if let Some(write) = counter_step(node, self.source) {
                cfg.emit(next, Stmt::Rebind(write), line(node));
            }
            next
        })
    }

    fn loop_condition(
        &self,
        cfg: &mut Cfg<Stmt>,
        head: NodeId,
        condition: Option<Node<'_>>,
    ) -> NodeId {
        let Some(test) = condition else {
            return head;
        };
        let branch = self.exceptional_edge(cfg, test, head);
        if let Some(call) = super::direct_call(
            keyed_syntax::expression_value(test).unwrap_or(test),
            self.source,
        ) {
            cfg.emit(branch, call, line(test));
        }
        cfg.nodes[branch].condition = keyed_syntax::condition_value(test, self.source);
        cfg.nodes[branch].loop_condition = true;
        branch
    }

    pub(super) fn loop_jump(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, current: NodeId) -> NodeId {
        cfg.route_loop_jump(
            current,
            line(node),
            jump_kind(node, self.source) == Some(true),
        )
    }
}

impl Cfg<Stmt> {
    fn route_loop_jump(&mut self, current: NodeId, line: u32, breaks: bool) -> NodeId {
        if let Some(&(again, after, boundary)) = self.loop_targets.last() {
            let target = if breaks { after } else { again };
            if boundary != self.exit {
                return self.terminate(current, Stmt::LoopJump { target, boundary }, line);
            }
            self.edges.push((current, target, EdgeKind::Fallthrough));
        }
        self.push_node(line)
    }
}

struct Parts<'tree> {
    init: Vec<Node<'tree>>,
    step: Vec<Node<'tree>>,
    condition: Option<Node<'tree>>,
    body: Option<Node<'tree>>,
    post_test: bool,
    can_exit: bool,
}

impl<'tree> Parts<'tree> {
    fn new(node: Node<'tree>) -> Self {
        let header = site::children(node).find(|child| child.kind() == "for_clause");
        let header_node = header.unwrap_or(node);
        let post_test = matches!(node.kind(), "do_statement" | "do_while_statement");
        let condition = condition(node, header_node);
        let init = fields(header_node, &["init", "initializer"]);
        let step = fields(header_node, &["update", "increment"]);
        let body = node
            .child_by_field_name("body")
            .or_else(|| kotlin_body(node, condition));
        let can_exit = condition.is_some() || iterable(node);
        Self {
            init,
            step,
            condition,
            body,
            post_test,
            can_exit,
        }
    }
}

fn counter_step(node: Node<'_>, source: &[u8]) -> Option<BindingWrite> {
    if !matches!(
        node.kind(),
        "update_expression" | "inc_statement" | "dec_statement"
    ) {
        return None;
    }
    let name = node
        .named_child(0)
        .filter(|node| site::identifier(*node))?
        .utf8_text(source)
        .ok()?
        .to_owned();
    Some(BindingWrite {
        start: node.start_byte(),
        value: Some(IdentityOperand::Fresh {
            name: name.clone(),
            start: node.start_byte(),
            age: 0,
        }),
        name,
    })
}

fn condition<'tree>(node: Node<'tree>, header: Node<'tree>) -> Option<Node<'tree>> {
    let explicit = header
        .child_by_field_name("condition")
        .filter(|child| child.kind() != "empty_statement");
    explicit.or_else(|| {
        // A Go condition-only for loop has a positional expression; an
        // omitted C-style condition and the iteration forms have none.
        if node != header || iterable(node) || node.kind() != "for_statement" {
            return None;
        }
        let body = node.child_by_field_name("body")?;
        let first = site::children(node).next()?;
        (first != body
            && !first.is_extra()
            && first.kind() != "lexical_declaration"
            && node.child_by_field_name("initializer").is_none()
            && !node
                .children(&mut node.walk())
                .any(|child| child.kind() == "("))
        .then_some(first)
    })
}

fn kotlin_body<'tree>(node: Node<'tree>, condition: Option<Node<'tree>>) -> Option<Node<'tree>> {
    if node.kind() == "do_while_statement" {
        return site::children(node)
            .find(|child| Some(*child) != condition && child.kind() != "label");
    }
    let mut past_header = false;
    for child in node.children(&mut node.walk()) {
        if child.kind() == ")" {
            past_header = true;
        } else if past_header && child.is_named() && !child.is_extra() {
            return Some(child);
        }
    }
    None
}

fn fields<'tree>(node: Node<'tree>, names: &[&str]) -> Vec<Node<'tree>> {
    names
        .iter()
        .flat_map(|name| {
            node.children_by_field_name(name, &mut node.walk())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn iterable(node: Node<'_>) -> bool {
    matches!(node.kind(), "enhanced_for_statement" | "for_in_statement")
        || site::children(node).any(|child| {
            child.kind() == "range_clause"
                || (child.kind() == "variable_declaration"
                    && node.child_by_field_name("body").is_none())
        })
}

pub(crate) fn iterator_bindings(node: Node<'_>) -> Vec<Node<'_>> {
    match node.kind() {
        "range_clause" => node
            .child_by_field_name("left")
            .into_iter()
            .flat_map(site::children)
            .filter(|node| site::identifier(*node))
            .collect(),
        "for_in_statement" => node
            .child_by_field_name("left")
            .filter(|child| site::identifier(*child))
            .into_iter()
            .collect(),
        "enhanced_for_statement" => node.child_by_field_name("name").into_iter().collect(),
        "for_statement" => site::children(node)
            .find(|child| child.kind() == "variable_declaration")
            .and_then(|child| child.named_child(0))
            .filter(|child| site::identifier(*child))
            .into_iter()
            .collect(),
        "for_expression" => node
            .child_by_field_name("pattern")
            .filter(|child| site::identifier(*child))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn iteration_writes(node: Node<'_>, source: &[u8]) -> Vec<BindingWrite> {
    let binding = site::children(node)
        .find(|child| child.kind() == "range_clause")
        .unwrap_or(node);
    iterator_bindings(binding)
        .into_iter()
        .filter_map(|var| {
            let name = var.utf8_text(source).ok()?;
            if name == "_" {
                return None;
            }
            Some(BindingWrite {
                name: name.to_owned(),
                start: binding.start_byte(),
                value: Some(IdentityOperand::Fresh {
                    name: name.to_owned(),
                    start: var.start_byte(),
                    age: 0,
                }),
            })
        })
        .collect()
}
