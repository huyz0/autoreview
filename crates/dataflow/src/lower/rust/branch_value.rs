use super::{argument_flow, conditional, line, match_flow, Bindings, Lower, Node, NodeId};
use crate::cfg::{RhsShape, Stmt};
use crate::lower::{record_evaluation, site};

mod assignment;
use assignment::{is_initializer, is_reassignment, Assignment};

pub(super) enum Mode<'a> {
    Return,
    Assign(Assignment<'a>),
    Project,
    Operand { syntax: Node<'a>, temporary: &'a str },
}

pub(super) fn ungroup_value(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let Some(inner) = site::children(node).next() else { break };
        node = inner;
    }
    node
}

pub(super) fn from_value(node: Node<'_>) -> Option<Node<'_>> {
    let node = ungroup_value(node);
    matches!(node.kind(), "if_expression" | "match_expression").then_some(node)
}

pub(super) fn outermost_group(mut node: Node<'_>) -> Node<'_> {
    while let Some(group) = node.parent().filter(|owner| owner.kind() == "parenthesized_expression") {
        node = group;
    }
    node
}

pub(crate) fn is_returned(node: Node<'_>) -> bool {
    let node = outermost_group(node);
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "return_expression" {
        return true;
    }
    let expression = if parent.kind() == "expression_statement" && !has_semicolon(parent) {
        parent
    } else {
        node
    };
    last_statement(expression)
        && expression.parent().is_some_and(|block| {
            block.kind() == "block"
                && block.parent().is_some_and(|function| {
                    function.kind() == "function_item"
                        && function.child_by_field_name("body") == Some(block)
                })
        })
}

pub(super) fn supports_position(node: Node<'_>, source: &[u8]) -> bool {
    is_returned(node) || is_initializer(node, source) || is_reassignment(node)
        || is_discarded_statement(node) || argument_flow::is_operand(node)
}

pub(super) fn is_discarded_statement(node: Node<'_>) -> bool {
    let statement = node
        .parent()
        .filter(|parent| parent.kind() == "expression_statement")
        .unwrap_or(node);
    statement.parent().is_some_and(|parent| {
        parent.kind() == "block"
            && (has_semicolon(statement) || !last_statement(statement) || super::let_else::is_fallback(parent))
    })
}

fn has_semicolon(node: Node<'_>) -> bool {
    node.children(&mut node.walk())
        .any(|part| part.kind() == ";")
}

fn last_statement(node: Node<'_>) -> bool {
    std::iter::successors(node.next_named_sibling(), Node::next_named_sibling)
        .all(|part| matches!(part.kind(), "line_comment" | "block_comment"))
}

pub(super) fn supports_value_if(node: Node<'_>, source: &[u8]) -> bool {
    let root = conditional::root_branch(node);
    (is_returned(root) || is_initializer(root, source) || is_reassignment(root) || argument_flow::is_operand(root)) && supports_if(node, source)
}

pub(super) fn supports_if(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "if_expression"
        && node
            .child_by_field_name("condition")
            .is_some_and(|test| conditional::supported(test, source))
        && node
            .child_by_field_name("consequence")
            .is_some_and(|arm| match_flow::simple_arm(arm, source))
        && alternative(node).map_or_else(|| argument_flow::is_operand(conditional::root_branch(node)), |arm| {
            if arm.kind() == "if_expression" {
                supports_if(arm, source)
            } else {
                match_flow::simple_arm(arm, source)
            }
        })
}

fn alternative(node: Node<'_>) -> Option<Node<'_>> {
    site::children(node.child_by_field_name("alternative")?).next()
}

impl Lower<'_> {
    pub(super) fn explicit_return(
        &mut self,
        stmt: Node<'_>,
        at: NodeId,
        names: &Bindings,
    ) -> Option<NodeId> {
        let value = site::children(stmt).next();
        if let Some(value) = value.and_then(from_value) {
            return self.lower_branch_value(value, at, names, &Mode::Return);
        }
        let end = self.value(value, at, names, None)?;
        record_evaluation(&mut self.cfg, end, stmt);
        if let Some(value) = value {
            self.return_identity(value, end);
        }
        self.finish(end, line(stmt));
        None
    }

    pub(super) fn lower_branch_value(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        names: &Bindings,
        mode: &Mode<'_>,
    ) -> Option<NodeId> {
        if node.kind() != "if_expression" {
            return self.select_match(node, at, names, Some(mode));
        }
        let test = node.child_by_field_name("condition")?;
        let yes = node.child_by_field_name("consequence")?;
        let no = alternative(node);
        let (passed, failed, mut success_names) =
            self.split_branch(test, at, (line(yes), no.map_or(line(node), line)), names);
        let then_end = self.lower_value_arm(yes, passed, &mut success_names, mode);
        let else_end = match no {
            Some(no) if no.kind() == "if_expression" => self.lower_branch_value(no, failed, names, mode),
            Some(no) => self.lower_value_arm(no, failed, &mut names.clone(), mode),
            None => self.complete_value(None, failed, names, mode),
        };
        self.join_paths(node, [then_end, else_end].into_iter().flatten())
    }

    pub(super) fn lower_value_arm(
        &mut self,
        arm: Node<'_>,
        at: NodeId,
        names: &mut Bindings,
        mode: &Mode<'_>,
    ) -> Option<NodeId> {
        match arm.kind() {
            "return_expression" | "break_expression" | "continue_expression" => self.statement(arm, at, names),
            "assignment_expression" => {
                let end = self.statement(arm, at, names)?;
                self.complete_value(None, end, names, mode)
            }
            "block" => self.lower_value_block(arm, at, names, mode),
            _ => self.complete_value(Some(arm), at, names, mode),
        }
    }

    fn lower_value_block(&mut self, arm: Node<'_>, mut at: NodeId, names: &mut Bindings, mode: &Mode<'_>) -> Option<NodeId> {
        let mut statements = site::children(arm).peekable();
        while let Some(statement) = statements.next() {
            if statements.peek().is_none() && match_flow::simple_value(statement, self.source) {
                return self.complete_value(Some(statement), at, names, mode);
            }
            at = self.statement_in_sequence(statement, at, names, &mut statements)?;
        }
        self.complete_value(None, at, names, mode)
    }

    fn complete_value(
        &mut self,
        value: Option<Node<'_>>,
        at: NodeId,
        names: &Bindings,
        mode: &Mode<'_>,
    ) -> Option<NodeId> {
        match mode {
            Mode::Return => {
                let end = self.evaluate(value, at, names)?;
                if let Some(value) = value {
                    if let Some(evaluation) = self.cfg.nodes[end].evaluations.last_mut() {
                        evaluation.returned = true;
                    }
                    self.return_identity(value, end);
                }
                let exit_line = value.map_or(self.cfg.nodes[end].source_line, line);
                self.finish(end, exit_line);
                None
            }
            Mode::Assign(assignment) => self.assign_arm(value, at, names, assignment),
            Mode::Project => self.evaluate(value, at, names),
            Mode::Operand { syntax, temporary } => {
                self.complete_operand(*syntax, value, temporary, at, names)
            }
        }
    }

    fn complete_operand(&mut self, syntax: Node<'_>, value: Option<Node<'_>>, temporary: &str, at: NodeId, names: &Bindings) -> Option<NodeId> {
        let target = temporary.to_owned();
        let end = if value.is_some() {
            self.value(value, at, names, Some(target))?
        } else {
            self.cfg.emit(at, Stmt::Assign { lhs: target, rhs: RhsShape::Unknown }, line(syntax));
            at
        };
        self.capture_operand(syntax, value, temporary, end);
        Some(end)
    }

}
